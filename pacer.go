package main

import (
	"fmt"
	"math"
	"math/rand"
	"sync"
	"time"

	vegeta "github.com/tsenart/vegeta/v12/lib"
)

// PoissonPacer produces a Poisson arrival process: inter-arrival times are
// exponentially distributed with mean 1/Rate seconds, so the *average* request
// rate converges to Rate while individual gaps are random — i.e. a "natural
// cadence" rather than the perfectly-uniform clock of vegeta's default
// ConstantPacer.

//		sine, ramp, or this Poisson behaviour without touching the attacker.
//	  - Self-correcting feedback: when the actual hit count falls behind the
//	    expected count (Rate * elapsed), Pace returns 0 so the attacker fires
//	    immediately to catch up. Without this the heavy right tail of the
//	    exponential would let the run drift below the target rate.
type PoissonPacer struct {
	// MeanRate is the target mean hits per second.
	MeanRate float64

	mu  sync.Mutex
	rng *rand.Rand
}

// Pace returns how long the attacker should sleep before the next hit.
func (p *PoissonPacer) Pace(elapsed time.Duration, hits uint64) (time.Duration, bool) {
	if p.MeanRate <= 0 {
		return 0, true
	}

	// Catch-up: if we're already behind schedule, fire now.
	expected := p.MeanRate * elapsed.Seconds()
	if float64(hits) < expected {
		return 0, false
	}

	p.mu.Lock()
	u := p.rng.Float64()
	p.mu.Unlock()
	if u <= 0 {
		u = math.SmallestNonzeroFloat64
	}

	// Exponential sample: -ln(U)/λ seconds.
	wait := -math.Log(u) / p.MeanRate
	return time.Duration(wait * float64(time.Second)), false
}

// Rate reports the steady-state mean rate. vegeta uses it for logging.
func (p *PoissonPacer) Rate(_ time.Duration) float64 { return p.MeanRate }

func (p *PoissonPacer) String() string {
	return fmt.Sprintf("Poisson{mean=%.1f rps}", p.MeanRate)
}

// NewPoissonPacer is a small constructor that hides the rng wiring.
func NewPoissonPacer(rate float64, seed int64) *PoissonPacer {
	return &PoissonPacer{
		MeanRate: rate,
		rng:      rand.New(rand.NewSource(seed)),
	}
}

// Compile-time interface check.
var _ vegeta.Pacer = (*PoissonPacer)(nil)

// phase is one (pacer, duration) attack segment. The steps profile expands
// to several phases; every other profile is a single phase. Each phase gets
// a fresh attacker (see main.go) — reusing one across Attack() calls leaves
// its worker pool in a state where the second attack fires nothing.
type phase struct {
	name     string
	pacer    vegeta.Pacer
	duration time.Duration
	rate     float64 // nominal, for logs/manifest
}

// buildPhases is the pacing factory (Factory Method pattern): it maps the
// declarative load profile onto vegeta.Pacer strategies.
//
// Alternatives worth knowing: vegeta.SinePacer models diurnal waves; a
// custom pacer could replay production traffic shapes from a file.
func buildPhases(load LoadConfig) ([]phase, error) {
	switch load.Profile {
	case "constant":
		return []phase{{
			name:     "constant",
			pacer:    vegeta.Rate{Freq: int(load.Rate), Per: time.Second},
			duration: load.Duration.D(),
			rate:     load.Rate,
		}}, nil
	case "poisson":
		return []phase{{
			name:     "poisson",
			pacer:    NewPoissonPacer(load.Rate, load.Seed),
			duration: load.Duration.D(),
			rate:     load.Rate,
		}}, nil
	case "ramp":
		secs := load.Duration.D().Seconds()
		if secs <= 0 {
			return nil, fmt.Errorf("ramp profile needs load.duration > 0")
		}
		return []phase{{
			name: "ramp",
			pacer: vegeta.LinearPacer{
				StartAt: vegeta.Rate{Freq: int(load.Ramp.From), Per: time.Second},
				Slope:   (load.Ramp.To - load.Ramp.From) / secs,
			},
			duration: load.Duration.D(),
			rate:     load.Ramp.To,
		}}, nil
	case "steps":
		phases := make([]phase, len(load.Steps))
		for i, s := range load.Steps {
			phases[i] = phase{
				name:     fmt.Sprintf("step-%d(%g rps)", i+1, s.Rate),
				pacer:    vegeta.Rate{Freq: int(s.Rate), Per: time.Second},
				duration: s.Duration.D(),
				rate:     s.Rate,
			}
		}
		return phases, nil
	default:
		return nil, fmt.Errorf("unsupported load profile %q", load.Profile)
	}
}
