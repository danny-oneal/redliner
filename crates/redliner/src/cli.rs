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
    /// Override report.live_stats: progress line interval (e.g. 5s; 0s = off).
    #[arg(long)]
    pub live_stats: Option<ConfigDuration>,
    /// Validate config and exit.
    #[arg(long)]
    pub validate: bool,
    /// Replay a production request log (JSONL or HAR) instead of a load profile.
    #[arg(long)]
    pub replay: Option<PathBuf>,
    /// Replay time compression: 2 replays twice as fast (needs replay).
    #[arg(long)]
    pub speed: Option<f64>,
    /// Concurrent copies of the replayed log (needs replay).
    #[arg(long)]
    pub copies: Option<u64>,
    /// Fail instead of skipping log entries no scenario matches.
    #[arg(long)]
    pub strict: bool,
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
        if let Some(l) = self.live_stats {
            cfg.report.live_stats = Some(l);
        }
        if let Some(p) = &self.replay {
            // Absolute against the caller's cwd: the config-dir join in
            // `resolve_files` must not re-root a path typed on the command line.
            let abs = std::env::current_dir()
                .map(|d| d.join(p))
                .unwrap_or_else(|_| p.clone());
            cfg.replay.get_or_insert_with(Default::default).source =
                abs.to_string_lossy().into_owned();
        }
        // The knobs tune an existing replay block (from YAML or `--replay`); on
        // their own they must not switch the run into replay mode. When they are
        // passed without a target, `check_replay_knobs` reports one clear error.
        if let Some(r) = cfg.replay.as_mut() {
            if let Some(s) = self.speed {
                r.speed = s;
            }
            if let Some(c) = self.copies {
                r.copies = c;
            }
            if self.strict {
                r.on_unmatched = "fail".into();
            }
        }
    }

    /// One clear error when a replay knob was passed with no replay target,
    /// instead of the pile of validation errors a synthesized empty replay
    /// block would raise. `has_replay` is whether the loaded config is in
    /// replay mode.
    pub fn check_replay_knobs(&self, has_replay: bool) -> Result<(), String> {
        if !has_replay && (self.speed.is_some() || self.copies.is_some() || self.strict) {
            return Err(
                "--speed, --copies and --strict need --replay <log> or a replay block in the config"
                    .into(),
            );
        }
        Ok(())
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

    #[test]
    fn live_stats_flag_overrides() {
        let mut cfg = Config::default();
        Args::parse_from(["redliner", "--live-stats", "0s"]).apply_overrides(&mut cfg);
        assert_eq!(cfg.report.live_stats, Some(ConfigDuration(std::time::Duration::ZERO)));
    }

    #[test]
    fn replay_flags_switch_mode() {
        let mut cfg = Config::default();
        let args = Args::parse_from([
            "redliner",
            "--replay",
            "prod.jsonl",
            "--speed",
            "2",
            "--copies",
            "3",
            "--strict",
        ]);
        args.apply_overrides(&mut cfg);
        let r = cfg.replay.expect("replay set by flags");
        assert_eq!(
            r.source,
            std::env::current_dir()
                .unwrap()
                .join("prod.jsonl")
                .to_string_lossy()
        );
        assert_eq!(r.speed, 2.0);
        assert_eq!(r.copies, 3);
        assert_eq!(r.on_unmatched, "fail");
    }

    #[test]
    fn no_replay_flags_leave_replay_unset() {
        let mut cfg = Config::default();
        Args::parse_from(["redliner"]).apply_overrides(&mut cfg);
        assert!(cfg.replay.is_none());
    }

    #[test]
    fn knobs_without_replay_do_not_create_block() {
        // --speed/--copies/--strict alone must not switch the run into replay
        // mode (which produced a pile of confusing validation errors).
        let mut cfg = Config::default();
        Args::parse_from(["redliner", "--speed", "2", "--strict"]).apply_overrides(&mut cfg);
        assert!(cfg.replay.is_none(), "knobs alone must not create a replay block");
    }

    #[test]
    fn replay_knobs_without_target_is_one_error() {
        let msg = "--speed, --copies and --strict need --replay <log> or a replay block in the config";
        let e = Args::parse_from(["redliner", "--speed", "2"])
            .check_replay_knobs(false)
            .unwrap_err();
        assert_eq!(e, msg);
        assert_eq!(
            Args::parse_from(["redliner", "--copies", "3"])
                .check_replay_knobs(false)
                .unwrap_err(),
            msg
        );
        assert_eq!(
            Args::parse_from(["redliner", "--strict"])
                .check_replay_knobs(false)
                .unwrap_err(),
            msg
        );
        // With a replay target present, or no knobs, it is fine.
        assert!(Args::parse_from(["redliner", "--speed", "2"])
            .check_replay_knobs(true)
            .is_ok());
        assert!(Args::parse_from(["redliner"]).check_replay_knobs(false).is_ok());
    }
}
