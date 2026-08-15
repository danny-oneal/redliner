package main

import (
	"strings"
	"testing"
	"time"

	vegeta "github.com/tsenart/vegeta/v12/lib"
)

func boolPtr(b bool) *bool { return &b }

func TestCheckerGraphQLErrors(t *testing.T) {
	c := newChecker(ChecksConfig{GraphQLErrors: boolPtr(true)})

	ok := &vegeta.Result{Code: 200, Body: []byte(`{"data":{"x":1}}`)}
	if c.apply(ok) {
		t.Error("clean 200 should pass")
	}

	bad := &vegeta.Result{Code: 200, Body: []byte(`{"errors":[{"message":"boom"}],"data":null}`)}
	if !c.apply(bad) {
		t.Fatal("200-with-errors should be reclassified")
	}
	if bad.Code != 0 || !strings.Contains(bad.Error, "boom") {
		t.Errorf("reclassified result = code %d, err %q", bad.Code, bad.Error)
	}
}

func TestCheckerStatusAllowlist(t *testing.T) {
	c := newChecker(ChecksConfig{Statuses: []int{200, 201}})
	r := &vegeta.Result{Code: 204}
	if !c.apply(r) || r.Code != 0 {
		t.Error("204 outside allowlist should fail")
	}
	r2 := &vegeta.Result{Code: 201}
	if c.apply(r2) {
		t.Error("201 in allowlist should pass")
	}
}

func TestCheckerPreservesTransportErrors(t *testing.T) {
	c := newChecker(ChecksConfig{Statuses: []int{200}})
	r := &vegeta.Result{Code: 0, Error: "dial tcp: connection refused"}
	c.apply(r)
	if r.Error != "dial tcp: connection refused" {
		t.Errorf("transport error overwritten: %q", r.Error)
	}
}

func TestSLOEvaluation(t *testing.T) {
	m := &vegeta.Metrics{}
	for i := 0; i < 99; i++ {
		m.Add(&vegeta.Result{Code: 200, Latency: 10 * time.Millisecond, Timestamp: time.Now()})
	}
	m.Add(&vegeta.Result{Code: 500, Latency: 900 * time.Millisecond, Timestamp: time.Now()})
	m.Close()

	results := evaluateSLOs(SLOConfig{
		SuccessRate: 0.995,                           // fails: 0.99
		P50:         Duration(50 * time.Millisecond), // passes
	}, m)
	if len(results) != 2 {
		t.Fatalf("want 2 results, got %d", len(results))
	}
	if results[0].Passed {
		t.Error("success_rate 0.99 < 0.995 should fail")
	}
	if !results[1].Passed {
		t.Errorf("p50 should pass: %+v", results[1])
	}
	if slosPassed(results) {
		t.Error("overall verdict should be fail")
	}
}
