//! Turns matched events into a timed schedule of hits, applying the
//! `speed` and `copies` amplification knobs.

use std::time::Duration;

use super::matcher::Matched;

/// One request to fire, `at` after the start of the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduledHit {
    pub at: Duration,
    /// Index into the scenario list the events were matched against.
    pub scenario: usize,
}

/// Hits in non-decreasing `at` order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Schedule {
    pub hits: Vec<ScheduledHit>,
}

/// Builds the schedule for `matched` (sorted by `at`).
///
/// Each copy of the trace is rotated within the trace span rather than
/// appended, so every copy runs in parallel: `copies` times the volume over
/// the same overall duration. `speed` divides every resulting offset.
pub fn build_schedule(matched: &[Matched], speed: f64, copies: u64) -> Schedule {
    let Some(first) = matched.first() else {
        return Schedule::default();
    };
    let copies = u128::from(copies.max(1));
    let offsets: Vec<u128> = matched
        .iter()
        .map(|m| (m.at - first.at).to_std().map_or(0, |d| d.as_nanos()))
        .collect();
    let span = offsets.iter().copied().max().unwrap_or(0);

    let mut emitted: Vec<(u128, usize)> = Vec::with_capacity(matched.len() * copies as usize);
    for k in 0..copies {
        let shift = span * k / copies;
        for (m, &o) in matched.iter().zip(&offsets) {
            let mut p = o + shift;
            if p > span {
                p -= span;
            }
            emitted.push((p, m.scenario));
        }
    }
    // Stable: ties keep copy order, then original order within a copy.
    emitted.sort_by_key(|&(p, _)| p);

    let hits = emitted
        .into_iter()
        .map(|(p, scenario)| ScheduledHit {
            at: Duration::from_nanos((p as f64 / speed) as u64),
            scenario,
        })
        .collect();
    Schedule { hits }
}

impl Schedule {
    pub fn len(&self) -> usize {
        self.hits.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }

    /// The `at` of the last hit; zero when empty.
    pub fn span(&self) -> Duration {
        self.hits.last().map_or(Duration::ZERO, |h| h.at)
    }

    /// The most hits in any `[t, t + 1s)` window that starts at a hit.
    pub fn peak_rps(&self) -> u64 {
        let second = Duration::from_secs(1);
        let mut peak = 0;
        let mut end = 0;
        for (start, hit) in self.hits.iter().enumerate() {
            while end < self.hits.len() && self.hits[end].at < hit.at + second {
                end += 1;
            }
            peak = peak.max(end - start);
        }
        peak as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, TimeZone, Utc};

    fn at_ms(ms: i64) -> DateTime<Utc> {
        Utc.timestamp_millis_opt(1_700_000_000_000 + ms).unwrap()
    }

    fn matched(events: &[(i64, usize)]) -> Vec<Matched> {
        events
            .iter()
            .map(|&(ms, scenario)| Matched {
                at: at_ms(ms),
                scenario,
            })
            .collect()
    }

    fn ats(s: &Schedule) -> Vec<Duration> {
        s.hits.iter().map(|h| h.at).collect()
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn offsets_relative_to_first_event() {
        let s = build_schedule(&matched(&[(0, 0), (250, 0), (1000, 0)]), 1.0, 1);
        assert_eq!(ats(&s), vec![ms(0), ms(250), ms(1000)]);
    }

    #[test]
    fn speed_divides_gaps() {
        let m = matched(&[(0, 0), (250, 0), (1000, 0)]);
        assert_eq!(
            ats(&build_schedule(&m, 2.0, 1)),
            vec![ms(0), ms(125), ms(500)]
        );
        assert_eq!(
            ats(&build_schedule(&m, 0.5, 1)),
            vec![ms(0), ms(500), ms(2000)]
        );
    }

    #[test]
    fn copies_multiply_hits_within_span() {
        let m = matched(&[(0, 0), (1000, 1), (2000, 0), (3000, 2)]);
        let s = build_schedule(&m, 1.0, 3);
        assert_eq!(s.len(), 12);
        assert!(s.hits.iter().all(|h| h.at <= ms(3000)));
        assert!(s.hits.windows(2).all(|w| w[0].at <= w[1].at));
        for (scenario, per_copy) in [(0, 2), (1, 1), (2, 1)] {
            let n = s.hits.iter().filter(|h| h.scenario == scenario).count();
            assert_eq!(n, per_copy * 3, "scenario {scenario}");
        }
        // Copy 0 keeps the original offsets.
        for (offset, scenario) in [(0, 0), (1000, 1), (2000, 0), (3000, 2)] {
            assert!(
                s.hits
                    .iter()
                    .any(|h| h.at == ms(offset) && h.scenario == scenario),
                "missing {offset}ms scenario {scenario}"
            );
        }
        assert_eq!(s.span(), ms(3000));
    }

    #[test]
    fn copies_rotate_within_span() {
        // Span 3s, 2 copies: copy 1 is shifted by 1.5s and wraps past the end.
        let s = build_schedule(&matched(&[(0, 0), (3000, 1)]), 1.0, 2);
        assert_eq!(ats(&s), vec![ms(0), ms(1500), ms(1500), ms(3000)]);
        // Tie at 1.5s: copy 0's hit (scenario 1) is ordered before copy 1's.
        let scenarios: Vec<usize> = s.hits.iter().map(|h| h.scenario).collect();
        assert_eq!(scenarios, vec![0, 0, 1, 1]);
    }

    #[test]
    fn copies_on_zero_span_trace() {
        let s = build_schedule(&matched(&[(5, 0), (5, 1), (5, 2)]), 1.0, 2);
        assert_eq!(s.len(), 6);
        assert!(s.hits.iter().all(|h| h.at == Duration::ZERO));
    }

    #[test]
    fn zero_copies_is_treated_as_one() {
        let s = build_schedule(&matched(&[(0, 0), (10, 0)]), 1.0, 0);
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn peak_rps_counts_densest_second() {
        // 3 hits inside [0, 1s), then 2 hits inside [5s, 6s).
        let s = build_schedule(
            &matched(&[(0, 0), (300, 0), (999, 0), (5000, 0), (5500, 0)]),
            1.0,
            1,
        );
        assert_eq!(s.peak_rps(), 3);
        // Window is half-open: hits exactly 1s apart do not share a window.
        let s = build_schedule(&matched(&[(0, 0), (1000, 0), (2000, 0)]), 1.0, 1);
        assert_eq!(s.peak_rps(), 1);
        // Copies raise the peak.
        let s = build_schedule(&matched(&[(0, 0), (500, 0)]), 1.0, 4);
        assert_eq!(s.peak_rps(), 8);
    }

    #[test]
    fn empty_schedule() {
        let s = build_schedule(&[], 1.0, 3);
        assert_eq!(s.len(), 0);
        assert!(s.is_empty());
        assert_eq!(s.span(), Duration::ZERO);
        assert_eq!(s.peak_rps(), 0);
    }
}
