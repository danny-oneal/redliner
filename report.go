package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"time"

	vegeta "github.com/tsenart/vegeta/v12/lib"
)

// collector accumulates results across phases: overall metrics, per-phase
// metrics (knee-finding with the steps profile), per-operation metrics, the
// latency histogram, and the replayable raw stream.

type collector struct {
	overall  vegeta.Metrics
	perOp    map[string]*vegeta.Metrics
	perPhase map[string]*vegeta.Metrics
	hist     *vegeta.Histogram
	enc      vegeta.Encoder // nil when keep_raw: false
	opOf     func(*vegeta.Result) string
}

func newCollector(opOf func(*vegeta.Result) string, enc vegeta.Encoder) *collector {
	return &collector{
		perOp:    map[string]*vegeta.Metrics{},
		perPhase: map[string]*vegeta.Metrics{},
		hist:     newHistogram(),
		enc:      enc,
		opOf:     opOf,
	}
}

func (c *collector) Add(res *vegeta.Result, phaseName string) {
	if c.enc != nil {
		if err := c.enc.Encode(res); err != nil {
			fmt.Fprintf(os.Stderr, "encode result: %v\n", err)
		}
	}
	c.overall.Add(res)
	c.hist.Add(res)
	if op := c.opOf(res); op != "" {
		m, ok := c.perOp[op]
		if !ok {
			m = &vegeta.Metrics{}
			c.perOp[op] = m
		}
		m.Add(res)
	}
	pm, ok := c.perPhase[phaseName]
	if !ok {
		pm = &vegeta.Metrics{}
		c.perPhase[phaseName] = pm
	}
	pm.Add(res)
}

func (c *collector) Close() {
	c.overall.Close()
	for _, m := range c.perOp {
		m.Close()
	}
	for _, m := range c.perPhase {
		m.Close()
	}
}

// newHistogram returns a latency histogram with log-spaced buckets spanning
// sub-ms point lookups up to multi-second responses under distress.
func newHistogram() *vegeta.Histogram {
	return &vegeta.Histogram{Buckets: vegeta.Buckets{
		0,
		500 * time.Microsecond,
		1 * time.Millisecond,
		2 * time.Millisecond,
		5 * time.Millisecond,
		10 * time.Millisecond,
		25 * time.Millisecond,
		50 * time.Millisecond,
		100 * time.Millisecond,
		250 * time.Millisecond,
		500 * time.Millisecond,
		1 * time.Second,
		2 * time.Second,
		5 * time.Second,
	}}
}

// runManifest captures everything needed to reproduce or audit a run. It is
// written before the attack starts (so aborted runs still leave a trail) and
// finalized after with observed numbers.
type runManifest struct {
	StartedAt  time.Time   `json:"started_at"`
	FinishedAt time.Time   `json:"finished_at,omitempty"`
	ConfigFile string      `json:"config_file"`
	Config     *Config     `json:"config"`
	Seed       int64       `json:"seed"`
	Requests   uint64      `json:"requests,omitempty"`
	SLOResults []sloResult `json:"slo_results,omitempty"`
	Passed     *bool       `json:"passed,omitempty"`
}

// makeReportDir creates reports/<UTC timestamp>/ and refreshes a `latest`
// symlink, so historical runs are never overwritten and dashboards can
// always point at the newest one.
func makeReportDir(base string, start time.Time) (string, error) {
	dir := filepath.Join(base, start.UTC().Format("20060102T150405Z"))
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return "", err
	}
	link := filepath.Join(base, "latest")
	_ = os.Remove(link)
	_ = os.Symlink(filepath.Base(dir), link) // best-effort (Windows, perms)
	return dir, nil
}

func writeReports(dir string, c *collector, manifest *runManifest) error {
	if err := writeWith(filepath.Join(dir, "text.txt"), func(f *os.File) error {
		return vegeta.NewTextReporter(&c.overall).Report(f)
	}); err != nil {
		return err
	}
	if err := writeWith(filepath.Join(dir, "histogram.txt"), func(f *os.File) error {
		return vegeta.NewHistogramReporter(c.hist).Report(f)
	}); err != nil {
		return err
	}
	if err := writeWith(filepath.Join(dir, "metrics.json"), func(f *os.File) error {
		return vegeta.NewJSONReporter(&c.overall).Report(f)
	}); err != nil {
		return err
	}
	if len(c.perOp) > 0 {
		if err := writeJSON(filepath.Join(dir, "per_operation.json"), summarize(c.perOp)); err != nil {
			return err
		}
	}
	if len(c.perPhase) > 1 {
		if err := writeJSON(filepath.Join(dir, "per_phase.json"), summarize(c.perPhase)); err != nil {
			return err
		}
	}
	return writeJSON(filepath.Join(dir, "run_manifest.json"), manifest)
}

// opSummary is the compact per-operation/per-phase row teams actually read;
// full vegeta metrics stay in metrics.json.
type opSummary struct {
	Name     string  `json:"name"`
	Requests uint64  `json:"requests"`
	Success  float64 `json:"success"`
	P50      string  `json:"p50"`
	P95      string  `json:"p95"`
	P99      string  `json:"p99"`
	Max      string  `json:"max"`
}

func summarize(ms map[string]*vegeta.Metrics) []opSummary {
	names := make([]string, 0, len(ms))
	for n := range ms {
		names = append(names, n)
	}
	sort.Strings(names)
	out := make([]opSummary, 0, len(names))
	for _, n := range names {
		m := ms[n]
		out = append(out, opSummary{
			Name:     n,
			Requests: m.Requests,
			Success:  m.Success,
			P50:      m.Latencies.P50.Round(time.Microsecond).String(),
			P95:      m.Latencies.P95.Round(time.Microsecond).String(),
			P99:      m.Latencies.P99.Round(time.Microsecond).String(),
			Max:      m.Latencies.Max.Round(time.Microsecond).String(),
		})
	}
	return out
}

func writeJSON(path string, v any) error {
	data, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, append(data, '\n'), 0o644)
}

func writeWith(path string, fn func(*os.File) error) error {
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	defer f.Close()
	return fn(f)
}
