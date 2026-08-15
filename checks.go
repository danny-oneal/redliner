package main

import (
	"encoding/json"
	"fmt"

	vegeta "github.com/tsenart/vegeta/v12/lib"
)

// checker reclassifies results that passed at the HTTP layer but failed at
// the application layer. The canonical case: GraphQL servers return 200 for
// resolver errors, so a load test that only looks at status codes reports
// 100% success while every response is an error payload.
//
// vegeta counts success from the status code (2xx/3xx), so a failed check
// zeroes the code and records a descriptive error — the failure then shows
// up in the success rate, the SLO gate, and the error report.

// attacker's http.Client, or post-processing results.bin offline.)
type checker struct {
	statuses      map[uint16]bool // empty = accept vegeta's default 2xx/3xx
	graphqlErrors bool
}

func newChecker(cfg ChecksConfig) *checker {
	c := &checker{graphqlErrors: cfg.GraphQLErrors != nil && *cfg.GraphQLErrors}
	if len(cfg.Statuses) > 0 {
		c.statuses = make(map[uint16]bool, len(cfg.Statuses))
		for _, s := range cfg.Statuses {
			c.statuses[uint16(s)] = true
		}
	}
	return c
}

// apply mutates res in place; returns true if the result was reclassified.
func (c *checker) apply(res *vegeta.Result) bool {
	if res.Error != "" {
		return false // transport already failed; keep the original error
	}
	if c.statuses != nil && !c.statuses[res.Code] {
		res.Error = fmt.Sprintf("check: status %d not in allowed set", res.Code)
		res.Code = 0
		return true
	}
	if c.graphqlErrors && res.Code >= 200 && res.Code < 400 && len(res.Body) > 0 {
		var doc any
		if err := json.Unmarshal(res.Body, &doc); err == nil && errsPresent(doc) {
			res.Error = fmt.Sprintf("check: graphql errors (http %d): %s", res.Code, firstGraphQLError(doc))
			res.Code = 0
			return true
		}
	}
	return false
}

// errsPresent reports whether a decoded GraphQL response carries a non-empty
// top-level "errors" array.
func errsPresent(doc any) bool {
	m, ok := doc.(map[string]any)
	if !ok {
		return false
	}
	errs, ok := m["errors"].([]any)
	return ok && len(errs) > 0
}

func firstGraphQLError(doc any) string {
	m, _ := doc.(map[string]any)
	errs, _ := m["errors"].([]any)
	if len(errs) == 0 {
		return ""
	}
	if em, ok := errs[0].(map[string]any); ok {
		if msg, ok := em["message"].(string); ok {
			return msg
		}
	}
	return "unknown error shape"
}
