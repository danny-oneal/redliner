package main

import (
	"encoding/base64"
	"encoding/json"
	"fmt"
	"math/rand"
	"net/http"
	"net/url"
	"os"
	"regexp"
	"strings"

	vegeta "github.com/tsenart/vegeta/v12/lib"
)

// encoder turns one scenario draw into a concrete vegeta.Target. This is the
// single point where "GraphQL vs REST" exists; everything upstream (mix,

// - Adapter: both adapt (scenario, sampled variables) onto vegeta.Target.
type encoder interface {
	// encode fills t for the given scenario. rng drives pool sampling and
	// must be guarded by the caller (the targeter holds the lock).
	encode(t *vegeta.Target, sc *Scenario, pools Pools, rng *rand.Rand) error
	// operation maps a completed result back to a scenario name for the
	// per-operation report, or "" if unknown.
	operation(res *vegeta.Result) string
}

// baseHeaders builds the static header set shared by every request: content
// negotiation, user-supplied headers, and resolved auth. Computed once —
// vegeta targets can share the map because it is never mutated after start.
func baseHeaders(cfg TargetConfig) (http.Header, error) {
	h := http.Header{}
	h.Set("Accept", "application/json")
	for k, v := range cfg.Headers {
		h.Set(k, v)
	}
	switch cfg.Auth.Type {
	case "", "none":
	case "bearer":
		tok, err := requireEnv(cfg.Auth.TokenEnv)
		if err != nil {
			return nil, err
		}
		h.Set("Authorization", "Bearer "+tok)
	case "header":
		tok, err := requireEnv(cfg.Auth.TokenEnv)
		if err != nil {
			return nil, err
		}
		h.Set(cfg.Auth.Header, tok)
	case "basic":
		user, err := requireEnv(cfg.Auth.UsernameEnv)
		if err != nil {
			return nil, err
		}
		pass, err := requireEnv(cfg.Auth.PasswordEnv)
		if err != nil {
			return nil, err
		}
		cred := base64.StdEncoding.EncodeToString([]byte(user + ":" + pass))
		h.Set("Authorization", "Basic "+cred)
	}
	return h, nil
}

func requireEnv(name string) (string, error) {
	v := os.Getenv(name)
	if v == "" {
		return "", fmt.Errorf("environment variable %s is not set (required by target.auth)", name)
	}
	return v, nil
}

// ---- GraphQL ---------------------------------------------------------------

type graphqlEncoder struct {
	endpoint string
	headers  http.Header
}

func newGraphQLEncoder(cfg TargetConfig) (*graphqlEncoder, error) {
	h, err := baseHeaders(cfg)
	if err != nil {
		return nil, err
	}
	h.Set("Content-Type", "application/json")
	return &graphqlEncoder{endpoint: cfg.URL, headers: h}, nil
}

func (g *graphqlEncoder) encode(t *vegeta.Target, sc *Scenario, pools Pools, rng *rand.Rand) error {
	vars, err := renderValue(sc.Request.Variables, pools, rng)
	if err != nil {
		return fmt.Errorf("scenario %s: %w", sc.Name, err)
	}
	body, err := json.Marshal(map[string]any{
		"operationName": sc.Name,
		"query":         sc.Request.Query,
		"variables":     vars,
	})
	if err != nil {
		return fmt.Errorf("scenario %s: marshal: %w", sc.Name, err)
	}
	t.Method = http.MethodPost
	// The op query param is a correlation tag: it lets the per-operation
	// report (and server access logs) attribute latency to a scenario even
	// though every GraphQL request shares one URL. Servers ignore it.
	t.URL = g.endpoint + "?op=" + url.QueryEscape(sc.Name)
	t.Body = body
	t.Header = g.headers
	return nil
}

func (g *graphqlEncoder) operation(res *vegeta.Result) string {
	if i := strings.Index(res.URL, "?op="); i >= 0 {
		op, _ := url.QueryUnescape(res.URL[i+4:])
		return op
	}
	return ""
}

// ---- REST -------------------------------------------------------------------

type restEncoder struct {
	baseURL   string
	headers   http.Header
	scenarios []restRoute
}

// restRoute precompiles what encode/operation need per scenario: merged
// headers and a regex that matches concrete URLs back to the scenario
// (placeholders become wildcard segments).
type restRoute struct {
	name    string
	method  string
	headers http.Header
	pattern *regexp.Regexp
}

func newRESTEncoder(cfg TargetConfig, scenarios []Scenario) (*restEncoder, error) {
	base, err := baseHeaders(cfg)
	if err != nil {
		return nil, err
	}
	e := &restEncoder{baseURL: strings.TrimRight(cfg.URL, "/"), headers: base}
	for _, sc := range scenarios {
		h := base
		if len(sc.Request.Headers) > 0 {
			h = base.Clone()
			for k, v := range sc.Request.Headers {
				h.Set(k, v)
			}
		}
		if sc.Request.Body != "" && h.Get("Content-Type") == "" {
			h = h.Clone()
			h.Set("Content-Type", "application/json")
		}
		e.scenarios = append(e.scenarios, restRoute{
			name:    sc.Name,
			method:  strings.ToUpper(sc.Request.Method),
			headers: h,
			pattern: pathPattern(e.baseURL, sc.Request.Path),
		})
	}
	return e, nil
}

// pathPattern turns "/products/{{ids}}?full=true" into a regex matching any
// concrete expansion of the template.
func pathPattern(base, path string) *regexp.Regexp {
	var b strings.Builder
	b.WriteString("^")
	rest := base + path
	for {
		i := strings.Index(rest, "{{")
		if i < 0 {
			b.WriteString(regexp.QuoteMeta(rest))
			break
		}
		b.WriteString(regexp.QuoteMeta(rest[:i]))
		b.WriteString(`[^/?&]+`)
		j := strings.Index(rest[i:], "}}")
		if j < 0 {
			break
		}
		rest = rest[i+j+2:]
	}
	b.WriteString("$")
	return regexp.MustCompile(b.String())
}

func (r *restEncoder) route(name string) *restRoute {
	for i := range r.scenarios {
		if r.scenarios[i].name == name {
			return &r.scenarios[i]
		}
	}
	return nil
}

func (r *restEncoder) encode(t *vegeta.Target, sc *Scenario, pools Pools, rng *rand.Rand) error {
	route := r.route(sc.Name)
	path, err := renderString(sc.Request.Path, pools, rng)
	if err != nil {
		return fmt.Errorf("scenario %s: %w", sc.Name, err)
	}
	t.Method = route.method
	t.URL = r.baseURL + path
	t.Header = route.headers
	if sc.Request.Body != "" {
		body, err := renderString(sc.Request.Body, pools, rng)
		if err != nil {
			return fmt.Errorf("scenario %s: %w", sc.Name, err)
		}
		t.Body = []byte(body)
	}
	return nil
}

func (r *restEncoder) operation(res *vegeta.Result) string {
	for i := range r.scenarios {
		sc := &r.scenarios[i]
		if sc.method == res.Method && sc.pattern.MatchString(res.URL) {
			return sc.name
		}
	}
	return ""
}

// newEncoder is the protocol factory (Factory Method pattern).
func newEncoder(cfg *Config) (encoder, error) {
	switch cfg.Target.Protocol {
	case ProtocolGraphQL:
		return newGraphQLEncoder(cfg.Target)
	case ProtocolREST:
		return newRESTEncoder(cfg.Target, cfg.Scenarios)
	default:
		return nil, fmt.Errorf("unsupported protocol %q", cfg.Target.Protocol)
	}
}
