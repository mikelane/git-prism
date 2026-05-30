#![cfg(unix)]

//! Adversarial QA (issue #296): the shim must distinguish exit 126
//! (real git found but not executable) from exit 127 (real git not found),
//! per POSIX shell convention.
//!
//! This drives the built binary end-to-end via its argv[0] = "git" dispatch.

use std::os::unix::fs::symlink;
use std::process::Command;

use tempfile::TempDir;

/// When the resolver finds a `git` entry that passes the executable check but
/// the process-replacement call fails with `PermissionDenied` (e.g. a directory
/// named `git`), the shim must exit 126, NOT 127.
#[test]
fn it_exits_126_when_real_git_is_found_but_unexecutable() {
    let bin = env!("CARGO_BIN_EXE_git-prism");
    let tmp = TempDir::new().unwrap();
    let shim_dir = tmp.path().join("shimbin");
    let real_dir = tmp.path().join("realbin");
    std::fs::create_dir_all(&shim_dir).unwrap();
    std::fs::create_dir_all(&real_dir).unwrap();

    // Symlink the shim binary in as `git` (argv[0] = "git" triggers shim mode).
    let shim_git = shim_dir.join("git");
    symlink(bin, &shim_git).unwrap();

    // The "real git" is a *directory* named `git`: it passes the resolver's
    // `is_executable` check (directories carry the exec bit) but launching it
    // fails with EACCES / PermissionDenied — the found-but-unexecutable case.
    std::fs::create_dir(real_dir.join("git")).unwrap();

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

/// Adversarial QA (issue #324, gate 3): `git-prism shim status` must report a
/// dangling symlink (target no longer exists) as BROKEN, not "installed".
///
/// The `PathShimStatus::BrokenLink` variant and its doc comment promise exactly
/// this. But `path_shim_status` uses `std::fs::read_link`, which SUCCEEDS on a
/// dangling symlink (it only reads the link's textual target). So the canonical
/// broken-link case is mis-classified as `Installed`. This drives the built
/// binary end-to-end and currently FAILS.
#[test]
fn shim_status_reports_broken_for_dangling_symlink() {
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
        stdout.contains("broken"),
        "dangling symlink must be reported as broken; got: {stdout}"
    );
    assert!(
        !stdout.contains("shim: installed"),
        "dangling symlink must NOT be reported as installed; got: {stdout}"
    );
}
