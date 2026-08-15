// capacity_bench is a vegeta-powered capacity/stress harness driven by a
// declarative YAML scenario file. It speaks GraphQL or REST, fires a
// weighted mix of operations with variables sampled from data pools
// (static or harvested from the live service), paces load as constant,
// Poisson, ramp, or stepped profiles, validates responses at the
// application layer, and gates CI on SLOs with a non-zero exit code.
//
// Usage:
//
//	capacity_bench -config configs/graphql-catalog.yaml
//	capacity_bench -config configs/rest-example.yaml -rate 500 -duration 2m
//
// Exit codes: 0 = run passed, 1 = SLO violation, 2 = configuration or
// runtime error. Reports land in <report.dir>/<timestamp>/ with a `latest`
// symlink.
package main

import (
	"flag"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"time"

	vegeta "github.com/tsenart/vegeta/v12/lib"
)

const (
	exitOK        = 0
	exitSLOFailed = 1
	exitError     = 2
)

func main() {
	os.Exit(run())
}

func run() int {
	fs := flag.NewFlagSet("capacity_bench", flag.ExitOnError)
	var (
		configPath = fs.String("config", "capacity_bench.yaml", "scenario config file (YAML)")
		urlFlag    = fs.String("url", "", "override target.url")
		rate       = fs.Float64("rate", 0, "override load.rate (RPS)")
		duration   = fs.Duration("duration", 0, "override load.duration")
		warmup     = fs.Duration("warmup", 0, "override load.warmup")
		seed       = fs.Int64("seed", 0, "override load.seed (0 keeps config/time-based)")
		out        = fs.String("out", "", "override report.dir")
		validate   = fs.Bool("validate", false, "validate config and exit")
	)
	fs.Parse(os.Args[1:])

	cfg, err := loadConfigFile(*configPath)
	if err != nil {
		log.Printf("config: %v", err)
		return exitError
	}
	cfg.applyOverrides(fs, *urlFlag, *rate, *duration, *warmup, *seed, *out)
	if *validate {
		fmt.Printf("%s: valid (%s, %d scenarios, profile %s)\n",
			*configPath, cfg.Target.Protocol, len(cfg.Scenarios), cfg.Load.Profile)
		return exitOK
	}

	log.Printf("target=%s protocol=%s profile=%s seed=%d",
		cfg.Target.URL, cfg.Target.Protocol, cfg.Load.Profile, cfg.Load.Seed)

	// ---- Data pools: static first, then live harvest -----------------------
	pools, err := buildPools(cfg.Data)
	if err != nil {
		log.Printf("pools: %v", err)
		return exitError
	}
	if err := runHarvest(cfg, pools); err != nil {
		log.Printf("harvest: %v", err)
		return exitError
	}
	for name := range pools {
		if lp, ok := pools[name].(*listPool); ok {
			log.Printf("pool %-20s %d values", name, len(lp.values))
		}
	}

	// ---- Protocol encoder + targeter ---------------------------------------
	enc, err := newEncoder(cfg)
	if err != nil {
		log.Printf("encoder: %v", err)
		return exitError
	}
	targeter := NewTargeter(enc, newWeightedMix(cfg.Scenarios), pools, cfg.Load.Seed)

	phases, err := buildPhases(cfg.Load)
	if err != nil {
		log.Printf("load: %v", err)
		return exitError
	}

	// ---- Report scaffolding -------------------------------------------------
	start := time.Now()
	reportDir, err := makeReportDir(cfg.Report.Dir, start)
	if err != nil {
		log.Printf("report dir: %v", err)
		return exitError
	}
	manifest := &runManifest{
		StartedAt:  start,
		ConfigFile: *configPath,
		Config:     cfg,
		Seed:       cfg.Load.Seed,
	}
	_ = writeJSON(filepath.Join(reportDir, "run_manifest.json"), manifest)

	var rawEnc vegeta.Encoder
	if *cfg.Report.KeepRaw {
		binFile, err := os.Create(filepath.Join(reportDir, "results.bin"))
		if err != nil {
			log.Printf("create results.bin: %v", err)
			return exitError
		}
		defer binFile.Close()
		rawEnc = vegeta.NewEncoder(binFile)
	}
	check := newChecker(cfg.Checks)
	coll := newCollector(enc.operation, rawEnc)

	// ---- Warmup: uniform, unmeasured, 10% of the first phase's rate --------
	if cfg.Load.Warmup > 0 {
		warmRate := vegeta.Rate{Freq: max(1, int(phases[0].rate/10)), Per: time.Second}
		log.Printf("warmup: %s @ %d rps (uniform, not measured)", cfg.Load.Warmup.D(), warmRate.Freq)
		warmAttacker := newAttacker(cfg)
		for range warmAttacker.Attack(targeter, warmRate, cfg.Load.Warmup.D(), "warmup") {
			// drain
		}
		warmAttacker.Stop()
	}

	// ---- Measured phases -----------------------------------------------------
	// One fresh attacker per phase: reusing an attacker across Attack() calls
	// can leave its worker pool in a state where the next attack fires nothing.
	for _, ph := range phases {
		log.Printf("attack: %s for %s (nominal %g rps)", ph.name, ph.duration, ph.rate)
		attacker := newAttacker(cfg)
		for res := range attacker.Attack(targeter, ph.pacer, ph.duration, ph.name) {
			check.apply(res)
			coll.Add(res, ph.name)
		}
		attacker.Stop()
	}
	coll.Close()
	elapsed := time.Since(start)

	// ---- SLO gate + reports ---------------------------------------------------
	sloResults := evaluateSLOs(cfg.SLOs, &coll.overall)
	passed := slosPassed(sloResults)
	manifest.FinishedAt = time.Now()
	manifest.Requests = coll.overall.Requests
	manifest.SLOResults = sloResults
	manifest.Passed = &passed

	if err := writeReports(reportDir, coll, manifest); err != nil {
		log.Printf("write reports: %v", err)
		return exitError
	}
	if cfg.Report.JUnit {
		if err := writeJUnit(filepath.Join(reportDir, "junit.xml"), sloResults, elapsed); err != nil {
			log.Printf("write junit: %v", err)
			return exitError
		}
	}

	printSummary(&coll.overall, coll, sloResults, elapsed, reportDir)

	if cfg.SLOs.enforced() && !passed {
		return exitSLOFailed
	}
	return exitOK
}

func printSummary(m *vegeta.Metrics, coll *collector, slos []sloResult, elapsed time.Duration, dir string) {
	fmt.Println()
	fmt.Println("==== capacity_bench summary ====")
	fmt.Printf("elapsed       : %s\n", elapsed.Round(time.Millisecond))
	fmt.Printf("requests      : %d\n", m.Requests)
	fmt.Printf("rate (actual) : %.1f rps\n", m.Rate)
	fmt.Printf("throughput    : %.1f rps (successful)\n", m.Throughput)
	fmt.Printf("success       : %.3f%%\n", m.Success*100)
	fmt.Printf("latency p50   : %s\n", m.Latencies.P50.Round(time.Microsecond))
	fmt.Printf("latency p95   : %s\n", m.Latencies.P95.Round(time.Microsecond))
	fmt.Printf("latency p99   : %s\n", m.Latencies.P99.Round(time.Microsecond))
	fmt.Printf("latency max   : %s\n", m.Latencies.Max.Round(time.Microsecond))

	if len(coll.perOp) > 0 {
		fmt.Println()
		fmt.Println("per operation:")
		for _, s := range summarize(coll.perOp) {
			fmt.Printf("  %-24s n=%-8d ok=%6.2f%%  p50=%-10s p95=%-10s p99=%s\n",
				s.Name, s.Requests, s.Success*100, s.P50, s.P95, s.P99)
		}
	}

	if len(m.Errors) > 0 {
		fmt.Printf("\nerrors        : %d distinct\n", len(m.Errors))
		for i, e := range m.Errors {
			if i >= 5 {
				fmt.Printf("  … +%d more\n", len(m.Errors)-5)
				break
			}
			fmt.Printf("  - %s\n", e)
		}
	}

	if len(slos) > 0 {
		fmt.Println()
		fmt.Println("SLOs:")
		for _, r := range slos {
			mark := "PASS"
			if !r.Passed {
				mark = "FAIL"
			}
			fmt.Printf("  [%s] %-14s target %-12s observed %s\n", mark, r.Name, r.Target, r.Observed)
		}
	}
	fmt.Printf("\nreports in    : %s\n", dir)
}

// newAttacker centralises vegeta client options so warmup and every phase
// share identical transport settings.
func newAttacker(cfg *Config) *vegeta.Attacker {
	return vegeta.NewAttacker(
		vegeta.Timeout(cfg.Target.Timeout.D()),
		vegeta.Workers(cfg.Load.Workers),
		vegeta.MaxWorkers(cfg.Load.MaxWorkers),
		vegeta.Connections(cfg.Target.MaxIdleConns),
		vegeta.KeepAlive(*cfg.Target.KeepAlive),
		vegeta.HTTP2(cfg.Target.HTTP2),
	)
}
