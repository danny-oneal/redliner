//! Command-line flags (Go `main.go` flag set).

use std::path::PathBuf;

use clap::Parser;

use crate::config::{Config, ConfigDuration};

/// Redliner: SLO-driven HTTP load testing.
#[derive(Parser, Debug, Clone)]
#[command(name = "redliner", version)]
pub struct Args {
    /// Scenario config file (YAML).
    #[arg(long, default_value = "redliner.yaml")]
    pub config: PathBuf,
    /// Override target.url.
    #[arg(long)]
    pub url: Option<String>,
    /// Override load.rate (RPS).
    #[arg(long)]
    pub rate: Option<f64>,
    /// Override load.duration (Go syntax, e.g. 30s).
    #[arg(long)]
    pub duration: Option<ConfigDuration>,
    /// Override load.warmup (Go syntax, e.g. 5s).
    #[arg(long)]
    pub warmup: Option<ConfigDuration>,
    /// Override load.seed.
    #[arg(long)]
    pub seed: Option<i64>,
    /// Override report.dir.
    #[arg(long)]
    pub out: Option<String>,
    /// Validate config and exit.
    #[arg(long)]
    pub validate: bool,
}

impl Args {
    /// Applies only the flags the user set (`Some`), mirroring Go's `fs.Visit`.
    pub fn apply_overrides(&self, cfg: &mut Config) {
        if let Some(u) = &self.url {
            cfg.target.url = u.clone();
        }
        if let Some(r) = self.rate {
            cfg.load.rate = r;
        }
        if let Some(d) = self.duration {
            cfg.load.duration = d;
        }
        if let Some(w) = self.warmup {
            cfg.load.warmup = w;
        }
        if let Some(s) = self.seed {
            cfg.load.seed = s;
        }
        if let Some(o) = &self.out {
            cfg.report.dir = o.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_set_flags_override() {
        let mut cfg = Config::default();
        cfg.load.rate = 5.0;
        cfg.target.url = "http://a".into();
        let args = Args::parse_from(["redliner", "--rate", "9", "--duration", "2s"]);
        args.apply_overrides(&mut cfg);
        assert_eq!(cfg.load.rate, 9.0);
        assert_eq!(cfg.load.duration.as_std().as_secs(), 2);
        assert_eq!(cfg.target.url, "http://a");
    }
}
