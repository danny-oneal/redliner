package main

import (
	"math/rand"
	"sort"
	"sync"

	vegeta "github.com/tsenart/vegeta/v12/lib"
)

// weightedMix selects a scenario per hit using a cumulative-weight table and
// one rng draw — O(log n) via binary search, stable regardless of how many
// scenarios a team defines.

type weightedMix struct {
	scenarios []Scenario
	cum       []float64
}

func newWeightedMix(scenarios []Scenario) *weightedMix {
	mix := &weightedMix{scenarios: scenarios, cum: make([]float64, len(scenarios))}
	var running float64
	for i, s := range scenarios {
		running += s.Weight
		mix.cum[i] = running
	}
	return mix
}

func (m *weightedMix) pick(rng *rand.Rand) *Scenario {
	total := m.cum[len(m.cum)-1]
	r := rng.Float64() * total
	i := sort.SearchFloat64s(m.cum, r)
	if i >= len(m.scenarios) {
		i = len(m.scenarios) - 1
	}
	return &m.scenarios[i]
}

// NewTargeter builds the vegeta.Targeter: pick a scenario by weight, sample
// its variables from the pools, and let the protocol encoder produce the
// concrete HTTP request. Safe for vegeta's concurrent worker pool — the
// mutex guards the shared rng, keeping runs reproducible from one seed.
func NewTargeter(enc encoder, mix *weightedMix, pools Pools, seed int64) vegeta.Targeter {
	var mu sync.Mutex
	rng := rand.New(rand.NewSource(seed))

	return func(t *vegeta.Target) error {
		if t == nil {
			return vegeta.ErrNilTarget
		}
		mu.Lock()
		defer mu.Unlock()
		sc := mix.pick(rng)
		return enc.encode(t, sc, pools, rng)
	}
}
