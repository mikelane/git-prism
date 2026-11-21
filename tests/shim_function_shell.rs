#![cfg(unix)]

//! Issue #387: the rc function installed by `git-prism shim install` must
//! intercept only what the agent types into its shell, fall back to real git
//! when `git-prism` is not on PATH, and survive a user's `alias git=...`.
//!
//! Each scenario installs the real block into a temp HOME, then sources the
//! resulting rc file in a fresh shell. Scenarios are skipped (with a printed
//! reason) when the shell is not installed.

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

const REAL_GIT: &str = "/usr/bin/git";

#[derive(Clone, Copy)]
enum Shell {
    Zsh,
    Bash,
}

impl Shell {
    fn program(self) -> &'static str {
        match self {
            Shell::Zsh => "zsh",
            Shell::Bash => "bash",
        }
    }

    fn rc_file(self) -> &'static str {
        match self {
            Shell::Zsh => ".zshrc",
            Shell::Bash => ".bashrc",
        }
    }

    fn is_available(self) -> bool {
        Command::new(self.program())
            .args(["-c", "exit 0"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

struct Fixture {
    _tmp: TempDir,
    home: PathBuf,
    repo: PathBuf,
    bin_with_git_prism: PathBuf,
    range: String,
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

fn install_block(home: &Path, shell: Shell) {
    let out = Command::new(env!("CARGO_BIN_EXE_git-prism"))
        .env("HOME", home)
        .env("SHELL", format!("/bin/{}", shell.program()))
        .args(["shim", "install"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fixture(shell: Shell) -> Fixture {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    let repo = tmp.path().join("repo");
    let bin = tmp.path().join("bin");
    for dir in [&home, &repo, &bin] {
        std::fs::create_dir_all(dir).unwrap();
    }
    symlink(env!("CARGO_BIN_EXE_git-prism"), bin.join("git-prism")).unwrap();

    git_in(&repo, &["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    git_in(&repo, &["add", "a.txt"]);
    git_in(&repo, &["commit", "-q", "-m", "first"]);
    let first = git_in(&repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    git_in(&repo, &["commit", "-q", "-am", "second"]);
    let second = git_in(&repo, &["rev-parse", "HEAD"]);

    install_block(&home, shell);
    Fixture {
        _tmp: tmp,
        home,
        repo,
        bin_with_git_prism: bin,
        range: format!("{first}..{second}"),
    }
}

/// Run `script` in a fresh `shell` as an agent, with the rc file already
/// sourced by the script's first line.
fn run_in_shell(f: &Fixture, shell: Shell, path: &str, script: &str) -> Output {
    let full = format!("shopt -s expand_aliases 2>/dev/null; {script}").replace(
        "SOURCE_RC",
        &format!("source \"$HOME/{}\"", shell.rc_file()),
    );
    Command::new(shell.program())
        .args(["-c", &full])
        .current_dir(&f.repo)
        .env_remove("CI")
        .env_remove("GIT_PRISM_PASSTHROUGH")
        .env_remove("GIT_PRISM_DISABLE")
        .env_remove("GIT_PRISM_INSIDE_SHIM")
        .env("AI_AGENT", "claude-code")
        .env("HOME", &f.home)
        .env("PATH", path)
        .output()
        .unwrap()
}

fn path_with_git_prism(f: &Fixture) -> String {
    format!("{}:/usr/bin:/bin", f.bin_with_git_prism.display())
}

fn real_git(f: &Fixture, args: &[&str]) -> Output {
    Command::new(REAL_GIT)
        .args(args)
        .current_dir(&f.repo)
        .output()
        .unwrap()
}

fn skip_unless_available(shell: Shell) -> bool {
    let available = shell.is_available();
    if !available {
        eprintln!("skipping: {} is not installed", shell.program());
    }
    !available
}

fn typed_git_diff_is_structured_json(shell: Shell) {
    if skip_unless_available(shell) {
        return;
    }
    let f = fixture(shell);
    let script = format!("SOURCE_RC; git diff {}", f.range);

    let out = run_in_shell(&f, shell, &path_with_git_prism(&f), &script);

    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    assert_eq!(json["files"][0]["path"], "a.txt", "files: {json}");
    assert_eq!(json["files"][0]["lines_added"], 1, "files: {json}");
}

fn a_child_process_gets_real_git_bytes(shell: Shell) {
    if skip_unless_available(shell) {
        return;
    }
    let f = fixture(shell);
    let script = format!("SOURCE_RC; sh -c 'git diff {}'", f.range);

    let out = run_in_shell(&f, shell, &path_with_git_prism(&f), &script);

    let expected = real_git(&f, &["diff", &f.range]);
    assert_eq!(out.stdout, expected.stdout);
}

fn redirecting_stdout_to_a_file_writes_real_git_bytes(shell: Shell) {
    if skip_unless_available(shell) {
        return;
    }
    let f = fixture(shell);
    let script = format!("SOURCE_RC; git diff {} > out.patch", f.range);

    let out = run_in_shell(&f, shell, &path_with_git_prism(&f), &script);

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let written = std::fs::read(f.repo.join("out.patch")).unwrap();
    assert_eq!(written, real_git(&f, &["diff", &f.range]).stdout);
}

fn a_missing_git_prism_falls_back_to_real_git(shell: Shell) {
    if skip_unless_available(shell) {
        return;
    }
    let f = fixture(shell);

    let out = run_in_shell(&f, shell, "/usr/bin:/bin", "SOURCE_RC; git rev-parse HEAD");

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, real_git(&f, &["rev-parse", "HEAD"]).stdout);
}

fn a_user_alias_for_git_does_not_break_the_block(shell: Shell) {
    if skip_unless_available(shell) {
        return;
    }
    let f = fixture(shell);

    let out = run_in_shell(
        &f,
        shell,
        &path_with_git_prism(&f),
        "alias git=echo; SOURCE_RC; unalias git; type git",
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("function"), "type git said: {stdout}");
}

macro_rules! scenario_per_shell {
    ($name:ident, $scenario:ident) => {
        mod $name {
            #[test]
            fn it_works_in_zsh() {
                super::$scenario(super::Shell::Zsh);
            }

            #[test]
            fn it_works_in_bash() {
                super::$scenario(super::Shell::Bash);
            }
        }
    };
}

scenario_per_shell!(typed_git_diff, typed_git_diff_is_structured_json);
scenario_per_shell!(child_process, a_child_process_gets_real_git_bytes);
scenario_per_shell!(
    redirected_stdout,
    redirecting_stdout_to_a_file_writes_real_git_bytes
);
scenario_per_shell!(
    missing_git_prism,
    a_missing_git_prism_falls_back_to_real_git
);
scenario_per_shell!(user_alias, a_user_alias_for_git_does_not_break_the_block);
