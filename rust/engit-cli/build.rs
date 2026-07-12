//! Build script: derive a git-tag-based version string for `engit-cli`.
//!
//! See `rust/envoy-cli/build.rs` for the full rationale -- this mirrors that
//! script exactly, just for the `engit` binary. `Cargo.toml`'s
//! `[workspace.package] version` stays a fixed placeholder (`0.0.0`); the
//! *reported* version (via `engit --version`) comes from this build-time
//! `ENGIT_VERSION` environment variable, consumed in `src/main.rs` via
//! `#[command(version = env!("ENGIT_VERSION"))]`.

use std::process::Command;

fn main() {
    let version = git_describe().unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
    println!("cargo:rustc-env=ENGIT_VERSION={version}");

    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs");
}

/// Run `git describe --tags --always --dirty` from the repo root and return
/// its trimmed stdout, or `None` if `git` isn't available or the command
/// fails (e.g. not a git checkout, no tags reachable, shallow clone without
/// tag history).
fn git_describe() -> Option<String> {
    let output = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let version = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}
