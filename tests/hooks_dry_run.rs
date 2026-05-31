#![cfg(unix)]

//! Regression test for issue #326: `git-prism hooks install --path-shim --dry-run`
//! must honor --dry-run and NOT create the symlink.
//!
//! This drives the built binary end-to-end to verify the fix.

use std::process::Command;
use tempfile::TempDir;

/// When --dry-run is passed to `hooks install --path-shim`, the symlink must
/// NOT be created and exit code must be 0.
#[test]
fn hooks_install_path_shim_dry_run_does_not_create_symlink() {
    let bin = env!("CARGO_BIN_EXE_git-prism");
    let home = TempDir::new().unwrap();

    let out = Command::new(bin)
        .env("HOME", home.path())
        .args(["hooks", "install", "--path-shim", "--dry-run"])
        .output()
        .unwrap();

    // Exit code must be 0
    assert_eq!(
        out.status.code(),
        Some(0),
        "hooks install --path-shim --dry-run must exit 0"
    );

    // stdout/stderr must contain "dry-run" to confirm dry-run mode was active
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{}{}", stdout, stderr);
    assert!(
        combined.contains("dry-run"),
        "output must contain 'dry-run' confirmation; got stdout: {}\nstderr: {}",
        stdout,
        stderr
    );

    // The symlink must NOT exist
    let symlink_path = home.path().join(".local/share/git-prism/bin/git");
    assert!(
        !symlink_path.exists(),
        "symlink must not exist after --dry-run; checked at {}",
        symlink_path.display()
    );
}

/// Control test: without --dry-run, the symlink IS created.
#[test]
fn hooks_install_path_shim_without_dry_run_creates_symlink() {
    let bin = env!("CARGO_BIN_EXE_git-prism");
    let home = TempDir::new().unwrap();

    let out = Command::new(bin)
        .env("HOME", home.path())
        .args(["hooks", "install", "--path-shim"])
        .output()
        .unwrap();

    // Exit code must be 0
    assert_eq!(
        out.status.code(),
        Some(0),
        "hooks install --path-shim must exit 0"
    );

    // The symlink MUST exist
    let symlink_path = home.path().join(".local/share/git-prism/bin/git");
    assert!(
        symlink_path.exists(),
        "symlink must exist after install without --dry-run; checked at {}",
        symlink_path.display()
    );

    // Verify it's a symlink
    assert!(
        symlink_path.is_symlink(),
        "path must be a symlink; checked at {}",
        symlink_path.display()
    );
}
