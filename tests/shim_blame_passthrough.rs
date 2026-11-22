#![cfg(unix)]

//! Issue #387: `git blame` always passes through to real git. Structured blame
//! is tracked in #388; until then the shim must never answer from HEAD content
//! while real git blames something else.

use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_git-prism");
const REAL_GIT: &str = "/usr/bin/git";

fn real_git(dir: &Path, args: &[&str]) -> Output {
    Command::new(REAL_GIT)
        .env_remove("GIT_PRISM_PASSTHROUGH")
        .current_dir(dir)
        .env_remove("GIT_PRISM_INSIDE_SHIM")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap()
}

fn agent_shim_run(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .current_dir(dir)
        .env("PATH", "/usr/bin:/bin")
        .env("AI_AGENT", "claude-code")
        .env_remove("CI")
        .env_remove("GIT_PRISM_INSIDE_SHIM")
        .env_remove("GIT_PRISM_PASSTHROUGH")
        .env_remove("GIT_PRISM_DISABLE")
        .arg("shim-run")
        .arg("git")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn setup_git(dir: &Path, args: &[&str]) {
    let out = real_git(dir, args);
    assert!(out.status.success(), "fixture git {args:?} failed: {out:?}");
}

fn repo_with_committed_file() -> TempDir {
    let dir = TempDir::new().unwrap();
    setup_git(dir.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
    setup_git(dir.path(), &["add", "--", "a.txt"]);
    setup_git(dir.path(), &["commit", "-qm", "first"]);
    dir
}

#[test]
fn it_passes_git_blame_through_to_real_git() {
    let repo = repo_with_committed_file();

    let shim = agent_shim_run(repo.path(), &["blame", "a.txt"]);
    let real = real_git(repo.path(), &["blame", "a.txt"]);

    assert_eq!(real.status.code(), Some(0));
    assert!(!real.stdout.is_empty(), "reference blame printed nothing");
    assert_eq!(shim.stdout, real.stdout);
    assert_eq!(shim.status.code(), real.status.code());
}

#[test]
fn it_passes_git_blame_of_a_missing_path_through_with_real_gits_exit_code() {
    let repo = repo_with_committed_file();

    let shim = agent_shim_run(repo.path(), &["blame", "missing.txt"]);
    let real = real_git(repo.path(), &["blame", "missing.txt"]);

    assert_eq!(shim.status.code(), real.status.code());
    assert_ne!(real.status.code(), Some(0));
    assert!(
        shim.stdout.is_empty(),
        "no structured output for a missing path"
    );
}
