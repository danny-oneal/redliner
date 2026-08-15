package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func writeTemp(t *testing.T, content string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "cfg.yaml")
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	return path
}

const minimalGraphQL = `
version: 1
target:
  url: http://localhost:8080/query
scenarios:
  - name: Q
    weight: 1
    request:
      query: "query { ping }"
`

func TestLoadConfigDefaults(t *testing.T) {
	cfg, err := loadConfigFile(writeTemp(t, minimalGraphQL))
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Target.Protocol != ProtocolGraphQL {
		t.Errorf("default protocol = %q, want graphql", cfg.Target.Protocol)
	}
	if cfg.Load.Profile != "poisson" || cfg.Load.Rate != 200 {
		t.Errorf("default load = %q/%v, want poisson/200", cfg.Load.Profile, cfg.Load.Rate)
	}
	if cfg.Target.Timeout.D() != 10*time.Second {
		t.Errorf("default timeout = %s", cfg.Target.Timeout.D())
	}
	if cfg.Checks.GraphQLErrors == nil || !*cfg.Checks.GraphQLErrors {
		t.Error("graphql_errors should default on for graphql protocol")
	}
	if cfg.Load.Seed == 0 {
		t.Error("seed should default to a non-zero time-based value")
	}
}

func TestLoadConfigRESTValidation(t *testing.T) {
	// A REST config using a GraphQL field must fail fast.
	bad := `
version: 1
target:
  url: http://localhost:8081
  protocol: rest
scenarios:
  - name: Wrong
    weight: 1
    request:
      query: "query { nope }"
`
	_, err := loadConfigFile(writeTemp(t, bad))
	if err == nil || !strings.Contains(err.Error(), "request.path is required") {
		t.Errorf("want path-required error, got %v", err)
	}
}

func TestLoadConfigRejectsUnknownKeys(t *testing.T) {
	bad := strings.Replace(minimalGraphQL, "version: 1", "version: 1\nratee: 5", 1)
	if _, err := loadConfigFile(writeTemp(t, bad)); err == nil {
		t.Error("unknown top-level key should be rejected (KnownFields)")
	}
}

func TestLoadConfigInvalidProfile(t *testing.T) {
	bad := minimalGraphQL + `
load:
  profile: sawtooth
`
	_, err := loadConfigFile(writeTemp(t, bad))
	if err == nil || !strings.Contains(err.Error(), "profile") {
		t.Errorf("want profile error, got %v", err)
	}
}

func TestQueryFileResolution(t *testing.T) {
	dir := t.TempDir()
	if err := os.MkdirAll(filepath.Join(dir, "queries"), 0o755); err != nil {
		t.Fatal(err)
	}
	q := "query Deep($id:Int!){ a { b { c(id:$id) { d } } } }"
	if err := os.WriteFile(filepath.Join(dir, "queries", "deep.graphql"), []byte(q+"\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	cfgYAML := `
version: 1
target:
  url: http://localhost:8080/query
scenarios:
  - name: Deep
    weight: 1
    request:
      query_file: queries/deep.graphql   # relative to the config file
`
	path := filepath.Join(dir, "cfg.yaml")
	if err := os.WriteFile(path, []byte(cfgYAML), 0o644); err != nil {
		t.Fatal(err)
	}
	cfg, err := loadConfigFile(path)
	if err != nil {
		t.Fatal(err)
	}
	if cfg.Scenarios[0].Request.Query != q {
		t.Errorf("query not loaded from file:\n%q", cfg.Scenarios[0].Request.Query)
	}
}

func TestQueryFileMutuallyExclusiveWithQuery(t *testing.T) {
	bad := `
version: 1
target:
  url: http://localhost:8080/query
scenarios:
  - name: Q
    weight: 1
    request:
      query: "query { ping }"
      query_file: also.graphql
`
	_, err := loadConfigFile(writeTemp(t, bad))
	if err == nil || !strings.Contains(err.Error(), "mutually exclusive") {
		t.Errorf("want mutual-exclusion error, got %v", err)
	}
}

func TestQueryFileMissing(t *testing.T) {
	bad := `
version: 1
target:
  url: http://localhost:8080/query
scenarios:
  - name: Q
    weight: 1
    request:
      query_file: nope.graphql
`
	_, err := loadConfigFile(writeTemp(t, bad))
	if err == nil || !strings.Contains(err.Error(), "query_file") {
		t.Errorf("want missing-file error, got %v", err)
	}
}

func TestQueryFileRejectedForREST(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "q.graphql"), []byte("query { x }"), 0o644); err != nil {
		t.Fatal(err)
	}
	bad := `
version: 1
target:
  url: http://localhost:8081
  protocol: rest
scenarios:
  - name: Q
    weight: 1
    request:
      path: /x
      query_file: q.graphql
`
	path := filepath.Join(dir, "cfg.yaml")
	if err := os.WriteFile(path, []byte(bad), 0o644); err != nil {
		t.Fatal(err)
	}
	_, err := loadConfigFile(path)
	if err == nil || !strings.Contains(err.Error(), "GraphQL fields") {
		t.Errorf("want protocol-mismatch error, got %v", err)
	}
}

func TestStepsProfileBuildsPhases(t *testing.T) {
	cfgS := minimalGraphQL + `
load:
  profile: steps
  steps:
    - { rate: 10, duration: 1s }
    - { rate: 20, duration: 2s }
`
	cfg, err := loadConfigFile(writeTemp(t, cfgS))
	if err != nil {
		t.Fatal(err)
	}
	phases, err := buildPhases(cfg.Load)
	if err != nil {
		t.Fatal(err)
	}
	if len(phases) != 2 || phases[1].rate != 20 || phases[1].duration != 2*time.Second {
		t.Errorf("unexpected phases: %+v", phases)
	}
}
