//! Capture build-time provenance for the `--version` long-form output.
//!
//! Writes a multi-line `long_version.txt` to `OUT_DIR`; main.rs
//! `include_str!`s it as `LONG_VERSION`. We can't use the simpler
//! `cargo:rustc-env=...` directive because cargo treats `\n` in the
//! value as the line terminator for the directive itself.
//!
//! Falls back gracefully when build-time git capture is empty (e.g.
//! a `cargo install` from a crates.io tarball outside a git
//! checkout).

use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "<unknown — built outside a git checkout>".to_owned());

    let target = std::env::var("TARGET")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "<unknown>".to_owned());

    let features = if std::env::var("CARGO_FEATURE_IMPERSONATE").is_ok() {
        "impersonate"
    } else {
        "<default>"
    };

    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let long_version =
        format!("{version}\ncommit:   {sha}\ntarget:   {target}\nfeatures: {features}");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let path = std::path::Path::new(&out_dir).join("long_version.txt");
    std::fs::write(&path, long_version).expect("write long_version.txt");

    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/heads");
}
