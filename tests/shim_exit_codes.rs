#![cfg(unix)]

//! Adversarial QA (issue #296): the shim must distinguish exit 126
//! (real git found but not executable) from exit 127 (real git not found),
//! per POSIX shell convention.
//!
//! This drives the built binary end-to-end via its argv[0] = "git" dispatch.

use std::os::unix::fs::symlink;
use std::process::Command;

use tempfile::TempDir;

/// When the resolver finds a `git` regular file that carries an execute bit but
/// the process-replacement call fails with `PermissionDenied` (here: the bit is
/// set for group/other only, so the owner may not run it), the shim must exit
/// 126, NOT 127. A directory named `git` is a different case: a shell skips
/// it, and so does the resolver (see `shim_gate3_rc_function_pentest`).
#[test]
fn it_exits_126_when_real_git_is_found_but_unexecutable() {
    use std::os::unix::fs::PermissionsExt;

    let bin = env!("CARGO_BIN_EXE_git-prism");
    let tmp = TempDir::new().unwrap();
    let shim_dir = tmp.path().join("shimbin");
    let real_dir = tmp.path().join("realbin");
    std::fs::create_dir_all(&shim_dir).unwrap();
    std::fs::create_dir_all(&real_dir).unwrap();

    // Symlink the shim binary in as `git` (argv[0] = "git" triggers shim mode).
    let shim_git = shim_dir.join("git");
    symlink(bin, &shim_git).unwrap();

    // The "real git" is a regular file the owner cannot execute: it passes the
    // resolver's execute-bit check but launching it fails with EACCES.
    let real_git = real_dir.join("git");
    std::fs::write(&real_git, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&real_git, std::fs::Permissions::from_mode(0o011)).unwrap();

    let path = format!("{}:{}", shim_dir.display(), real_dir.display());

    let status = Command::new(&shim_git)
        // GIT_PRISM_INSIDE_SHIM=1 forces the passthrough path directly.
        .env("GIT_PRISM_INSIDE_SHIM", "1")
        .env("PATH", &path)
        .arg("status")
        .status()
        .unwrap();

    assert_eq!(
        status.code(),
        Some(126),
        "found-but-unexecutable real git must yield exit 126, not 127"
    );
}

/// Adversarial QA (issue #324, gate 3; reworked for #387): `git-prism shim
/// status` must report a legacy symlink, even a dangling one (target no longer
/// exists), as a legacy PATH shim to migrate, and must NOT claim the shim
/// function is installed.
#[test]
fn shim_status_reports_a_dangling_legacy_symlink_as_legacy_not_installed() {
    let bin = env!("CARGO_BIN_EXE_git-prism");
    let home = TempDir::new().unwrap();
    let shim_dir = home.path().join(".local/share/git-prism/bin");
    std::fs::create_dir_all(&shim_dir).unwrap();

    // A symlink whose target does not exist: the canonical "broken link".
    symlink(
        home.path().join("nonexistent-target-xyz"),
        shim_dir.join("git"),
    )
    .unwrap();

    let out = Command::new(bin)
        .env("HOME", home.path())
        .args(["shim", "status"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("legacy PATH shim: present at"),
        "dangling symlink must be reported as a legacy PATH shim; got: {stdout}"
    );
    assert!(
        stdout.contains("shim: not installed"),
        "a legacy symlink is not the shim function; got: {stdout}"
    );
}

/// Issue #387: when the structured handler fails (here: a ref that does not
/// exist), the shim must hand the command to real git so the agent sees real
/// git's stderr and exit code, with no `git-prism` text.
#[test]
fn it_returns_real_git_stderr_and_exit_code_when_handler_fails() {
    let bin = env!("CARGO_BIN_EXE_git-prism");
    let tmp = TempDir::new().unwrap();
    let shim_dir = tmp.path().join("shimbin");
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&shim_dir).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let shim_git = shim_dir.join("git");
    symlink(bin, &shim_git).unwrap();

    let run_git = |args: &[&str]| {
        let out = Command::new("/usr/bin/git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "setup git {args:?} failed");
    };
    run_git(&["init", "-q"]);
    run_git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "init",
    ]);

    let path = format!("{}:/usr/bin:/bin", shim_dir.display());
    let args = ["diff", "nosuchref..HEAD"];

    let real = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(&repo)
        .output()
        .unwrap();
    let shimmed = Command::new(bin)
        .args(["shim-run", "git"])
        .args(args)
        .current_dir(&repo)
        .env_remove("CI")
        .env_remove("GIT_PRISM_PASSTHROUGH")
        .env_remove("GIT_PRISM_DISABLE")
        .env_remove("GIT_PRISM_INSIDE_SHIM")
        .env("AI_AGENT", "claude-code")
        .env("PATH", &path)
        .output()
        .unwrap();

    assert_eq!(shimmed.status.code(), real.status.code());
    assert_eq!(real.status.code(), Some(128));
    assert_eq!(
        String::from_utf8_lossy(&shimmed.stderr),
        String::from_utf8_lossy(&real.stderr)
    );
    assert!(!String::from_utf8_lossy(&shimmed.stderr).contains("git-prism"));
}
