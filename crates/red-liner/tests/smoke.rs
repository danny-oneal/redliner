#[test]
fn prints_version() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_red-liner"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("red-liner"));
}
