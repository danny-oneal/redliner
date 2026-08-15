package main

import (
	"bytes"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"go.yaml.in/yaml/v3"
)

// Protocol selects the request encoder (Factory Method — see protocol.go).
// Everything upstream of the encoder is protocol-agnostic, so switching is
// one config line.
type Protocol string

const (
	ProtocolGraphQL Protocol = "graphql"
	ProtocolREST    Protocol = "rest"
)

// Config is the whole scenario file. Decoding uses KnownFields(true), so
// every key a config may carry must appear here.
type Config struct {
	Version   int          `yaml:"version"`
	Target    TargetConfig `yaml:"target"`
	Load      LoadConfig   `yaml:"load"`
	Data      DataConfig   `yaml:"data"`
	Scenarios []Scenario   `yaml:"scenarios"`
	Checks    ChecksConfig `yaml:"checks"`
	SLOs      SLOConfig    `yaml:"slos"`
	Report    ReportConfig `yaml:"report"`
}

// TargetConfig describes the service under test and the transport settings
// shared by warmup, harvest, and every attack phase.
type TargetConfig struct {
	// URL is the full endpoint for graphql, or the base URL that scenario
	// paths append to for rest.
	URL      string   `yaml:"url"`
	Protocol Protocol `yaml:"protocol"`
	Timeout  Duration `yaml:"timeout"`
	// Headers apply to every request, including harvest.
	Headers          map[string]string `yaml:"headers"`
	Auth             AuthConfig        `yaml:"auth"`
	KeepAlive        *bool             `yaml:"keep_alive"` // default true
	MaxIdleConns     int               `yaml:"max_idle_conns"`
	ReadBufferBytes  int               `yaml:"read_buffer_bytes"`
	WriteBufferBytes int               `yaml:"write_buffer_bytes"`
	HTTP2            bool              `yaml:"http2"`
}

// AuthConfig resolves credentials from the environment, never from the
// config file, so secrets stay out of version control.
type AuthConfig struct {
	Type        string `yaml:"type"` // none | bearer | basic | header
	TokenEnv    string `yaml:"token_env"`
	Header      string `yaml:"header"` // header name for type: header
	UsernameEnv string `yaml:"username_env"`
	PasswordEnv string `yaml:"password_env"`
}

type LoadConfig struct {
	// Profile selects the pacing strategy (Strategy pattern — see pacer.go):
	// constant | poisson | ramp | steps
	Profile    string     `yaml:"profile"`
	Rate       float64    `yaml:"rate"`     // constant/poisson mean RPS
	Duration   Duration   `yaml:"duration"` // constant/poisson/ramp
	Warmup     Duration   `yaml:"warmup"`   // uniform pre-run at 10% rate; 0 disables
	Ramp       RampConfig `yaml:"ramp"`
	Steps      []Step     `yaml:"steps"`
	Workers    uint64     `yaml:"workers"`
	MaxWorkers uint64     `yaml:"max_workers"`
	Seed       int64      `yaml:"seed"` // 0 = derived from wall clock
}

type RampConfig struct {
	From float64 `yaml:"from"`
	To   float64 `yaml:"to"`
}

type Step struct {
	Rate     float64  `yaml:"rate"`
	Duration Duration `yaml:"duration"`
}

type DataConfig struct {
	Pools   map[string]PoolConfig `yaml:"pools"`
	Harvest []HarvestConfig       `yaml:"harvest"`
}

type PoolConfig struct {
	Type   string `yaml:"type"` // list | file | range
	Values []any  `yaml:"values"`
	Path   string `yaml:"path"`
	Min    int    `yaml:"min"`
	Max    int    `yaml:"max"`
	As     string `yaml:"as"` // for file pools: string (default) | int
}

// HarvestConfig describes one bootstrap request whose JSON response seeds
// pools with live identifiers, so every attack request resolves to real
// data and report errors are genuine server errors, not synthetic 404s.
type HarvestConfig struct {
	Request RequestConfig     `yaml:"request"`
	Extract map[string]string `yaml:"extract"` // pool name -> JSON path (a.b[].c)
}

type Scenario struct {
	Name    string        `yaml:"name"`
	Weight  float64       `yaml:"weight"`
	Request RequestConfig `yaml:"request"`
}

// RequestConfig is a tagged union across the two protocols; validation
// enforces the fields required by the active one.
type RequestConfig struct {
	// GraphQL. Exactly one of Query (inline, fine for short operations) or
	// QueryFile (external .graphql document — syntax highlighting, linting,
	// and reuse of the exact operations clients ship) may be set. QueryFile
	// paths resolve relative to the config file, and the file's content is
	// loaded into Query before validation.
	Query     string         `yaml:"query"`
	QueryFile string         `yaml:"query_file"`
	Variables map[string]any `yaml:"variables"`
	// REST
	Method  string            `yaml:"method"`
	Path    string            `yaml:"path"`
	Body    string            `yaml:"body"`
	Headers map[string]string `yaml:"headers"`
}

type ChecksConfig struct {
	// Statuses lists acceptable HTTP status codes; empty means 2xx/3xx.
	Statuses []int `yaml:"status"`
	// GraphQLErrors treats a 200 response whose body carries a top-level
	// "errors" array as a failure. GraphQL servers return 200 for resolver
	// errors, so without this check the success rate lies. Defaults on for
	// protocol: graphql.
	GraphQLErrors *bool `yaml:"graphql_errors"`
}

// SLOConfig is the CI gate: any violated objective fails the run with a
// non-zero exit code so pipelines can block regressions automatically.
// Zero values mean "not enforced".
type SLOConfig struct {
	SuccessRate   float64  `yaml:"success_rate"` // e.g. 0.995
	P50           Duration `yaml:"p50"`
	P95           Duration `yaml:"p95"`
	P99           Duration `yaml:"p99"`
	ThroughputMin float64  `yaml:"throughput_min"` // successful RPS
}

func (s SLOConfig) enforced() bool {
	return s.SuccessRate > 0 || s.P50 > 0 || s.P95 > 0 || s.P99 > 0 || s.ThroughputMin > 0
}

type ReportConfig struct {
	Dir          string `yaml:"dir"`
	JUnit        bool   `yaml:"junit"`
	KeepRaw      *bool  `yaml:"keep_raw"`      // results.bin; default true
	PerOperation *bool  `yaml:"per_operation"` // per-scenario latency table; default true
}

// Duration wraps time.Duration with YAML support ("250ms", "2m").
type Duration time.Duration

func (d *Duration) UnmarshalYAML(node *yaml.Node) error {
	var s string
	if err := node.Decode(&s); err != nil {
		return err
	}
	v, err := time.ParseDuration(s)
	if err != nil {
		return fmt.Errorf("invalid duration %q: %w", s, err)
	}
	*d = Duration(v)
	return nil
}

func (d Duration) MarshalYAML() (any, error) { return time.Duration(d).String(), nil }
func (d Duration) D() time.Duration          { return time.Duration(d) }

// loadConfigFile reads, defaults, and validates a scenario file. Fail-fast:
// every problem is reported before a single request is fired.
func loadConfigFile(path string) (*Config, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	cfg := &Config{}
	dec := yaml.NewDecoder(bytes.NewReader(raw))
	dec.KnownFields(true) // typo'd keys are config bugs, not silence
	if err := dec.Decode(cfg); err != nil {
		return nil, fmt.Errorf("parse %s: %w", path, err)
	}
	cfg.applyDefaults()
	if err := cfg.resolveFiles(filepath.Dir(path)); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	if err := cfg.validate(); err != nil {
		return nil, fmt.Errorf("%s: %w", path, err)
	}
	return cfg, nil
}

// resolveFiles loads external file references before validation. All paths
// resolve relative to the config file's directory, not the process CWD, so
// configs behave identically from CI, the repo root, or `make run`.
func (c *Config) resolveFiles(baseDir string) error {
	for i := range c.Scenarios {
		if err := resolveQueryFile(&c.Scenarios[i].Request, baseDir); err != nil {
			return fmt.Errorf("scenario %q: %w", c.Scenarios[i].Name, err)
		}
	}
	for i := range c.Data.Harvest {
		if err := resolveQueryFile(&c.Data.Harvest[i].Request, baseDir); err != nil {
			return fmt.Errorf("data.harvest[%d]: %w", i, err)
		}
	}
	for name, p := range c.Data.Pools {
		if p.Type == "file" && p.Path != "" && !filepath.IsAbs(p.Path) {
			p.Path = filepath.Join(baseDir, p.Path)
			c.Data.Pools[name] = p
		}
	}
	return nil
}

func resolveQueryFile(rc *RequestConfig, baseDir string) error {
	if rc.QueryFile == "" {
		return nil
	}
	if rc.Query != "" {
		return fmt.Errorf("query and query_file are mutually exclusive")
	}
	p := rc.QueryFile
	if !filepath.IsAbs(p) {
		p = filepath.Join(baseDir, p)
	}
	raw, err := os.ReadFile(p)
	if err != nil {
		return fmt.Errorf("query_file: %w", err)
	}
	q := strings.TrimSpace(string(raw))
	if q == "" {
		return fmt.Errorf("query_file %s is empty", rc.QueryFile)
	}
	rc.Query = q
	return nil
}

func (c *Config) applyDefaults() {
	if c.Target.Protocol == "" {
		c.Target.Protocol = ProtocolGraphQL
	}
	if c.Target.Timeout == 0 {
		c.Target.Timeout = Duration(10 * time.Second)
	}
	if c.Target.KeepAlive == nil {
		t := true
		c.Target.KeepAlive = &t
	}
	if c.Target.MaxIdleConns == 0 {
		c.Target.MaxIdleConns = 10000
	}
	if c.Target.ReadBufferBytes == 0 {
		c.Target.ReadBufferBytes = 4096
	}
	if c.Target.WriteBufferBytes == 0 {
		c.Target.WriteBufferBytes = 4096
	}
	if c.Load.Profile == "" {
		c.Load.Profile = "poisson"
	}
	if c.Load.Rate == 0 && len(c.Load.Steps) == 0 && c.Load.Ramp.To == 0 {
		c.Load.Rate = 200
	}
	if c.Load.Duration == 0 && len(c.Load.Steps) == 0 {
		c.Load.Duration = Duration(30 * time.Second)
	}
	if c.Load.Workers == 0 {
		c.Load.Workers = 10
	}
	if c.Load.MaxWorkers == 0 {
		c.Load.MaxWorkers = 100_000
	}
	if c.Load.Seed == 0 {
		c.Load.Seed = time.Now().UnixNano()
	}
	if c.Checks.GraphQLErrors == nil {
		on := c.Target.Protocol == ProtocolGraphQL
		c.Checks.GraphQLErrors = &on
	}
	if c.Report.Dir == "" {
		c.Report.Dir = "reports"
	}
	if c.Report.KeepRaw == nil {
		t := true
		c.Report.KeepRaw = &t
	}
	if c.Report.PerOperation == nil {
		t := true
		c.Report.PerOperation = &t
	}
	for i := range c.Scenarios {
		if c.Scenarios[i].Request.Method == "" {
			c.Scenarios[i].Request.Method = "GET"
		}
	}
}

func (c *Config) validate() error {
	var errs []string
	fail := func(format string, args ...any) { errs = append(errs, fmt.Sprintf(format, args...)) }

	if c.Target.URL == "" {
		fail("target.url is required")
	}
	if c.Target.Protocol != ProtocolGraphQL && c.Target.Protocol != ProtocolREST {
		fail("target.protocol must be graphql or rest, got %q", c.Target.Protocol)
	}
	switch c.Target.Auth.Type {
	case "", "none":
	case "bearer", "header":
		if c.Target.Auth.TokenEnv == "" {
			fail("target.auth.token_env is required for auth type %q", c.Target.Auth.Type)
		}
		if c.Target.Auth.Type == "header" && c.Target.Auth.Header == "" {
			fail("target.auth.header is required for auth type header")
		}
	case "basic":
		if c.Target.Auth.UsernameEnv == "" || c.Target.Auth.PasswordEnv == "" {
			fail("target.auth.username_env and password_env are required for basic auth")
		}
	default:
		fail("target.auth.type %q not supported (none|bearer|basic|header)", c.Target.Auth.Type)
	}

	switch c.Load.Profile {
	case "constant", "poisson":
		if c.Load.Rate <= 0 {
			fail("load.rate must be > 0 for profile %q", c.Load.Profile)
		}
	case "ramp":
		if c.Load.Ramp.From < 0 || c.Load.Ramp.To <= 0 {
			fail("load.ramp.from/to must be set for profile ramp")
		}
	case "steps":
		if len(c.Load.Steps) == 0 {
			fail("load.steps must be non-empty for profile steps")
		}
		for i, s := range c.Load.Steps {
			if s.Rate <= 0 || s.Duration <= 0 {
				fail("load.steps[%d]: rate and duration must be > 0", i)
			}
		}
	default:
		fail("load.profile %q not supported (constant|poisson|ramp|steps)", c.Load.Profile)
	}

	if len(c.Scenarios) == 0 {
		fail("at least one scenario is required")
	}
	seen := map[string]bool{}
	for i, s := range c.Scenarios {
		if s.Name == "" {
			fail("scenarios[%d]: name is required", i)
			continue
		}
		if seen[s.Name] {
			fail("scenarios[%d]: duplicate name %q", i, s.Name)
		}
		seen[s.Name] = true
		if s.Weight <= 0 {
			fail("scenario %q: weight must be > 0", s.Name)
		}
		switch c.Target.Protocol {
		case ProtocolGraphQL:
			if s.Request.Query == "" {
				fail("scenario %q: request.query or request.query_file is required for graphql", s.Name)
			}
			if s.Request.Path != "" {
				fail("scenario %q: request.path is a REST field; protocol is graphql", s.Name)
			}
		case ProtocolREST:
			if s.Request.Path == "" {
				fail("scenario %q: request.path is required for rest", s.Name)
			}
			if s.Request.Query != "" || s.Request.QueryFile != "" {
				fail("scenario %q: request.query/query_file are GraphQL fields; protocol is rest", s.Name)
			}
		}
	}

	for name, p := range c.Data.Pools {
		switch p.Type {
		case "list":
			if len(p.Values) == 0 {
				fail("pool %q: list pool needs values", name)
			}
		case "file":
			if p.Path == "" {
				fail("pool %q: file pool needs path", name)
			}
		case "range":
			if p.Max < p.Min {
				fail("pool %q: range max < min", name)
			}
		default:
			fail("pool %q: type %q not supported (list|file|range)", name, p.Type)
		}
	}
	for i, h := range c.Data.Harvest {
		if len(h.Extract) == 0 {
			fail("data.harvest[%d]: extract map is required", i)
		}
		if c.Target.Protocol == ProtocolGraphQL && h.Request.Query == "" {
			fail("data.harvest[%d]: request.query or request.query_file is required for graphql", i)
		}
		if c.Target.Protocol == ProtocolREST && h.Request.Path == "" {
			fail("data.harvest[%d]: request.path is required for rest", i)
		}
	}

	if len(errs) > 0 {
		return fmt.Errorf("invalid config:\n  - %s", strings.Join(errs, "\n"))
	}
	return nil
}

// applyOverrides lets CLI flags win over the file for the handful of knobs
// that vary per environment or pipeline stage.
func (c *Config) applyOverrides(fs *flag.FlagSet, url string, rate float64, duration, warmup time.Duration, seed int64, out string) {
	set := map[string]bool{}
	fs.Visit(func(f *flag.Flag) { set[f.Name] = true })
	if set["url"] {
		c.Target.URL = url
	}
	if set["rate"] {
		c.Load.Rate = rate
	}
	if set["duration"] {
		c.Load.Duration = Duration(duration)
	}
	if set["warmup"] {
		c.Load.Warmup = Duration(warmup)
	}
	if set["seed"] {
		c.Load.Seed = seed
	}
	if set["out"] {
		c.Report.Dir = out
	}
}

func joinLines(lines []string) string {
	out := ""
	for i, l := range lines {
		if i > 0 {
			out += "\n  - "
		}
		out += l
	}
	return out
}
