package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
)

// bootstrapClient is a small one-shot HTTP client used before the attack to
// harvest live identifiers into pools. It reuses the target's headers/auth
// so harvest requests are indistinguishable from real client traffic.
type bootstrapClient struct {
	cfg     TargetConfig
	client  *http.Client
	headers http.Header
}

func newBootstrapClient(cfg TargetConfig) (*bootstrapClient, error) {
	h, err := baseHeaders(cfg)
	if err != nil {
		return nil, err
	}
	return &bootstrapClient{
		cfg:     cfg,
		client:  &http.Client{Timeout: cfg.Timeout.D()},
		headers: h,
	}, nil
}

// runHarvest executes every configured harvest request and merges the
// extracted values into pools. Pool names shared across harvests append.
func runHarvest(cfg *Config, pools Pools) error {
	if len(cfg.Data.Harvest) == 0 {
		return nil
	}
	bc, err := newBootstrapClient(cfg.Target)
	if err != nil {
		return err
	}
	for i, h := range cfg.Data.Harvest {
		doc, err := bc.fetch(cfg.Target.Protocol, h.Request)
		if err != nil {
			return fmt.Errorf("harvest[%d]: %w", i, err)
		}
		for poolName, path := range h.Extract {
			vals, err := extractPath(doc, path)
			if err != nil {
				return fmt.Errorf("harvest[%d] pool %q: %w", i, poolName, err)
			}
			if len(vals) == 0 {
				return fmt.Errorf("harvest[%d] pool %q: path %q matched nothing — schema mismatch?", i, poolName, path)
			}
			if existing, ok := pools[poolName].(*listPool); ok {
				existing.values = append(existing.values, vals...)
			} else {
				pools[poolName] = &listPool{values: vals}
			}
		}
	}
	return nil
}

func (b *bootstrapClient) fetch(proto Protocol, rc RequestConfig) (any, error) {
	var req *http.Request
	var err error
	switch proto {
	case ProtocolGraphQL:
		body, merr := json.Marshal(map[string]any{
			"query":     rc.Query,
			"variables": rc.Variables,
		})
		if merr != nil {
			return nil, merr
		}
		req, err = http.NewRequest(http.MethodPost, b.cfg.URL, bytes.NewReader(body))
		if req != nil {
			req.Header = b.headers.Clone()
			req.Header.Set("Content-Type", "application/json")
		}
	case ProtocolREST:
		method := rc.Method
		if method == "" {
			method = http.MethodGet
		}
		var rd io.Reader
		if rc.Body != "" {
			rd = strings.NewReader(rc.Body)
		}
		req, err = http.NewRequest(strings.ToUpper(method), strings.TrimRight(b.cfg.URL, "/")+rc.Path, rd)
		if req != nil {
			req.Header = b.headers.Clone()
			if rc.Body != "" {
				req.Header.Set("Content-Type", "application/json")
			}
		}
	default:
		return nil, fmt.Errorf("unsupported protocol %q", proto)
	}
	if err != nil {
		return nil, err
	}

	resp, err := b.client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("%s — is the server up?", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("status %d from %s", resp.StatusCode, req.URL)
	}
	raw, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, err
	}
	var doc any
	if err := json.Unmarshal(raw, &doc); err != nil {
		return nil, fmt.Errorf("decode harvest response: %w", err)
	}
	if proto == ProtocolGraphQL {
		if errsPresent(doc) {
			return nil, fmt.Errorf("harvest query returned GraphQL errors: %s", firstGraphQLError(doc))
		}
	}
	return doc, nil
}
