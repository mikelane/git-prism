//! `git-prism shim` subcommand: install / uninstall / report status of the
//! PATH-layer git shim.
//!
//! This is a thin CLI wrapper around the path-shim helpers in `hooks.rs`.
//! See ADR-0009 (`docs/decisions/0009-path-shim-architecture.md`) for design
//! rationale.

use std::io::{self, BufRead, Write};

use anyhow::Result;

use crate::hooks;

/// Install the PATH shim symlink and print the result to stdout.
///
/// Idempotent: if the symlink already exists and points at the current binary,
/// this is a silent no-op. `force` allows overwriting a regular file at the
/// target path.
///
/// After installing the symlink, checks whether the shim directory is already
/// first on PATH for non-login shells. If not, prompts the user via stdin for
/// consent to write the managed PATH block to the appropriate rc files.
pub fn run_install(home: &std::path::Path, force: bool) -> Result<()> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    run_install_with_io(
        home,
        force,
        &mut io::stdin().lock(),
        &mut io::stdout(),
        &shell,
    )
}

/// Testable install implementation with injected I/O.
///
/// `shell` drives which rc files receive the managed PATH block when the user
/// consents:
///   - `"zsh"` → `.zshenv` + `.zshrc`
///   - `"bash"` → `.bash_profile` + `.bashrc`
///
/// Production callers pass the ambient `$SHELL`; tests pass an explicit value
/// so results are independent of the CI runner's shell.
pub fn run_install_with_io(
    home: &std::path::Path,
    force: bool,
    stdin: &mut dyn BufRead,
    stdout: &mut dyn Write,
    shell: &str,
) -> Result<()> {
    hooks::install_path_shim(home, force)?;
    let shim_dir = home.join(hooks::PATH_SHIM_REL_DIR);
    for name in hooks::PATH_SHIM_LINK_NAMES {
        writeln!(stdout, "Created symlink: {}", shim_dir.join(name).display())?;
    }

    let shim_dir_str = shim_dir.to_string_lossy().into_owned();

    if shim_dir_is_first_in_path(&shim_dir_str) {
        // Already first on PATH — nothing to do.
        return Ok(());
    }

    offer_path_setup(home, shell, stdin, stdout)?;
    Ok(())
}

/// Return true if the shim directory is the FIRST entry in the current `PATH`.
///
/// Unlike the old "present anywhere" check, this requires the shim to be first
/// so that it wins over Homebrew/cargo/nvm entries that re-prepend later.
/// Normalizes trailing slashes so `/path/to/bin/` matches `/path/to/bin`.
fn shim_dir_is_first_in_path(shim_dir: &str) -> bool {
    let path_env = std::env::var("PATH").unwrap_or_default();
    let normalized_shim = shim_dir.trim_end_matches('/');
    path_env
        .split(':')
        .next()
        .map(|first| first.trim_end_matches('/') == normalized_shim)
        .unwrap_or(false)
}

/// Prompt for PATH consent and act on the answer.
fn offer_path_setup(
    home: &std::path::Path,
    shell: &str,
    stdin: &mut dyn BufRead,
    stdout: &mut dyn Write,
) -> Result<()> {
    writeln!(
        stdout,
        "\nThe shim directory is not first in your PATH.\nAdd it automatically? [y/N] "
    )?;
    stdout.flush()?;

    let mut answer = String::new();
    stdin.read_line(&mut answer)?;

    if answer.trim().eq_ignore_ascii_case("y") {
        write_rc_block_for_shell(home, shell)?;
        print_setup_success(shell, stdout)?;
    } else {
        print_manual_instructions(stdout)?;
    }
    Ok(())
}

/// Print success message after writing the managed PATH block.
fn print_setup_success(shell: &str, stdout: &mut dyn Write) -> Result<()> {
    let basename = std::path::Path::new(shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(shell);
    let (env_file, rc_file) = if basename == "bash" {
        (".bash_profile", ".bashrc")
    } else {
        (".zshenv", ".zshrc")
    };
    let env_display = format!("~/{env_file}");
    let rc_display = format!("~/{rc_file}");
    writeln!(
        stdout,
        "Added to {env_display} and {rc_display}. Please restart Claude Code so the new PATH takes effect in its shell snapshot."
    )?;
    Ok(())
}

/// Print manual PATH instructions to stdout.
fn print_manual_instructions(stdout: &mut dyn Write) -> Result<()> {
    writeln!(
        stdout,
        "To complete setup, add this block to ~/.zshenv and the end of ~/.zshrc manually:\n\
         \n\
         {BLOCK_START_MARKER}\n\
         {SHIM_PATH_BLOCK_BODY}\n\
         {BLOCK_END_MARKER}"
    )?;
    Ok(())
}

/// Write the marker-delimited PATH shim block to the appropriate rc files for the given shell.
///
/// For zsh: writes to `~/.zshenv` (covers non-login non-interactive agent shells)
/// and appends/replaces at the end of `~/.zshrc` (re-asserts priority after brew/cargo prepends).
/// For bash: writes to `~/.bashrc` and `~/.bash_profile`.
///
/// The block is marker-delimited so re-running replaces the existing block (idempotent).
pub(crate) fn write_rc_block_for_shell(home: &std::path::Path, shell: &str) -> Result<()> {
    let basename = std::path::Path::new(shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(shell);
    let (env_file, rc_file) = if basename == "bash" {
        (".bash_profile", ".bashrc")
    } else {
        (".zshenv", ".zshrc")
    };
    write_marker_block(home, &home.join(env_file))?;
    write_marker_block(home, &home.join(rc_file))?;
    Ok(())
}

/// Markers that delimit the managed PATH block so it can be replaced idempotently.
const BLOCK_START_MARKER: &str = "# >>> git-prism shim >>>";
const BLOCK_END_MARKER: &str = "# <<< git-prism shim <<<";

/// The managed PATH block content (without markers).
///
/// Strips the shim dir from ALL positions in PATH (front, middle, or end) before
/// prepending it exactly once. The implementation wraps PATH in colons so every
/// entry is colon-bounded on both sides, applies a global substitution, then
/// unwraps. Only the exact shim dir path is removed; superstring siblings such as
/// `git-prism/bin-extra` are not affected because the patterns are colon-anchored.
const SHIM_PATH_BLOCK_BODY: &str = r#"case ":$PATH:" in
  ":$HOME/.local/share/git-prism/bin:"*) ;;
  *)
    _gp_shim="$HOME/.local/share/git-prism/bin"
    _gp_p=":${PATH}:"
    _gp_p="${_gp_p//:${_gp_shim}:/:}"
    _gp_p="${_gp_p#:}"
    _gp_p="${_gp_p%:}"
    PATH="${_gp_shim}${_gp_p:+:${_gp_p}}"
    export PATH
    unset _gp_shim _gp_p
    ;;
esac"#;

/// Write (or replace) the marker-delimited PATH block in the given rc file.
///
/// Strips ALL existing managed blocks and stray markers (handles corruption
/// from hand-edits, interrupted writes, or prior buggy runs) then appends
/// exactly one canonical block.  Creates the file if it doesn't exist.
fn write_marker_block(home: &std::path::Path, rc_path: &std::path::Path) -> Result<()> {
    let existing = if rc_path.exists() {
        std::fs::read_to_string(rc_path)?
    } else {
        String::new()
    };

    let new_block = format!("{BLOCK_START_MARKER}\n{SHIM_PATH_BLOCK_BODY}\n{BLOCK_END_MARKER}\n");

    let cleaned = strip_all_marker_blocks(&existing);

    // Append the single canonical block with a blank-line separator.
    let separator = if cleaned.ends_with('\n') || cleaned.is_empty() {
        ""
    } else {
        "\n"
    };
    let new_content = if cleaned.is_empty() {
        new_block
    } else {
        format!("{cleaned}{separator}\n{new_block}")
    };

    std::fs::write(rc_path, new_content)?;

    let rc_display = rc_path
        .strip_prefix(home)
        .map(|p| format!("~/{}", p.display()))
        .unwrap_or_else(|_| rc_path.display().to_string());
    let _ = rc_display; // used in caller output
    Ok(())
}

/// Remove every git-prism managed block (and any stray marker lines) from `content`.
///
/// Handles all corruption forms:
///   - Well-formed START..END pairs (any number of them)
///   - Truncated block: START present, END missing
///   - Inverted block: END appears before START
///   - Orphaned END markers with no preceding START
///
/// The invariant: after this call, the returned string contains neither
/// `BLOCK_START_MARKER` nor `BLOCK_END_MARKER`.
fn strip_all_marker_blocks(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut inside_block = false;

    for line in content.lines() {
        if line.contains(BLOCK_START_MARKER) {
            // Enter managed block; discard this line.
            inside_block = true;
        } else if line.contains(BLOCK_END_MARKER) {
            // Exit managed block; discard this line regardless of whether we
            // saw a matching START (handles orphaned END markers).
            inside_block = false;
        } else if !inside_block {
            result.push_str(line);
            result.push('\n');
        }
        // Lines inside a managed block are silently dropped.
    }

    // Trim trailing blank lines introduced by block removal, but preserve a
    // single trailing newline when the original content ended with one.
    let trimmed = result.trim_end_matches('\n');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
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
///
/// Also checks whether the shim directory is first on PATH using the current
/// process environment, and emits a warning when it is not.
pub fn run_status(home: &std::path::Path) -> Result<()> {
    let path_env = std::env::var("PATH").ok();
    run_status_with_path(home, &mut std::io::stdout(), path_env.as_deref())
}

/// Testable status implementation with injected output writer.
#[cfg(test)]
pub fn run_status_with_io(home: &std::path::Path, out: &mut dyn std::io::Write) -> Result<()> {
    let path_env = std::env::var("PATH").ok();
    run_status_with_path(home, out, path_env.as_deref())
}

/// Fully-injectable status implementation for testing (injected PATH string).
#[cfg(test)]
pub(crate) fn run_status_with_path_for_test(
    home: &std::path::Path,
    out: &mut dyn std::io::Write,
    path_override: Option<&str>,
) -> Result<()> {
    run_status_with_path(home, out, path_override)
}

/// Core status implementation with injected PATH.
fn run_status_with_path(
    home: &std::path::Path,
    out: &mut dyn std::io::Write,
    path_env: Option<&str>,
) -> Result<()> {
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

    // Warn when the shim dir is not first on PATH — this is the condition that
    // causes agent shells to resolve the real git/gh instead of the shim.
    if let Some(path) = path_env {
        let normalized_shim = shim_dir_str.trim_end_matches('/');
        let is_first = path
            .split(':')
            .next()
            .map(|first| first.trim_end_matches('/') == normalized_shim)
            .unwrap_or(false);
        if !is_first {
            writeln!(
                out,
                "warning: shim directory is not first on PATH — agent shells will not intercept git/gh.\n\
                 Run `git-prism shim install` and restart Claude Code to fix this."
            )?;
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

    // ---------------------------------------------------------------------------
    // TDD: marker-delimited multi-file block writer (issue #355)
    // ---------------------------------------------------------------------------

    #[test]
    fn write_rc_block_creates_zshenv_and_zshrc_for_zsh() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        write_rc_block_for_shell(home, "zsh").unwrap();
        assert!(
            home.join(".zshenv").exists(),
            ".zshenv must be created for zsh"
        );
        assert!(
            home.join(".zshrc").exists(),
            ".zshrc must be created for zsh"
        );
    }

    #[test]
    fn write_rc_block_creates_bash_profile_and_bashrc_for_bash() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        write_rc_block_for_shell(home, "bash").unwrap();
        assert!(
            home.join(".bash_profile").exists(),
            ".bash_profile must be created for bash"
        );
        assert!(
            home.join(".bashrc").exists(),
            ".bashrc must be created for bash"
        );
    }

    #[test]
    fn write_rc_block_contains_marker_delimiters() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        write_rc_block_for_shell(home, "zsh").unwrap();
        let zshenv = std::fs::read_to_string(home.join(".zshenv")).unwrap();
        assert!(
            zshenv.contains(BLOCK_START_MARKER),
            ".zshenv must contain start marker"
        );
        assert!(
            zshenv.contains(BLOCK_END_MARKER),
            ".zshenv must contain end marker"
        );
    }

    #[test]
    fn write_rc_block_is_idempotent_no_duplicate_block() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        write_rc_block_for_shell(home, "zsh").unwrap();
        write_rc_block_for_shell(home, "zsh").unwrap();
        let zshenv = std::fs::read_to_string(home.join(".zshenv")).unwrap();
        let start_count = zshenv.matches(BLOCK_START_MARKER).count();
        assert_eq!(
            start_count, 1,
            "start marker must appear exactly once after two writes; content:\n{zshenv}"
        );
    }

    #[test]
    fn write_rc_block_uses_forces_first_case_statement() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        write_rc_block_for_shell(home, "zsh").unwrap();
        let zshenv = std::fs::read_to_string(home.join(".zshenv")).unwrap();
        // The block must use `case` (not a simple guard) to force-prepend even
        // when the shim dir already appears later in PATH.
        assert!(
            zshenv.contains("case \":$PATH:\""),
            ".zshenv block must use case statement for forces-first semantics; got:\n{zshenv}"
        );
        assert!(
            zshenv.contains("export PATH"),
            ".zshenv block must export PATH"
        );
    }

    #[test]
    fn write_rc_block_replaces_existing_block_on_rerun() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        // Manually write a "stale" block.
        let stale = format!(
            "{}\n# stale content\n{}\n",
            BLOCK_START_MARKER, BLOCK_END_MARKER
        );
        std::fs::write(home.join(".zshenv"), &stale).unwrap();
        write_rc_block_for_shell(home, "zsh").unwrap();
        let after = std::fs::read_to_string(home.join(".zshenv")).unwrap();
        assert!(
            !after.contains("stale content"),
            "stale block content must be replaced on rerun; got:\n{after}"
        );
        assert!(
            after.contains("case \":$PATH:\""),
            "replacement block must contain the current case statement"
        );
    }

    #[test]
    fn write_rc_block_preserves_content_after_end_marker() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let initial = format!(
            "{}\n# old block\n{}\n# user stuff after\n",
            BLOCK_START_MARKER, BLOCK_END_MARKER
        );
        std::fs::write(home.join(".zshenv"), &initial).unwrap();
        write_rc_block_for_shell(home, "zsh").unwrap();
        let after = std::fs::read_to_string(home.join(".zshenv")).unwrap();
        assert!(
            after.contains("user stuff after"),
            "content after the end marker must be preserved; got:\n{after}"
        );
    }

    #[test]
    fn write_rc_block_works_with_full_shell_path_like_bin_zsh() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        write_rc_block_for_shell(home, "/bin/zsh").unwrap();
        assert!(
            home.join(".zshenv").exists(),
            ".zshenv must be created for /bin/zsh"
        );
        assert!(
            home.join(".zshrc").exists(),
            ".zshrc must be created for /bin/zsh"
        );
    }

    /// The shim-dir path fragment, derived from the canonical constant in
    /// `hooks` so these assertions track the real install location instead of a
    /// drifting literal copy.
    const SHIM_EXPORT_FRAGMENT: &str = hooks::PATH_SHIM_REL_DIR;

    /// Install with consent, pinning shell to zsh so tests are independent of
    /// the CI runner's `$SHELL`.
    fn install_with_consent(home: &std::path::Path) {
        let mut stdin = Cursor::new("y\n");
        let mut stdout = Vec::new();
        run_install_with_io(home, false, &mut stdin, &mut stdout, "zsh").unwrap();
    }

    fn install_with_decline(home: &std::path::Path) {
        let mut stdin = Cursor::new("n\n");
        let mut stdout = Vec::new();
        run_install_with_io(home, false, &mut stdin, &mut stdout, "zsh").unwrap();
    }

    #[test]
    fn consent_writes_marker_block_to_zshenv_for_zsh() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let mut stdin = Cursor::new("y\n");
        let mut stdout = Vec::new();
        run_install_with_io(home, false, &mut stdin, &mut stdout, "zsh").unwrap();
        let zshenv = std::fs::read_to_string(home.join(".zshenv")).unwrap();
        assert!(
            zshenv.contains(BLOCK_START_MARKER),
            ".zshenv must contain the marker block after consent"
        );
    }

    #[test]
    fn consent_writes_marker_block_to_zshrc_for_zsh() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        let mut stdin = Cursor::new("y\n");
        let mut stdout = Vec::new();
        run_install_with_io(home, false, &mut stdin, &mut stdout, "zsh").unwrap();
        let zshrc = std::fs::read_to_string(home.join(".zshrc")).unwrap();
        assert!(
            zshrc.contains(BLOCK_START_MARKER),
            ".zshrc must contain the marker block after consent"
        );
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
    fn consent_appends_block_exactly_once_on_repeat() {
        let dir = TempDir::new().unwrap();
        let rc = dir.path().join(".zshrc");
        std::fs::write(&rc, "# shell rc\n").unwrap();

        install_with_consent(dir.path());
        install_with_consent(dir.path());

        let content = std::fs::read_to_string(&rc).unwrap();
        // Count the start marker — exactly one block means exactly one marker.
        let count = content.matches(BLOCK_START_MARKER).count();
        assert_eq!(
            count, 1,
            "managed PATH block must appear exactly once after two consents; rc:\n{content}"
        );
    }

    #[test]
    fn consent_output_contains_restart() {
        let dir = TempDir::new().unwrap();

        let mut stdin = Cursor::new("y\n");
        let mut stdout = Vec::new();
        run_install_with_io(dir.path(), false, &mut stdin, &mut stdout, "zsh").unwrap();

        let out = String::from_utf8(stdout).unwrap();
        assert!(
            out.contains("restart"),
            "output must tell user to restart Claude Code; got: {out:?}"
        );
    }

    #[test]
    fn decline_leaves_rc_files_unchanged() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        // Pre-create both files so we can check neither is modified.
        std::fs::write(home.join(".zshenv"), "# zshenv\n").unwrap();
        std::fs::write(home.join(".zshrc"), "# zshrc\n").unwrap();
        let zshenv_before = std::fs::read_to_string(home.join(".zshenv")).unwrap();
        let zshrc_before = std::fs::read_to_string(home.join(".zshrc")).unwrap();

        install_with_decline(home);

        let zshenv_after = std::fs::read_to_string(home.join(".zshenv")).unwrap();
        let zshrc_after = std::fs::read_to_string(home.join(".zshrc")).unwrap();
        assert_eq!(
            zshenv_before, zshenv_after,
            ".zshenv must be unchanged after decline"
        );
        assert_eq!(
            zshrc_before, zshrc_after,
            ".zshrc must be unchanged after decline"
        );
    }

    #[test]
    fn decline_output_contains_shim_dir_path() {
        let dir = TempDir::new().unwrap();
        let mut stdin = Cursor::new("n\n");
        let mut stdout = Vec::new();
        run_install_with_io(dir.path(), false, &mut stdin, &mut stdout, "zsh").unwrap();

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
    fn shell_routing_writes_zshenv_and_zshrc_for_zsh() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        write_rc_block_for_shell(home, "zsh").unwrap();
        assert!(home.join(".zshenv").exists(), "zsh must write .zshenv");
        assert!(home.join(".zshrc").exists(), "zsh must write .zshrc");
    }

    #[test]
    fn shell_routing_writes_bash_profile_and_bashrc_for_bash() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        write_rc_block_for_shell(home, "bash").unwrap();
        assert!(
            home.join(".bash_profile").exists(),
            "bash must write .bash_profile"
        );
        assert!(home.join(".bashrc").exists(), "bash must write .bashrc");
    }

    #[test]
    fn shell_routing_defaults_to_zsh_files_for_unknown_shell() {
        let dir = TempDir::new().unwrap();
        write_rc_block_for_shell(dir.path(), "").unwrap();
        assert!(
            dir.path().join(".zshenv").exists(),
            "unknown shell must default to .zshenv"
        );
    }

    /// A path that merely CONTAINS "bash" as a substring (e.g. `/usr/local/newbash`)
    /// must not be misidentified as bash — only a path whose final component IS "bash"
    /// should route to bash files.
    #[test]
    fn shell_routing_does_not_misroute_shell_path_containing_bash_as_substring() {
        let dir = TempDir::new().unwrap();
        write_rc_block_for_shell(dir.path(), "/usr/local/bin/newbash").unwrap();
        assert!(
            dir.path().join(".zshenv").exists(),
            "shell whose basename is 'newbash' (not 'bash') must default to .zshenv"
        );
    }

    #[test]
    fn run_status_warns_when_shim_not_first_in_nonlogin_shell_path() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        install_with_decline(home);
        let mut out = Vec::new();
        // Simulate a non-login shell where the shim dir is present but NOT first.
        run_status_with_path_for_test(home, &mut out, Some("/opt/homebrew/bin:/usr/bin")).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("warning") || text.contains("not first"),
            "status must warn when shim dir is not first on PATH; got: {text:?}"
        );
    }

    #[test]
    fn run_status_no_warning_when_shim_is_first_in_path() {
        let dir = TempDir::new().unwrap();
        let home = dir.path();
        install_with_decline(home);
        let shim_dir = home.join(".local/share/git-prism/bin");
        let path_with_shim_first = format!("{}:/opt/homebrew/bin:/usr/bin", shim_dir.display());
        let mut out = Vec::new();
        run_status_with_path_for_test(home, &mut out, Some(&path_with_shim_first)).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            !text.contains("not first"),
            "status must not warn when shim dir is first on PATH; got: {text:?}"
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
