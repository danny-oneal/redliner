package main

import (
	"encoding/json"
	"math/rand"
	"strings"
	"testing"

	vegeta "github.com/tsenart/vegeta/v12/lib"
)

func TestWeightedMixDistribution(t *testing.T) {
	mix := newWeightedMix([]Scenario{
		{Name: "A", Weight: 80},
		{Name: "B", Weight: 20},
	})
	rng := rand.New(rand.NewSource(42))
	counts := map[string]int{}
	const n = 20000
	for i := 0; i < n; i++ {
		counts[mix.pick(rng).Name]++
	}
	got := float64(counts["A"]) / n
	if got < 0.77 || got > 0.83 {
		t.Errorf("A frequency = %.3f, want ≈0.80", got)
	}
}

func TestGraphQLEncoderBody(t *testing.T) {
	enc, err := newGraphQLEncoder(TargetConfig{URL: "http://h/query"})
	if err != nil {
		t.Fatal(err)
	}
	sc := &Scenario{Name: "Op", Request: RequestConfig{
		Query:     "query($id:Int!){ x(id:$id) }",
		Variables: map[string]any{"id": "{{ids}}"},
	}}
	var tgt vegeta.Target
	rng := rand.New(rand.NewSource(1))
	if err := enc.encode(&tgt, sc, testPools(), rng); err != nil {
		t.Fatal(err)
	}
	if tgt.Method != "POST" || !strings.HasPrefix(tgt.URL, "http://h/query?op=Op") {
		t.Errorf("unexpected target: %s %s", tgt.Method, tgt.URL)
	}
	var body struct {
		OperationName string         `json:"operationName"`
		Variables     map[string]any `json:"variables"`
	}
	if err := json.Unmarshal(tgt.Body, &body); err != nil {
		t.Fatal(err)
	}
	if body.OperationName != "Op" {
		t.Errorf("operationName = %q", body.OperationName)
	}
	if _, ok := body.Variables["id"].(float64); !ok { // json numbers decode as float64
		t.Errorf("id variable should be numeric, got %T", body.Variables["id"])
	}
	// Round-trip: the encoder must attribute the result back to the scenario.
	res := &vegeta.Result{URL: tgt.URL, Method: tgt.Method}
	if op := enc.operation(res); op != "Op" {
		t.Errorf("operation() = %q, want Op", op)
	}
}

func TestRESTEncoderTargetAndAttribution(t *testing.T) {
	scenarios := []Scenario{
		{Name: "Get", Weight: 1, Request: RequestConfig{Method: "GET", Path: "/products/{{ids}}"}},
		{Name: "Create", Weight: 1, Request: RequestConfig{
			Method: "POST", Path: "/orders",
			Body: `{"id": {{ids}}}`,
		}},
	}
	enc, err := newRESTEncoder(TargetConfig{URL: "http://h/"}, scenarios)
	if err != nil {
		t.Fatal(err)
	}
	rng := rand.New(rand.NewSource(1))

	var tgt vegeta.Target
	if err := enc.encode(&tgt, &scenarios[0], testPools(), rng); err != nil {
		t.Fatal(err)
	}
	if tgt.Method != "GET" || !strings.HasPrefix(tgt.URL, "http://h/products/") {
		t.Errorf("unexpected target: %s %s", tgt.Method, tgt.URL)
	}
	if op := enc.operation(&vegeta.Result{Method: "GET", URL: "http://h/products/17"}); op != "Get" {
		t.Errorf("operation() = %q, want Get", op)
	}

	var post vegeta.Target
	if err := enc.encode(&post, &scenarios[1], testPools(), rng); err != nil {
		t.Fatal(err)
	}
	if post.Header.Get("Content-Type") != "application/json" {
		t.Error("body-bearing scenario should default Content-Type to JSON")
	}
	if !strings.Contains(string(post.Body), `"id": `) {
		t.Errorf("body not rendered: %s", post.Body)
	}
	if op := enc.operation(&vegeta.Result{Method: "POST", URL: "http://h/orders"}); op != "Create" {
		t.Errorf("operation() = %q, want Create", op)
	}
}

func TestBearerAuthHeader(t *testing.T) {
	t.Setenv("BENCH_TOKEN_TEST", "sekret")
	h, err := baseHeaders(TargetConfig{Auth: AuthConfig{Type: "bearer", TokenEnv: "BENCH_TOKEN_TEST"}})
	if err != nil {
		t.Fatal(err)
	}
	if h.Get("Authorization") != "Bearer sekret" {
		t.Errorf("Authorization = %q", h.Get("Authorization"))
	}
	if _, err := baseHeaders(TargetConfig{Auth: AuthConfig{Type: "bearer", TokenEnv: "BENCH_MISSING_VAR"}}); err == nil {
		t.Error("missing token env should fail fast")
	}
}
