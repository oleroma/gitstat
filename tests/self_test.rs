//! gitstat tests itself on the PC's screen: the app is started with
//! `--self-test`, opens its views by itself, records them and reports
//! (src/selftest.rs). It runs beside whatever gitstat is running, with a
//! config and a data folder of its own under the build folder, and shows
//! on the screen for a few seconds.
//!
//! Not in CI, which has no screen worth looking at.
#![cfg(windows)]

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn gitstat_tests_itself_on_screen() {
    if std::env::var_os("GITHUB_ACTIONS").is_some() {
        return;
    }
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("self-test");
    // Last run's pictures go: only this folder, which nothing else uses.
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(out.join("data")).expect("make the self-test folder");

    // The repos to show: the folder that holds this repo's (the build
    // worktree is build/src inside it), read without touching them.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repos = manifest
        .ancestors()
        .find(|dir| dir.join(".git").is_dir())
        .and_then(|repo| repo.parent())
        .unwrap_or(&manifest)
        .to_path_buf();
    let config = serde_json::json!({
        "path": repos,
        "depth": 1,
        "watch": false,
        "interval_minutes": 600.0,
        "record_frames": true,
        "flip_frames": out.join("frames"),
        "panel_slide_ms": 300,
    });
    let config_file = out.join("config.json");
    std::fs::write(&config_file, config.to_string()).expect("write its config");

    let mut child = Command::new(env!("CARGO_BIN_EXE_gitstat"))
        .arg("--self-test")
        .arg(&out)
        .arg("--config")
        .arg(&config_file)
        // Its data folder: nothing of the user's gitstat is read or written.
        .env("LOCALAPPDATA", out.join("data"))
        .spawn()
        .expect("start gitstat");
    let began = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait for gitstat") {
            break status;
        }
        if began.elapsed() > Duration::from_secs(300) {
            let _ = child.kill();
            panic!("gitstat's self-test did not end in five minutes");
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let report = std::fs::read_to_string(out.join("report.txt")).unwrap_or_default();
    println!("self-test in {}:\n{report}", out.display());
    assert!(
        status.success() && report.contains("result: done"),
        "gitstat's self-test failed ({status}):\n{report}"
    );
}
