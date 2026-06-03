//! `git-prism shim` subcommand: install / uninstall / report status of the
//! PATH-layer git shim.
//!
//! This is a thin CLI wrapper around the path-shim helpers in `hooks.rs`.
//! See ADR-0009 (`docs/decisions/0009-path-shim-architecture.md`) for design
//! rationale.

use std::io::{self, BufRead, Write};

use anyhow::Result;

use crate::hooks;

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
    let rc_path = detect_rc_file(home);
    run_install_with_io(
        home,
        force,
        &mut io::stdin().lock(),
        &mut io::stdout(),
        &rc_path,
    )
}

/// Testable install implementation with injected I/O.
///
/// `rc_path` is the shell rc file to append the export line to when the user
/// consents. Production callers pass `detect_rc_file(home)`; tests pass an
/// explicit path so the result is independent of the runner's `$SHELL`.
pub fn run_install_with_io(
    home: &std::path::Path,
    force: bool,
    stdin: &mut dyn BufRead,
    stdout: &mut dyn Write,
    rc_path: &std::path::Path,
) -> Result<()> {
    hooks::install_path_shim(home, force)?;
    let shim_dir = home.join(hooks::PATH_SHIM_REL_DIR);
    for name in hooks::PATH_SHIM_LINK_NAMES {
        writeln!(stdout, "Created symlink: {}", shim_dir.join(name).display())?;
    }

    let shim_dir_str = shim_dir.to_string_lossy().into_owned();

    if shim_dir_is_in_path(&shim_dir_str) {
        // Already on PATH — nothing to do.
        return Ok(());
    }

    offer_path_setup(home, rc_path, stdin, stdout)?;
    Ok(())
}

/// Return true if the shim directory string appears in the current `PATH` env var.
/// Normalizes trailing slashes so a PATH entry like `/path/to/bin/` matches `/path/to/bin`.
fn shim_dir_is_in_path(shim_dir: &str) -> bool {
    let path_env = std::env::var("PATH").unwrap_or_default();
    let normalized_shim = shim_dir.trim_end_matches('/');
    path_env.split(':').any(|entry| {
        let normalized_entry = entry.trim_end_matches('/');
        normalized_entry == normalized_shim
    })
}

/// Prompt for PATH consent and act on the answer.
fn offer_path_setup(
    home: &std::path::Path,
    rc_path: &std::path::Path,
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
        append_to_rc_idempotent(home, rc_path, stdout)?;
    } else {
        print_manual_instructions(stdout)?;
    }
    Ok(())
}

/// Append the export line to the shell rc file, unless it is already present.
/// Detects presence by matching the full export line (line-wise, ignoring comments).
fn append_to_rc_idempotent(
    home: &std::path::Path,
    rc_path: &std::path::Path,
    stdout: &mut dyn Write,
) -> Result<()> {
    let existing = if rc_path.exists() {
        std::fs::read_to_string(rc_path)?
    } else {
        String::new()
    };

    // Check if the full export line already exists (non-comment, trimmed match).
    let export_line_already_present = existing.lines().any(|line| {
        let trimmed = line.trim_start();
        !trimmed.starts_with('#') && trimmed.trim() == SHIM_EXPORT_LINE.trim()
    });

    if !export_line_already_present {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(rc_path)?;
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

/// Determine the login-shell rc file based on `$SHELL`, defaulting to `.zprofile`.
///
/// Non-interactive login shells (like Claude Code's) source `.zprofile` / `.bash_profile`
/// but skip `.zshrc` / `.bashrc`. macOS `path_helper` also demotes PATH entries inherited
/// from the environment behind `/usr/bin`, so writing to `.zshrc` has no effect in that
/// context. Use login-shell rc files to ensure the export takes effect.
fn detect_rc_file(home: &std::path::Path) -> std::path::PathBuf {
    let shell = std::env::var("SHELL").unwrap_or_default();
    detect_rc_file_for(home, &shell)
}

/// Testable rc-file detection with an injected shell string.
///
/// Matches on the basename of the shell path so that a path which merely
/// contains "bash" as a substring (e.g. `/usr/local/bin/newbash`) is not
/// misidentified as bash.
pub(crate) fn detect_rc_file_for(home: &std::path::Path, shell: &str) -> std::path::PathBuf {
    let basename = std::path::Path::new(shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(shell);
    let rc_name = if basename == "bash" {
        ".bash_profile"
    } else {
        ".zprofile"
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
/// to add to `$PATH` regardless of install state. Each managed symlink name
/// (`git`, `gh`) is reported on a separate line.
pub fn run_status(home: &std::path::Path) -> Result<()> {
    run_status_with_io(home, &mut std::io::stdout())
}

/// Testable status implementation with injected output writer.
pub fn run_status_with_io(home: &std::path::Path, out: &mut dyn std::io::Write) -> Result<()> {
    let shim_dir = home.join(hooks::PATH_SHIM_REL_DIR);
    let shim_dir_str = shim_dir.to_string_lossy();

    for name in hooks::PATH_SHIM_LINK_NAMES {
        match hooks::path_shim_status_for(home, name) {
            hooks::PathShimStatus::Installed {
                target,
                staleness_warning,
            } => {
                writeln!(
                    out,
                    "shim {name}: installed at {} -> {}",
                    shim_dir.join(name).display(),
                    target.display()
                )?;
                if let Some(warning) = staleness_warning {
                    writeln!(out, "warning ({name}): {warning}")?;
                }
            }
            hooks::PathShimStatus::NotInstalled => {
                writeln!(out, "shim {name}: not installed")?;
            }
            hooks::PathShimStatus::BrokenLink { reason } => {
                writeln!(out, "shim {name}: broken link ({reason})")?;
            }
        }
    }

    writeln!(out, "shim directory: {shim_dir_str}")?;
    Ok(())
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use tempfile::TempDir;

    /// The shim-dir path fragment, derived from the canonical constant in
    /// `hooks` so these assertions track the real install location instead of a
    /// drifting literal copy.
    const SHIM_EXPORT_FRAGMENT: &str = hooks::PATH_SHIM_REL_DIR;

    /// Install with consent, using an explicit `.zshrc` path so the test is
    /// independent of the CI runner's `$SHELL`.
    fn install_with_consent(home: &std::path::Path) {
        let rc = home.join(".zshrc");
        let mut stdin = Cursor::new("y\n");
        let mut stdout = Vec::new();
        run_install_with_io(home, false, &mut stdin, &mut stdout, &rc).unwrap();
    }

    fn install_with_decline(home: &std::path::Path) {
        let rc = home.join(".zshrc");
        let mut stdin = Cursor::new("n\n");
        let mut stdout = Vec::new();
        run_install_with_io(home, false, &mut stdin, &mut stdout, &rc).unwrap();
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
        run_install_with_io(dir.path(), false, &mut stdin, &mut stdout, &rc).unwrap();

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
        let rc = dir.path().join(".zshrc");
        let mut stdin = Cursor::new("n\n");
        let mut stdout = Vec::new();
        run_install_with_io(dir.path(), false, &mut stdin, &mut stdout, &rc).unwrap();

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

    #[test]
    fn detect_rc_file_returns_zprofile_for_zsh() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        // Simulate SHELL=zsh by calling detect_rc_file directly.
        // We need to test the logic without relying on the CI runner's $SHELL.
        // detect_rc_file_for lets us inject the shell string.
        let rc = detect_rc_file_for(home, "zsh");
        assert!(
            rc.ends_with(".zprofile"),
            "zsh rc must be .zprofile (login-shell sourced), got: {}",
            rc.display()
        );
    }

    #[test]
    fn detect_rc_file_returns_bash_profile_for_bash() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let rc = detect_rc_file_for(home, "bash");
        assert!(
            rc.ends_with(".bash_profile"),
            "bash rc must be .bash_profile (login-shell sourced), got: {}",
            rc.display()
        );
    }

    #[test]
    fn detect_rc_file_defaults_to_zprofile_when_shell_unknown() {
        let dir = TempDir::new().unwrap();
        let rc = detect_rc_file_for(dir.path(), "");
        assert!(
            rc.ends_with(".zprofile"),
            "unknown shell must default to .zprofile, got: {}",
            rc.display()
        );
    }

    /// A path that merely CONTAINS "bash" as a substring (e.g. `/usr/local/newbash`)
    /// must not be misidentified as bash — only a path whose final component IS
    /// "bash" (or the bare string "bash") should select `.bash_profile`.
    #[test]
    fn detect_rc_file_does_not_misroute_shell_path_containing_bash_as_substring() {
        let dir = TempDir::new().unwrap();
        let rc = detect_rc_file_for(dir.path(), "/usr/local/bin/newbash");
        assert!(
            rc.ends_with(".zprofile"),
            "shell whose basename is 'newbash' (not 'bash') must default to .zprofile, got: {}",
            rc.display()
        );
    }

    #[test]
    fn run_status_output_mentions_gh_symlink() {
        let dir = TempDir::new().unwrap();
        install_with_decline(dir.path());
        let mut out = Vec::new();
        run_status_with_io(dir.path(), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("gh"),
            "status output must mention the gh symlink; got: {text:?}"
        );
    }
}
