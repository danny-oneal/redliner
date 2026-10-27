//! Periodic progress lines on stderr while an attack runs (`report.live_stats`).

use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::time::{Interval, MissedTickBehavior};
use volley::{AttackResult, Metrics};

use crate::slo::fmt_dur;

/// Prints one progress line per interval: the running request total plus the
/// rate, success and p95 of the window since the previous line.
pub struct Live {
    every: Duration,
    ticker: Option<Interval>,
    start: Instant,
    last: Instant,
    total: u64,
    window: Metrics,
}

impl Live {
    /// `every` of zero never prints.
    pub fn new(every: Duration) -> Self {
        let now = Instant::now();
        Self {
            every,
            ticker: None,
            start: now,
            last: now,
            total: 0,
            window: Metrics::new(),
        }
    }

    /// Starts the clock at the first measured hit, so an unmeasured warmup
    /// neither prints nor counts toward the elapsed time.
    pub fn begin(&mut self) {
        let now = Instant::now();
        self.start = now;
        self.last = now;
        self.ticker = (!self.every.is_zero()).then(|| {
            let first = tokio::time::Instant::from_std(now + self.every);
            let mut iv = tokio::time::interval_at(first, self.every);
            iv.set_missed_tick_behavior(MissedTickBehavior::Delay);
            iv
        });
    }

    /// Counts one (already checked) result.
    pub fn add(&mut self, r: &AttackResult) {
        self.total += 1;
        self.window.add(r);
    }

    /// The next result from `rx`, printing a progress line whenever the
    /// interval elapses while waiting for it.
    pub async fn recv(
        &mut self,
        rx: &mut mpsc::Receiver<AttackResult>,
        phase: &str,
    ) -> Option<AttackResult> {
        loop {
            let now = tokio::select! {
                r = rx.recv() => return r,
                now = tick(&mut self.ticker) => now,
            };
            eprintln!("{}", self.line(phase, now));
        }
    }

    /// Renders the line for the window ending at `now`, then starts a new one.
    fn line(&mut self, phase: &str, now: Instant) -> String {
        let mut w = std::mem::take(&mut self.window);
        w.close();
        let span = now.saturating_duration_since(self.last);
        self.last = now;
        let rate = match span.as_secs_f64() {
            s if s > 0.0 => w.requests as f64 / s,
            _ => 0.0,
        };
        let (ok, p95) = if w.requests == 0 {
            ("-".to_string(), "-".to_string())
        } else {
            (format!("{:.2}%", w.success * 100.0), fmt_dur(w.latencies.p95))
        };
        let failed = w.requests - (w.success * w.requests as f64).round() as u64;
        format!(
            "[live {:.1}s] {phase} · {} requests · last {:.1}s: {rate:.1} rps, {ok} ok, p95 {p95}, {failed} failed",
            now.saturating_duration_since(self.start).as_secs_f64(),
            self.total,
            span.as_secs_f64(),
        )
    }
}

/// The next tick; never, when live stats are off.
async fn tick(ticker: &mut Option<Interval>) -> Instant {
    match ticker {
        Some(iv) => iv.tick().await.into_std(),
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn res(code: u16, ms: u64) -> AttackResult {
        AttackResult {
            attack: "steady".into(),
            seq: 0,
            code,
            timestamp: SystemTime::now(),
            latency: Duration::from_millis(ms),
            bytes_in: 0,
            bytes_out: 0,
            error: None,
            method: "GET".into(),
            url: "http://h/".into(),
            body: Vec::new(),
        }
    }

    #[tokio::test]
    async fn line_reports_the_window_and_the_running_total() {
        let mut live = Live::new(Duration::from_secs(10));
        live.begin();
        let t0 = live.start;
        let batch = [res(200, 10), res(200, 10), res(500, 10)];
        let mut want = Metrics::new();
        for r in &batch {
            live.add(r);
            want.add(r);
        }
        want.close();

        let line = live.line("steady", t0 + Duration::from_secs(1));
        assert_eq!(
            line,
            format!(
                "[live 1.0s] steady · 3 requests · last 1.0s: 3.0 rps, 66.67% ok, p95 {}, 1 failed",
                fmt_dur(want.latencies.p95)
            )
        );
    }

    #[tokio::test]
    async fn next_line_covers_only_the_new_window() {
        let mut live = Live::new(Duration::from_secs(10));
        live.begin();
        let t0 = live.start;
        for _ in 0..3 {
            live.add(&res(500, 10));
        }
        live.line("steady", t0 + Duration::from_secs(1));
        live.add(&res(200, 10));

        let line = live.line("peak", t0 + Duration::from_secs(3));
        assert!(
            line.starts_with("[live 3.0s] peak · 4 requests · last 2.0s: 0.5 rps, 100.00% ok, p95 "),
            "{line}"
        );
        assert!(line.ends_with(", 0 failed"), "{line}");
    }

    #[tokio::test]
    async fn empty_window_shows_no_ratios() {
        let mut live = Live::new(Duration::from_secs(10));
        live.begin();
        let t0 = live.start;
        let line = live.line("steady", t0 + Duration::from_secs(2));
        assert_eq!(line, "[live 2.0s] steady · 0 requests · last 2.0s: 0.0 rps, - ok, p95 -, 0 failed");
    }

    #[tokio::test]
    async fn recv_passes_results_through_while_ticking() {
        let mut live = Live::new(Duration::from_millis(100));
        live.begin();
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(350)).await;
            let _ = tx.send(res(200, 1)).await;
        });

        let got = live.recv(&mut rx, "steady").await.expect("one result");
        assert_eq!(got.code, 200);
        assert!(live.recv(&mut rx, "steady").await.is_none(), "closed after the sender drops");
    }

    #[tokio::test]
    async fn zero_interval_never_ticks() {
        let mut live = Live::new(Duration::ZERO);
        live.begin();
        assert!(live.ticker.is_none());
    }
}
