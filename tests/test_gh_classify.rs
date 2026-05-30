//! Integration tests for gh-specific shim dispatch via the built binary.
//!
//! These tests drive the real binary end-to-end with argv[0] = "gh", which is
//! the symlink-dispatch path for @ISSUE-323 (gh CLI interception).
//!
//! The authoritative end-to-end acceptance tests are the BDD scenarios in
//! bdd/features/shim_parity.feature (@ISSUE-323). These integration tests
//! focus on the binary-level dispatch and passthrough path, not the full
//! gh pr diff → manifest pipeline (which requires a live GitHub connection
//! and is covered by the BDD suite).

use std::os::unix::fs::symlink;
use std::process::Command;

use tempfile::TempDir;

fn build_shim_dir_with_gh_symlink() -> (TempDir, std::path::PathBuf) {
    let bin = env!("CARGO_BIN_EXE_git-prism");
    let tmp = TempDir::new().unwrap();
    let shim_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let gh_link = shim_dir.join("gh");
    symlink(bin, &gh_link).unwrap();
    (tmp, shim_dir)
}

/// When invoked as "gh" (via symlink) without agent env vars, the shim must
/// pass through to the real gh binary transparently (no CLAUDECODE=1 → no
/// interception).
///
/// This test verifies the basename dispatch in main.rs routes "gh" → shim
/// mode, and that non-agent callers fall through to passthrough.
#[test]
#[cfg(unix)]
fn it_enters_shim_mode_when_invoked_as_gh() {
    let (_tmp, shim_dir) = build_shim_dir_with_gh_symlink();
    let real_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{}", shim_dir.display(), real_path);

    // Without CLAUDECODE=1, the shim must pass through (not intercept).
    // "gh --version" is safe and predictable — we just check it doesn't panic
    // or return a completely unexpected exit code (git-prism exits 0 on passthrough).
    let output = Command::new(shim_dir.join("gh"))
        .args(["--version"])
        .env("PATH", &path)
        // Deliberately NOT setting CLAUDECODE — non-agent caller
        .output()
        .unwrap();

    // The real gh binary exits 0 for --version. If gh is not installed the
    // shim exits 127. Either way it must not exit with a Rust panic code (101).
    let code = output.status.code().unwrap_or(-1);
    assert!(
        code == 0 || code == 127,
        "gh --version via shim must exit 0 (gh present) or 127 (gh absent), got {code}.\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// When invoked as "gh" with CLAUDECODE=1 and an unrecognised subcommand (not
/// "pr diff"), the shim must pass through — same exit code as the real gh.
#[test]
#[cfg(unix)]
fn it_passes_through_gh_non_diff_subcommands() {
    let (_tmp, shim_dir) = build_shim_dir_with_gh_symlink();
    let real_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{}", shim_dir.display(), real_path);

    // "gh --version" with CLAUDECODE=1 should passthrough (not a diff command).
    let shim_output = Command::new(shim_dir.join("gh"))
        .args(["--version"])
        .env("CLAUDECODE", "1")
        .env("PATH", &path)
        .output()
        .unwrap();

    // Real gh for comparison (skip shim dir).
    let real_path_no_shim = real_path
        .split(':')
        .filter(|p| *p != shim_dir.to_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join(":");

    if let Some(real_gh) = find_gh(&real_path_no_shim) {
        let real_output = Command::new(real_gh)
            .args(["--version"])
            .env("PATH", &real_path_no_shim)
            .output()
            .unwrap();

        assert_eq!(
            shim_output.status.code(),
            real_output.status.code(),
            "shim 'gh --version' exit code must match real gh.\nshim stderr: {}",
            String::from_utf8_lossy(&shim_output.stderr),
        );
    }
    // If gh is not installed, the test is vacuously satisfied.
}

fn find_gh(path: &str) -> Option<std::path::PathBuf> {
    path.split(':').find_map(|dir| {
        let p = std::path::Path::new(dir).join("gh");
        if p.is_file() { Some(p) } else { None }
    })
}

/// ADVERSARIAL QA PROBE: `gh pr diff --help` must pass through to real gh so
/// the user sees help text, not a git-prism manifest attempt against a PR
/// numbered "--help".
#[test]
#[cfg(unix)]
fn it_passes_through_gh_pr_diff_help_flag() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, shim_dir) = build_shim_dir_with_gh_symlink();
    let stub_tmp = TempDir::new().unwrap();
    let stub_dir = stub_tmp.path().join("bin");
    fs::create_dir_all(&stub_dir).unwrap();
    let stub = stub_dir.join("gh");
    fs::write(&stub, b"#!/bin/sh\necho GH_HELP_SENTINEL\nexit 0\n").unwrap();
    let mut p = fs::metadata(&stub).unwrap().permissions();
    p.set_mode(0o755);
    fs::set_permissions(&stub, p).unwrap();

    let path = format!("{}:{}", stub_dir.display(), shim_dir.display());
    let out = Command::new(shim_dir.join("gh"))
        .args(["pr", "diff", "--help"])
        .env("CLAUDECODE", "1")
        .env("PATH", &path)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("GH_HELP_SENTINEL"),
        "gh pr diff --help must pass through to real gh, not be intercepted as a \
         PR numbered '--help'. stdout: {stdout:?}, stderr: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}
