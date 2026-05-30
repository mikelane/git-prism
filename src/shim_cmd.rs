//! `git-prism shim` subcommand: install / uninstall / report status of the
//! PATH-layer git shim.
//!
//! This is a thin CLI wrapper around the path-shim helpers in `hooks.rs`.
//! See ADR-0009 (`docs/decisions/0009-path-shim-architecture.md`) for design
//! rationale.

use std::io::{self, BufRead, Write};

use anyhow::Result;

use crate::hooks;

/// The export line fragment used to detect existing entries and compute the shim dir.
const SHIM_EXPORT_FRAGMENT: &str = ".local/share/git-prism/bin";

/// The full export line written to shell rc files.
const SHIM_EXPORT_LINE: &str = r#"export PATH="$HOME/.local/share/git-prism/bin:$PATH""#;

/// Install the PATH shim symlink and print the result to stdout.
///
/// Idempotent: if the symlink already exists and points at the current binary,
/// this is a silent no-op. `force` allows overwriting a regular file at the
/// target path.
///
/// After installing the symlink, checks whether the shim directory is already
/// in `PATH`. If not, prompts the user via stdin for consent to append the
/// export line to their shell rc file.
pub fn run_install(home: &std::path::Path, force: bool) -> Result<()> {
    run_install_with_io(home, force, &mut io::stdin().lock(), &mut io::stdout())
}

/// Testable install implementation with injected I/O.
pub fn run_install_with_io(
    home: &std::path::Path,
    force: bool,
    stdin: &mut dyn BufRead,
    stdout: &mut dyn Write,
) -> Result<()> {
    let symlink_path = hooks::install_path_shim(home, force)?;
    writeln!(stdout, "Created symlink: {}", symlink_path.display())?;

    let shim_dir = home.join(SHIM_EXPORT_FRAGMENT);
    let shim_dir_str = shim_dir.to_string_lossy().into_owned();

    if shim_dir_is_in_path(&shim_dir_str) {
        // Already on PATH — nothing to do.
        return Ok(());
    }

    offer_path_setup(home, stdin, stdout)?;
    Ok(())
}

/// Return true if the shim directory string appears in the current `PATH` env var.
fn shim_dir_is_in_path(shim_dir: &str) -> bool {
    let path_env = std::env::var("PATH").unwrap_or_default();
    path_env.split(':').any(|entry| entry == shim_dir)
}

/// Prompt for PATH consent and act on the answer.
fn offer_path_setup(
    home: &std::path::Path,
    stdin: &mut dyn BufRead,
    stdout: &mut dyn Write,
) -> Result<()> {
    writeln!(
        stdout,
        "\nThe shim directory is not in your PATH.\nAdd it automatically? [y/N] "
    )?;
    stdout.flush()?;

    let mut answer = String::new();
    stdin.read_line(&mut answer)?;

    if answer.trim().eq_ignore_ascii_case("y") {
        append_to_rc_idempotent(home, stdout)?;
    } else {
        print_manual_instructions(stdout)?;
    }
    Ok(())
}

/// Append the export line to the shell rc file, unless it is already present.
fn append_to_rc_idempotent(home: &std::path::Path, stdout: &mut dyn Write) -> Result<()> {
    let rc_path = detect_rc_file(home);
    let existing = if rc_path.exists() {
        std::fs::read_to_string(&rc_path)?
    } else {
        String::new()
    };

    if !existing.contains(SHIM_EXPORT_FRAGMENT) {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&rc_path)?;
        writeln!(file, "\n{SHIM_EXPORT_LINE}")?;
    }

    let rc_display = rc_path
        .strip_prefix(home)
        .map(|p| format!("~/{}", p.display()))
        .unwrap_or_else(|_| rc_path.display().to_string());

    writeln!(
        stdout,
        "Added to {rc_display}. Please restart Claude Code so the new PATH takes effect in its shell snapshot."
    )?;
    Ok(())
}

/// Print manual PATH instructions to stdout.
fn print_manual_instructions(stdout: &mut dyn Write) -> Result<()> {
    writeln!(
        stdout,
        "To complete setup, add this line to your shell rc manually:\n  {SHIM_EXPORT_LINE}"
    )?;
    Ok(())
}

/// Determine the shell rc file based on `$SHELL`, defaulting to `.zshrc`.
fn detect_rc_file(home: &std::path::Path) -> std::path::PathBuf {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let rc_name = if shell.contains("bash") {
        ".bashrc"
    } else {
        ".zshrc"
    };
    home.join(rc_name)
}

/// Remove the PATH shim symlink.
pub fn run_uninstall(home: &std::path::Path) -> Result<()> {
    hooks::uninstall_path_shim(home)?;
    println!("Removed git-prism shim.");
    Ok(())
}

/// Report whether the PATH shim is installed, not installed, or broken.
///
/// Output always includes the shim directory path so the user knows where
/// to add to `$PATH` regardless of install state.
pub fn run_status(home: &std::path::Path) -> Result<()> {
    let shim_dir = home.join(hooks::PATH_SHIM_REL_DIR);
    let shim_dir_str = shim_dir.to_string_lossy();
    match hooks::path_shim_status(home) {
        hooks::PathShimStatus::Installed { target } => {
            println!(
                "shim: installed at {} -> {}",
                shim_dir.join(hooks::PATH_SHIM_LINK_NAME).display(),
                target.display()
            );
            println!("shim directory: {shim_dir_str}");
        }
        hooks::PathShimStatus::NotInstalled => {
            println!("shim: not installed");
            println!("shim directory: {shim_dir_str}");
        }
        hooks::PathShimStatus::BrokenLink { reason } => {
            println!("shim: broken link ({reason})");
            println!("shim directory: {shim_dir_str}");
        }
    }
    Ok(())
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use tempfile::TempDir;

    fn install_with_consent(home: &std::path::Path) {
        let mut stdin = Cursor::new("y\n");
        let mut stdout = Vec::new();
        run_install_with_io(home, false, &mut stdin, &mut stdout).unwrap();
    }

    fn install_with_decline(home: &std::path::Path) {
        let mut stdin = Cursor::new("n\n");
        let mut stdout = Vec::new();
        run_install_with_io(home, false, &mut stdin, &mut stdout).unwrap();
    }

    #[test]
    fn run_install_creates_symlink_under_home() {
        let dir = TempDir::new().unwrap();
        install_with_decline(dir.path());
        let link = dir.path().join(".local/share/git-prism/bin/git");
        assert!(link.is_symlink(), "symlink must exist after shim install");
    }

    #[test]
    fn run_install_is_idempotent() {
        let dir = TempDir::new().unwrap();
        install_with_decline(dir.path());
        install_with_decline(dir.path());
        let link = dir.path().join(".local/share/git-prism/bin/git");
        assert!(
            link.is_symlink(),
            "symlink must remain after second install"
        );
    }

    #[test]
    fn consent_appends_export_line_to_rc_file() {
        let dir = TempDir::new().unwrap();
        let rc = dir.path().join(".zshrc");
        std::fs::write(&rc, "# shell rc\n").unwrap();

        install_with_consent(dir.path());

        let content = std::fs::read_to_string(&rc).unwrap();
        assert!(
            content.contains(SHIM_EXPORT_FRAGMENT),
            "rc file must contain the export fragment after consent"
        );
    }

    #[test]
    fn consent_appends_export_line_exactly_once_on_repeat() {
        let dir = TempDir::new().unwrap();
        let rc = dir.path().join(".zshrc");
        std::fs::write(&rc, "# shell rc\n").unwrap();

        install_with_consent(dir.path());
        install_with_consent(dir.path());

        let content = std::fs::read_to_string(&rc).unwrap();
        let count = content.matches(SHIM_EXPORT_FRAGMENT).count();
        assert_eq!(count, 1, "export fragment must appear exactly once");
    }

    #[test]
    fn consent_output_contains_restart() {
        let dir = TempDir::new().unwrap();
        let rc = dir.path().join(".zshrc");
        std::fs::write(&rc, "# shell rc\n").unwrap();

        let mut stdin = Cursor::new("y\n");
        let mut stdout = Vec::new();
        run_install_with_io(dir.path(), false, &mut stdin, &mut stdout).unwrap();

        let out = String::from_utf8(stdout).unwrap();
        assert!(
            out.contains("restart"),
            "output must tell user to restart Claude Code; got: {out:?}"
        );
    }

    #[test]
    fn decline_leaves_rc_file_unchanged() {
        let dir = TempDir::new().unwrap();
        let rc = dir.path().join(".zshrc");
        std::fs::write(&rc, "# shell rc\n").unwrap();
        let original = std::fs::read_to_string(&rc).unwrap();

        install_with_decline(dir.path());

        let after = std::fs::read_to_string(&rc).unwrap();
        assert_eq!(original, after, "rc file must be unchanged after decline");
    }

    #[test]
    fn decline_output_contains_shim_dir_path() {
        let dir = TempDir::new().unwrap();
        let mut stdin = Cursor::new("n\n");
        let mut stdout = Vec::new();
        run_install_with_io(dir.path(), false, &mut stdin, &mut stdout).unwrap();

        let out = String::from_utf8(stdout).unwrap();
        assert!(
            out.contains(SHIM_EXPORT_FRAGMENT),
            "output must contain the shim dir path; got: {out:?}"
        );
    }

    #[test]
    fn run_uninstall_removes_symlink() {
        let dir = TempDir::new().unwrap();
        install_with_decline(dir.path());
        run_uninstall(dir.path()).unwrap();
        let link = dir.path().join(".local/share/git-prism/bin/git");
        assert!(!link.is_symlink() && !link.exists(), "symlink must be gone");
    }

    #[test]
    fn run_status_reports_not_installed_when_absent() {
        let dir = TempDir::new().unwrap();
        run_status(dir.path()).unwrap();
    }

    #[test]
    fn run_status_succeeds_after_install() {
        let dir = TempDir::new().unwrap();
        install_with_decline(dir.path());
        run_status(dir.path()).unwrap();
    }
}
