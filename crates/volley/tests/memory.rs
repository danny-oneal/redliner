//! A long attack must not keep its finished workers around: memory stays
//! flat however many hits a run fires.
#![cfg(target_os = "linux")]

use std::sync::Arc;
use std::time::Duration;

use redliner_volley::{Attacker, AttackerConfig, Error, Pacer, Target, Targeter};

/// Fails every hit before any I/O, so hits finish as fast as they fire.
struct NoTarget;

impl Targeter for NoTarget {
    fn target(&self, _seq: u64, _t: &mut Target) -> Result<(), Error> {
        Err(Error::Build("no target".into()))
    }
}

/// Fires `self.0` hits back to back, then stops.
struct Burst(u64);

impl Pacer for Burst {
    fn pace(&self, _elapsed: Duration, hits: u64) -> (Duration, bool) {
        (Duration::ZERO, hits >= self.0)
    }

    fn rate(&self, _elapsed: Duration) -> f64 {
        0.0
    }
}

/// Resident set size in MB.
fn rss_mb() -> f64 {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
    let pages: f64 = statm.split_whitespace().nth(1).unwrap().parse().unwrap();
    pages * 4096.0 / 1e6
}

#[tokio::test(flavor = "multi_thread")]
async fn finished_workers_are_not_retained() {
    let attacker = Attacker::new(AttackerConfig::default()).unwrap();
    let mut rx = attacker.attack(
        Arc::new(NoTarget),
        Arc::new(Burst(100_000)),
        Duration::from_secs(120),
        "mem".into(),
    );
    let (mut n, mut base, mut late) = (0u64, 0.0, 0.0);
    while rx.recv().await.is_some() {
        n += 1;
        // Measured while the attack still runs, before it can free anything.
        match n {
            10_000 => base = rss_mb(),
            90_000 => late = rss_mb(),
            _ => {}
        }
    }
    assert_eq!(n, 100_000);
    // Retaining each finished worker costs ~1.5 KB a hit: ~120 MB here.
    assert!(late - base < 16.0, "RSS grew {:.1} MB over 80k hits", late - base);
}
