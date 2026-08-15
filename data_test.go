package main

import (
	"encoding/json"
	"math/rand"
	"testing"
)

func testPools() Pools {
	return Pools{
		"ids":   &listPool{values: []any{1, 2, 3}},
		"skus":  &listPool{values: []any{"SKU-1"}},
		"range": &rangePool{min: 5, max: 5},
	}
}

func TestRenderValueKeepsTypes(t *testing.T) {
	rng := rand.New(rand.NewSource(1))
	v, err := renderValue(map[string]any{"id": "{{ids}}", "sku": "x-{{skus}}"}, testPools(), rng)
	if err != nil {
		t.Fatal(err)
	}
	m := v.(map[string]any)
	if _, ok := m["id"].(int); !ok {
		t.Errorf("whole-placeholder variable should keep int type, got %T", m["id"])
	}
	if m["sku"] != "x-SKU-1" {
		t.Errorf("embedded placeholder should interpolate, got %v", m["sku"])
	}
}

func TestRenderStringUnknownPool(t *testing.T) {
	rng := rand.New(rand.NewSource(1))
	if _, err := renderString("/x/{{nope}}", testPools(), rng); err == nil {
		t.Error("unknown pool should error")
	}
}

func TestRangePoolBounds(t *testing.T) {
	rng := rand.New(rand.NewSource(1))
	p := &rangePool{min: 5, max: 5}
	if got := p.Sample(rng); got != 5 {
		t.Errorf("degenerate range should return 5, got %v", got)
	}
}

func TestExtractPath(t *testing.T) {
	raw := `{"data":{"country":{"menus":[
		{"categories":[{"products":[{"id":1,"items":[{"sku":"A"},{"sku":"B"}]}]}]},
		{"categories":[{"products":[{"id":2,"items":[{"sku":"C"}]}]}]}
	]}}}`
	var doc any
	if err := json.Unmarshal([]byte(raw), &doc); err != nil {
		t.Fatal(err)
	}

	ids, err := extractPath(doc, "data.country.menus[].categories[].products[].id")
	if err != nil {
		t.Fatal(err)
	}
	if len(ids) != 2 || ids[0] != 1 || ids[1] != 2 {
		t.Errorf("ids = %v, want [1 2] as ints", ids)
	}

	skus, err := extractPath(doc, "data.country.menus[].categories[].products[].items[].sku")
	if err != nil {
		t.Fatal(err)
	}
	if len(skus) != 3 {
		t.Errorf("skus = %v, want 3 values", skus)
	}

	if _, err := extractPath(doc, "data.country.menus.categories"); err == nil {
		t.Error("array traversed without [] should error")
	}
}
