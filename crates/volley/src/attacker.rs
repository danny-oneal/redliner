//! The async paced attack loop — the engine core (Go `vegeta.Attacker.Attack`).

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tokio::sync::mpsc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::pacer::{Pacer, Targeter};
use crate::target::{AttackResult, Target};
use crate::Error;

/// Tuning for an [`Attacker`]'s HTTP client and worker pool.
#[derive(Debug, Clone)]
pub struct AttackerConfig {
    /// Per-request timeout.
    pub timeout: Duration,
    /// Nominal/initial concurrency (informational; `max_workers` is the hard cap).
    pub workers: usize,
    /// Hard cap on in-flight requests.
    pub max_workers: usize,
    /// Max idle pooled connections per host.
    pub max_idle_conns: usize,
    /// Keep connections alive and pooled between hits.
    pub keep_alive: bool,
    /// Negotiate HTTP/2 with prior knowledge (h2c / cleartext or ALPN).
    pub http2: bool,
}

impl Default for AttackerConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            workers: 10,
            max_workers: 50,
            max_idle_conns: 100,
            keep_alive: true,
            http2: false,
        }
    }
}

/// Fires HTTP requests at a paced rate and streams one [`AttackResult`] per hit.
pub struct Attacker {
    client: reqwest::Client,
    cfg: AttackerConfig,
}

impl Attacker {
    /// Builds an attacker, constructing the underlying reqwest client from `cfg`.
    pub fn new(cfg: AttackerConfig) -> Result<Self, Error> {
        let mut builder = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .pool_max_idle_per_host(if cfg.keep_alive { cfg.max_idle_conns } else { 0 });

        if !cfg.keep_alive {
            // Do not retain idle connections between hits.
            builder = builder.pool_idle_timeout(Some(Duration::ZERO));
        }
        if cfg.http2 {
            builder = builder.http2_prior_knowledge();
        }

        let client = builder.build().map_err(|e| Error::Build(e.to_string()))?;
        Ok(Self { client, cfg })
    }

    /// Spawns the scheduler and returns a receiver that yields one result per hit,
    /// then closes once the duration elapses (or the pacer stops) and all
    /// outstanding workers have flushed their results.
    pub fn attack<P, T>(
        &self,
        targeter: Arc<T>,
        pacer: Arc<P>,
        duration: Duration,
        name: String,
    ) -> mpsc::Receiver<AttackResult>
    where
        P: Pacer + 'static,
        T: Targeter + 'static,
    {
        let max_workers = self.cfg.max_workers.max(1);
        // At most `max_workers` results can be unsent at once (one per live
        // worker); a buffer >= that keeps sends from blocking under a draining
        // consumer, with headroom for a briefly-stalled one.
        let buffer = max_workers.saturating_mul(2).max(256);
        let (tx, rx) = mpsc::channel::<AttackResult>(buffer);

        let client = self.client.clone();
        let sem = Arc::new(Semaphore::new(max_workers));

        tokio::spawn(async move {
            let start = Instant::now();
            let mut hits: u64 = 0;
            let mut workers: JoinSet<()> = JoinSet::new();

            loop {
                let elapsed = start.elapsed();
                let (wait, stop) = pacer.pace(elapsed, hits);
                if stop || elapsed >= duration {
                    break;
                }
                if !wait.is_zero() {
                    // Cap: never sleep past the deadline.
                    let remaining = duration.saturating_sub(elapsed);
                    if wait >= remaining {
                        break;
                    }
                    tokio::time::sleep(wait).await;
                    continue;
                }

                // Bound in-flight requests. `acquire_owned` only errors on a
                // closed semaphore, which never happens here.
                let permit = match sem.clone().acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => break,
                };

                let seq = hits;
                hits += 1;

                let tx = tx.clone();
                let client = client.clone();
                let targeter = targeter.clone();
                let name = name.clone();

                workers.spawn(async move {
                    let result = run_hit(&client, targeter.as_ref(), name, seq).await;
                    let _ = tx.send(result).await;
                    drop(permit);
                });
            }

            // Drop the scheduler's own sender so the channel can close once the
            // workers (which hold the remaining clones) finish.
            drop(tx);
            while workers.join_next().await.is_some() {}
        });

        rx
    }
}

/// Builds and issues one request, mapping the outcome into an [`AttackResult`].
async fn run_hit<T: Targeter + ?Sized>(
    client: &reqwest::Client,
    targeter: &T,
    name: String,
    seq: u64,
) -> AttackResult {
    let timestamp = SystemTime::now();
    let mut target = Target::default();

    if let Err(e) = targeter.target(&mut target) {
        return AttackResult {
            attack: name,
            seq,
            code: 0,
            timestamp,
            latency: Duration::ZERO,
            bytes_in: 0,
            bytes_out: 0,
            error: Some(e.to_string()),
            method: target.method.to_string(),
            url: target.url,
        };
    }

    let method = target.method.clone();
    let url = target.url.clone();
    let bytes_out = target.body.len() as u64;

    let req = client
        .request(method.clone(), &url)
        .headers(target.header.clone())
        .body(target.body);

    let start = Instant::now();
    let send_ts = SystemTime::now();

    match req.send().await {
        Ok(resp) => {
            let code = resp.status().as_u16();
            match resp.bytes().await {
                Ok(body) => AttackResult {
                    attack: name,
                    seq,
                    code,
                    timestamp: send_ts,
                    latency: start.elapsed(),
                    bytes_in: body.len() as u64,
                    bytes_out,
                    error: None,
                    method: method.to_string(),
                    url,
                },
                Err(e) => AttackResult {
                    attack: name,
                    seq,
                    code,
                    timestamp: send_ts,
                    latency: start.elapsed(),
                    bytes_in: 0,
                    bytes_out,
                    error: Some(e.to_string()),
                    method: method.to_string(),
                    url,
                },
            }
        }
        Err(e) => AttackResult {
            attack: name,
            seq,
            code: 0,
            timestamp: send_ts,
            latency: start.elapsed(),
            bytes_in: 0,
            bytes_out,
            error: Some(e.to_string()),
            method: method.to_string(),
            url,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pacer::ConstantPacer;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A targeter that points every hit at a fixed URL with a GET.
    struct UrlTargeter {
        url: String,
    }

    impl Targeter for UrlTargeter {
        fn target(&self, t: &mut Target) -> Result<(), Error> {
            t.method = http::Method::GET;
            t.url = self.url.clone();
            Ok(())
        }
    }

    /// Spawns a minimal HTTP/1.1 server that answers every request with 200 OK
    /// and closes the connection. Returns the bound base URL.
    async fn spawn_ok_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = socket.read(&mut buf).await;
                    let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = socket.write_all(resp).await;
                    let _ = socket.flush().await;
                });
            }
        });
        format!("http://{addr}/")
    }

    fn cfg(timeout: Duration) -> AttackerConfig {
        AttackerConfig {
            timeout,
            workers: 10,
            max_workers: 50,
            max_idle_conns: 100,
            keep_alive: true,
            http2: false,
        }
    }

    #[tokio::test]
    async fn attack_fires_at_rate() {
        let url = spawn_ok_server().await;
        let attacker = Attacker::new(cfg(Duration::from_secs(5))).unwrap();
        let pacer = Arc::new(ConstantPacer { freq: 50, per: Duration::from_secs(1) });
        let targeter = Arc::new(UrlTargeter { url });

        let mut rx =
            attacker.attack(targeter, pacer, Duration::from_secs(2), "rate".to_string());

        let mut results = Vec::new();
        while let Some(r) = rx.recv().await {
            results.push(r);
        }

        assert!(
            (90..=110).contains(&results.len()),
            "expected ~100 results, got {}",
            results.len()
        );
        for r in &results {
            assert_eq!(r.code, 200, "unexpected code, err = {:?}", r.error);
            assert!(r.error.is_none(), "unexpected error: {:?}", r.error);
        }
    }

    #[tokio::test]
    async fn attack_records_transport_error() {
        // Nothing listens on port 1 → connection refused.
        let attacker = Attacker::new(cfg(Duration::from_secs(1))).unwrap();
        let pacer = Arc::new(ConstantPacer { freq: 20, per: Duration::from_secs(1) });
        let targeter = Arc::new(UrlTargeter { url: "http://127.0.0.1:1/".to_string() });

        let mut rx =
            attacker.attack(targeter, pacer, Duration::from_millis(500), "err".to_string());

        let mut results = Vec::new();
        while let Some(r) = rx.recv().await {
            results.push(r);
        }

        assert!(!results.is_empty(), "expected at least one result");
        for r in &results {
            assert!(r.error.is_some(), "expected a transport error");
            assert_eq!(r.code, 0, "transport failure should carry code 0");
        }
    }

    #[tokio::test]
    async fn attack_stops_at_deadline() {
        let url = spawn_ok_server().await;
        let attacker = Attacker::new(cfg(Duration::from_secs(5))).unwrap();
        let pacer = Arc::new(ConstantPacer { freq: 50, per: Duration::from_secs(1) });
        let duration = Duration::from_millis(500);
        let targeter = Arc::new(UrlTargeter { url });

        let start = Instant::now();
        let mut rx = attacker.attack(targeter, pacer, duration, "deadline".to_string());

        let mut results = Vec::new();
        while let Some(r) = rx.recv().await {
            results.push(r);
        }
        let total = start.elapsed();

        // The channel closes cleanly shortly after the deadline (drain grace).
        assert!(
            total < duration + Duration::from_secs(2),
            "attack ran {total:?}, past deadline + grace"
        );
        assert!(!results.is_empty(), "expected some results");
        // ~25 hits for 500ms @ 50rps; a hard upper bound proves it stopped.
        assert!(
            results.len() <= 45,
            "fired too many ({}) — did not stop at the deadline",
            results.len()
        );
    }
}
