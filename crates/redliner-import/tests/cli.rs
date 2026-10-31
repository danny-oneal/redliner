//! The binary end to end: flags, stdout, stderr and the exit code.

use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_redliner-import"))
}

#[test]
fn binary_prints_version() {
    let out = bin().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("redliner-import {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn binary_scaffolds_openapi_fixture_to_stdout() {
    let spec = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/petstore3.yaml");
    let out = bin().args(["openapi", spec, "--base-url", "http://h:9/api"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");

    let path = std::env::temp_dir().join(format!("redliner-import-bin-{}.yaml", std::process::id()));
    std::fs::write(&path, &out.stdout).unwrap();
    let cfg = redliner::config::load_config_file(&path);
    let _ = std::fs::remove_file(&path);
    assert_eq!(cfg.unwrap().scenarios.len(), 3);
    assert!(stderr.contains("next: review it"), "{stderr}");
}

#[test]
fn binary_reports_errors_with_exit_2() {
    let out = bin().args(["openapi", "/nonexistent/spec.yaml"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.starts_with("redliner-import: "), "{stderr}");
}
