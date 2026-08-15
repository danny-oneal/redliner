package main

import (
	"bufio"
	"fmt"
	"math/rand"
	"os"
	"strconv"
	"strings"
)

// Pool is a named source of values that scenarios draw from at attack time
// ({{pool_name}} placeholders). Implementations must be cheap: Sample is on
// the hot path, called once per placeholder per request.

type Pool interface {
	Sample(rng *rand.Rand) any
}

type listPool struct{ values []any }

func (p *listPool) Sample(rng *rand.Rand) any { return p.values[rng.Intn(len(p.values))] }

type rangePool struct{ min, max int }

func (p *rangePool) Sample(rng *rand.Rand) any { return p.min + rng.Intn(p.max-p.min+1) }

// Pools is the resolved registry, built once at startup (static pools) and
// extended by harvest (see bootstrap.go) before the attack begins.
type Pools map[string]Pool

func buildPools(cfg DataConfig) (Pools, error) {
	pools := Pools{}
	for name, pc := range cfg.Pools {
		switch pc.Type {
		case "list":
			pools[name] = &listPool{values: normalizeValues(pc.Values)}
		case "range":
			pools[name] = &rangePool{min: pc.Min, max: pc.Max}
		case "file":
			vals, err := readPoolFile(pc.Path, pc.As)
			if err != nil {
				return nil, fmt.Errorf("pool %q: %w", name, err)
			}
			pools[name] = &listPool{values: vals}
		}
	}
	return pools, nil
}

func readPoolFile(path, as string) ([]any, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	var vals []any
	sc := bufio.NewScanner(f)
	for sc.Scan() {
		line := strings.TrimSpace(sc.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		if as == "int" {
			n, err := strconv.Atoi(line)
			if err != nil {
				return nil, fmt.Errorf("line %q is not an int", line)
			}
			vals = append(vals, n)
		} else {
			vals = append(vals, line)
		}
	}
	if err := sc.Err(); err != nil {
		return nil, err
	}
	if len(vals) == 0 {
		return nil, fmt.Errorf("no values in %s", path)
	}
	return vals, nil
}

// normalizeValues collapses YAML's numeric decoding so int-ish values stay
// ints (GraphQL Int! variables must not be sent as floats).
func normalizeValues(in []any) []any {
	out := make([]any, len(in))
	for i, v := range in {
		out[i] = normalizeScalar(v)
	}
	return out
}

func normalizeScalar(v any) any {
	switch n := v.(type) {
	case float64:
		if n == float64(int64(n)) {
			return int(n)
		}
	case float32:
		if n == float32(int64(n)) {
			return int(n)
		}
	}
	return v
}

// ---- JSON path extraction (harvest) ----------------------------------------

// extractPath walks decoded JSON following a dot path where a segment
// suffixed with [] fans out over array elements, e.g.
//
//	data.country.menus[].categories[].products[].menuItemId
//
// and returns every leaf value found. It is deliberately a tiny subset of
// JSONPath: enough for harvesting IDs without pulling in a dependency.
// (Alternatives: JSONPath or GJSON libraries, or CUE expressions.)
func extractPath(doc any, path string) ([]any, error) {
	segs := strings.Split(path, ".")
	cur := []any{doc}
	for _, seg := range segs {
		fanOut := strings.HasSuffix(seg, "[]")
		key := strings.TrimSuffix(seg, "[]")
		var next []any
		for _, node := range cur {
			m, ok := node.(map[string]any)
			if !ok {
				return nil, fmt.Errorf("path %q: segment %q: expected object, got %T", path, seg, node)
			}
			val, ok := m[key]
			if !ok || val == nil {
				continue // tolerate sparse data
			}
			if fanOut {
				arr, ok := val.([]any)
				if !ok {
					return nil, fmt.Errorf("path %q: segment %q: expected array, got %T", path, seg, val)
				}
				next = append(next, arr...)
			} else {
				next = append(next, val)
			}
		}
		cur = next
	}
	for i := range cur {
		cur[i] = normalizeScalar(cur[i])
	}
	return cur, nil
}

// ---- Templating -------------------------------------------------------------

// renderString substitutes every {{pool}} placeholder in s with a fresh
// sample. Values embedded mid-string are stringified.
func renderString(s string, pools Pools, rng *rand.Rand) (string, error) {
	for {
		start := strings.Index(s, "{{")
		if start < 0 {
			return s, nil
		}
		end := strings.Index(s[start:], "}}")
		if end < 0 {
			return "", fmt.Errorf("unterminated {{ in %q", s)
		}
		name := strings.TrimSpace(s[start+2 : start+end])
		pool, ok := pools[name]
		if !ok {
			return "", fmt.Errorf("unknown pool %q in template %q", name, s)
		}
		v := pool.Sample(rng)
		s = s[:start] + stringify(v) + s[start+end+2:]
	}
}

// renderValue substitutes placeholders inside an arbitrary decoded-YAML value
// (GraphQL variables). A string that is exactly one placeholder keeps the
// sampled value's native type, so `id: "{{ids}}"` stays an Int for Int!
// variables; anything else falls back to string interpolation.
func renderValue(v any, pools Pools, rng *rand.Rand) (any, error) {
	switch t := v.(type) {
	case string:
		trimmed := strings.TrimSpace(t)
		if strings.HasPrefix(trimmed, "{{") && strings.HasSuffix(trimmed, "}}") {
			inner := strings.TrimSpace(trimmed[2 : len(trimmed)-2])
			if !strings.Contains(inner, "{{") && !strings.Contains(inner, "}}") {
				pool, ok := pools[inner]
				if !ok {
					return nil, fmt.Errorf("unknown pool %q", inner)
				}
				return pool.Sample(rng), nil
			}
		}
		return renderString(t, pools, rng)
	case map[string]any:
		out := make(map[string]any, len(t))
		for k, val := range t {
			rv, err := renderValue(val, pools, rng)
			if err != nil {
				return nil, err
			}
			out[k] = rv
		}
		return out, nil
	case []any:
		out := make([]any, len(t))
		for i, val := range t {
			rv, err := renderValue(val, pools, rng)
			if err != nil {
				return nil, err
			}
			out[i] = rv
		}
		return out, nil
	default:
		return normalizeScalar(v), nil
	}
}

func stringify(v any) string {
	switch t := v.(type) {
	case string:
		return t
	case int:
		return strconv.Itoa(t)
	case int64:
		return strconv.FormatInt(t, 10)
	case float64:
		return strconv.FormatFloat(t, 'f', -1, 64)
	default:
		return fmt.Sprintf("%v", t)
	}
}
