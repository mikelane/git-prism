#![cfg(unix)]

//! Issue #387: a program that execs `git` (an IDE, lefthook, cargo, an agent's
//! own subprocess) must see byte-for-byte real git even when a stale
//! `git` -> git-prism symlink is first on PATH. Interception is reachable only
//! through `git-prism shim-run git ...`.

use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const REAL_GIT: &str = "/usr/bin/git";

struct Fixture {
    _tmp: TempDir,
    repo: PathBuf,
    shim_dir: PathBuf,
    first_commit: String,
    second_commit: String,
}

fn git_in(repo: &Path, args: &[&str]) -> String {
    let out = Command::new(REAL_GIT)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success(), "setup git {args:?} failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn fixture() -> Fixture {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let shim_dir = tmp.path().join("shimbin");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&shim_dir).unwrap();
    symlink(env!("CARGO_BIN_EXE_git-prism"), shim_dir.join("git")).unwrap();

    git_in(&repo, &["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    git_in(&repo, &["add", "a.txt"]);
    git_in(&repo, &["commit", "-q", "-m", "first"]);
    let first_commit = git_in(&repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\nfour\nfive\n").unwrap();
    git_in(&repo, &["mv", "a.txt", "b.txt"]);
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-q", "-m", "second"]);
    let second_commit = git_in(&repo, &["rev-parse", "HEAD"]);

    Fixture {
        _tmp: tmp,
        repo,
        shim_dir,
        first_commit,
        second_commit,
    }
}

fn agent_command(program: impl AsRef<std::ffi::OsStr>, path: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_remove("CI")
        .env_remove("GIT_PRISM_PASSTHROUGH")
        .env_remove("GIT_PRISM_DISABLE")
        .env_remove("GIT_PRISM_INSIDE_SHIM")
        .env("AI_AGENT", "claude-code")
        .env("PATH", path);
    cmd
}

fn assert_program_gets_real_git(args: &[&str], expected_exit: i32) {
    let f = fixture();
    let path = format!("{}:/usr/bin:/bin", f.shim_dir.display());
    let args: Vec<String> = args
        .iter()
        .map(|a| {
            a.replace("{A}", &f.first_commit)
                .replace("{B}", &f.second_commit)
        })
        .collect();

    let real = Command::new(REAL_GIT)
        .args(&args)
        .current_dir(&f.repo)
        .output()
        .unwrap();
    let via_symlink = agent_command(f.shim_dir.join("git"), &path)
        .args(&args)
        .current_dir(&f.repo)
        .output()
        .unwrap();

    assert_eq!(real.status.code(), Some(expected_exit), "reference exit");
    assert_eq!(via_symlink.status.code(), real.status.code(), "exit code");
    assert_eq!(via_symlink.stdout, real.stdout, "stdout bytes");
}

#[test]
fn it_gives_programs_real_git_for_rename_diff() {
    assert_program_gets_real_git(
        &["diff", "-M", "--no-color", "--no-ext-diff", "{A}..{B}"],
        0,
    );
}

#[test]
fn it_gives_programs_real_git_for_show_rev_path() {
    assert_program_gets_real_git(&["show", "{B}:b.txt"], 0);
}

#[test]
fn it_gives_programs_real_git_for_show_rev_root_tree() {
    assert_program_gets_real_git(&["show", "{B}:"], 0);
}

#[test]
fn it_gives_programs_real_git_for_show_index_path() {
    assert_program_gets_real_git(&["show", ":b.txt"], 0);
}

#[test]
fn it_gives_programs_real_git_for_show_peeled_tree() {
    assert_program_gets_real_git(&["show", "{B}^{tree}"], 0);
}

#[test]
fn it_gives_programs_real_git_for_log_range() {
    assert_program_gets_real_git(&["log", "{A}..{B}"], 0);
}

#[test]
fn it_gives_programs_real_git_error_for_unknown_ref() {
    assert_program_gets_real_git(&["diff", "nosuchref..{B}"], 128);
}

#[test]
fn it_gives_programs_real_git_error_for_missing_blob() {
    assert_program_gets_real_git(&["show", "{A}:missing.txt"], 128);
}

#[test]
fn it_intercepts_via_shim_run() {
    let f = fixture();
    let path = format!("{}:/usr/bin:/bin", f.shim_dir.display());
    let range = format!("{}..{}", f.first_commit, f.second_commit);

    let out = agent_command(env!("CARGO_BIN_EXE_git-prism"), &path)
        .args(["shim-run", "git", "diff", &range])
        .current_dir(&f.repo)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let files = json["files"].as_array().expect("files array");
    // Second commit renamed a.txt to b.txt; the manifest must name the new path
    // and carry the structured-output hint.
    assert!(
        files.iter().any(|file| file["path"] == "b.txt"),
        "expected b.txt in files: {json}"
    );
    assert!(
        json["git_prism_hint"]
            .as_str()
            .is_some_and(|hint| hint.ends_with(&format!("command git diff {range}"))),
        "expected the raw git hint: {json}"
    );
}

#[test]
fn it_rejects_shim_run_for_programs_other_than_git() {
    let out = Command::new(env!("CARGO_BIN_EXE_git-prism"))
        .args(["shim-run", "ls"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage"));
}

// --- Two git-prism shims must never exec each other -------------------------

/// A copy of the binary under another path stands in for a second installed
/// git-prism (for example 0.10.0 from `~/.local/share/git-prism/bin`).
fn second_install(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let copy = dir.join("git-prism");
    std::fs::copy(env!("CARGO_BIN_EXE_git-prism"), &copy).unwrap();
    symlink(&copy, dir.join("git")).unwrap();
    symlink(&copy, dir.join("gh")).unwrap();
}

fn first_install(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    symlink(env!("CARGO_BIN_EXE_git-prism"), dir.join("git")).unwrap();
    symlink(env!("CARGO_BIN_EXE_git-prism"), dir.join("gh")).unwrap();
}

fn run_with_timeout(mut cmd: Command, limit: Duration) -> Option<Output> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return Some(child.wait_with_output().unwrap());
        }
        if start.elapsed() > limit {
            child.kill().unwrap();
            child.wait().unwrap();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn it_terminates_when_two_git_prism_shim_dirs_precede_real_git() {
    let tmp = TempDir::new().unwrap();
    let (one, two) = (tmp.path().join("one"), tmp.path().join("two"));
    first_install(&one);
    second_install(&two);
    let path = format!("{}:{}:/usr/bin:/bin", one.display(), two.display());

    let mut cmd = agent_command(one.join("git"), &path);
    cmd.arg("--version");
    let out = run_with_timeout(cmd, Duration::from_secs(10))
        .expect("git --version looped between two git-prism shims");

    assert!(String::from_utf8_lossy(&out.stdout).starts_with("git version"));
}

#[test]
fn it_runs_the_real_gh_when_two_git_prism_shim_dirs_precede_it() {
    let tmp = TempDir::new().unwrap();
    let (one, two, real) = (
        tmp.path().join("one"),
        tmp.path().join("two"),
        tmp.path().join("real"),
    );
    first_install(&one);
    second_install(&two);
    std::fs::create_dir_all(&real).unwrap();
    let stub = real.join("gh");
    std::fs::write(&stub, "#!/bin/sh\necho REAL-GH-STUB\n").unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}:{}:/usr/bin:/bin",
        one.display(),
        two.display(),
        real.display()
    );

    let mut cmd = agent_command(one.join("gh"), &path);
    cmd.arg("--version");
    let out = run_with_timeout(cmd, Duration::from_secs(10))
        .expect("gh --version looped between two git-prism shims");

    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "REAL-GH-STUB");
}
