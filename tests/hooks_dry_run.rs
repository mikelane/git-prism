#![cfg(unix)]

//! Regression test for issue #326, reworked for #387: `git-prism hooks install
//! --path-shim --dry-run` must honor --dry-run and NOT write the rc function
//! block; without --dry-run the deprecated alias installs the function block
//! and prints a deprecation warning.
//!
//! This drives the built binary end-to-end with an isolated HOME.

use std::process::Command;
use tempfile::TempDir;

fn hooks_install_path_shim(home: &TempDir, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_git-prism"))
        .env("HOME", home.path())
        .env("SHELL", "/bin/zsh")
        .args(["hooks", "install", "--path-shim"])
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn it_does_not_write_the_rc_block_for_a_path_shim_dry_run() {
    let home = TempDir::new().unwrap();

    let out = hooks_install_path_shim(&home, &["--dry-run"]);

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("dry-run"), "stdout: {stdout}");
    assert!(
        !home.path().join(".zshrc").exists(),
        ".zshrc must not be created by --dry-run"
    );
}

#[test]
fn it_writes_the_function_block_and_warns_for_the_deprecated_path_shim_alias() {
    let home = TempDir::new().unwrap();

    let out = hooks_install_path_shim(&home, &[]);

    assert_eq!(out.status.code(), Some(0));
    let zshrc = std::fs::read_to_string(home.path().join(".zshrc")).unwrap();
    assert!(zshrc.contains("function git {"), "zshrc:\n{zshrc}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("deprecated"), "stderr: {stderr}");
    assert!(
        !home.path().join(".local/share/git-prism/bin/git").exists(),
        "no symlink may be created"
    );
}
