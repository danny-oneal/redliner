package main

import (
	"encoding/xml"
	"fmt"
	"os"
	"time"

	vegeta "github.com/tsenart/vegeta/v12/lib"
)

// sloResult is one evaluated objective.
type sloResult struct {
	Name     string `json:"name"`
	Target   string `json:"target"`
	Observed string `json:"observed"`
	Passed   bool   `json:"passed"`
}

// evaluateSLOs turns the run's metrics into an explicit pass/fail verdict.
// This is what makes the harness CI-grade: a regression fails the pipeline
// instead of relying on a human to eyeball a latency table.
func evaluateSLOs(slos SLOConfig, m *vegeta.Metrics) []sloResult {
	var out []sloResult
	add := func(name, target, observed string, passed bool) {
		out = append(out, sloResult{Name: name, Target: target, Observed: observed, Passed: passed})
	}
	if slos.SuccessRate > 0 {
		add("success_rate",
			fmt.Sprintf(">= %.4f", slos.SuccessRate),
			fmt.Sprintf("%.4f", m.Success),
			m.Success >= slos.SuccessRate)
	}
	lat := func(name string, target Duration, observed time.Duration) {
		if target > 0 {
			add(name,
				"<= "+target.D().String(),
				observed.Round(time.Microsecond).String(),
				observed <= target.D())
		}
	}
	lat("latency_p50", slos.P50, m.Latencies.P50)
	lat("latency_p95", slos.P95, m.Latencies.P95)
	lat("latency_p99", slos.P99, m.Latencies.P99)
	if slos.ThroughputMin > 0 {
		add("throughput",
			fmt.Sprintf(">= %.1f rps", slos.ThroughputMin),
			fmt.Sprintf("%.1f rps", m.Throughput),
			m.Throughput >= slos.ThroughputMin)
	}
	return out
}

func slosPassed(results []sloResult) bool {
	for _, r := range results {
		if !r.Passed {
			return false
		}
	}
	return true
}

// ---- JUnit output ------------------------------------------------------------

// junit shapes follow the de-facto schema every CI system ingests (Jenkins,
// GitLab, Buildkite, GitHub Actions annotations), so SLO verdicts appear as
// test results next to unit tests without extra tooling.
type junitSuite struct {
	XMLName  xml.Name    `xml:"testsuite"`
	Name     string      `xml:"name,attr"`
	Tests    int         `xml:"tests,attr"`
	Failures int         `xml:"failures,attr"`
	Time     float64     `xml:"time,attr"`
	Cases    []junitCase `xml:"testcase"`
}

type junitCase struct {
	Name    string        `xml:"name,attr"`
	Class   string        `xml:"classname,attr"`
	Failure *junitFailure `xml:"failure,omitempty"`
}

type junitFailure struct {
	Message string `xml:"message,attr"`
	Body    string `xml:",chardata"`
}

func writeJUnit(path string, results []sloResult, elapsed time.Duration) error {
	suite := junitSuite{
		Name: "capacity_bench.slos",
		Time: elapsed.Seconds(),
	}
	for _, r := range results {
		c := junitCase{Name: r.Name, Class: "capacity_bench"}
		if !r.Passed {
			c.Failure = &junitFailure{
				Message: fmt.Sprintf("%s: target %s, observed %s", r.Name, r.Target, r.Observed),
			}
			suite.Failures++
		}
		suite.Tests++
		suite.Cases = append(suite.Cases, c)
	}
	data, err := xml.MarshalIndent(suite, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, append([]byte(xml.Header), data...), 0o644)
}
