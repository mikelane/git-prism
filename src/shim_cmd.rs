//! `git-prism shim` subcommand: install / uninstall / report status of the
//! interactive-shell `git` function.
//!
//! The shim is a marker-delimited `function git { ... }` block in the user's
//! interactive rc file (`~/.zshrc` / `~/.bashrc`). The function routes the
//! `git` commands an agent types into its shell through
//! `git-prism shim-run git ...`; programs that exec `git` directly never see it.
//!
//! Earlier releases put a PATH-prepend block in `.zshenv`/`.bash_profile` plus
//! `~/.local/share/git-prism/bin/{git,gh}` symlinks. Install migrates those
//! away automatically.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::hooks;

/// Markers that delimit the managed block so it can be replaced idempotently.
const BLOCK_START_MARKER: &str = "# >>> git-prism shim >>>";
const BLOCK_END_MARKER: &str = "# <<< git-prism shim <<<";

/// The managed function block content (without markers).
///
/// Uses `function git { ... }` syntax rather than `git() { ... }` so a user's
/// `alias git=...` cannot alias-expand the definition into a parse error. When
/// `git-prism` is not on PATH the function falls back to real git instead of
/// breaking every git call.
const SHIM_FUNCTION_BLOCK_BODY: &str = r#"function git {
  if command -v git-prism >/dev/null 2>&1; then
    command git-prism shim-run git "$@"
  else
    command git "$@"
  fi
}"#;

/// Every startup file a managed block may live in: the legacy PATH block went
/// into the env/login files and the rc files; the function block goes only into
/// an interactive rc file.
const ALL_STARTUP_FILES: [&str; 4] = [".zshenv", ".zshrc", ".bash_profile", ".bashrc"];

const RESTART_NOTICE: &str =
    "Restart your agent session so its shell snapshot picks up the function.";

/// Install the rc shell function, migrating any legacy PATH shim, and print the
/// result to stdout. Idempotent.
pub fn run_install(home: &Path) -> Result<()> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    run_install_with_io(home, &shell, &mut std::io::stdout())
}

/// Testable install implementation with injected output and shell name.
///
/// `shell` is the ambient `$SHELL`; tests pass an explicit value so results are
/// independent of the CI runner's shell.
pub fn run_install_with_io(home: &Path, shell: &str, stdout: &mut dyn Write) -> Result<()> {
    remove_legacy_path_shim(home)?;
    let rc_file = interactive_rc_for_shell(shell);
    write_marker_block(&home.join(rc_file))?;
    writeln!(stdout, "Installed git shim function in ~/{rc_file}")?;
    writeln!(stdout, "{RESTART_NOTICE}")?;
    Ok(())
}

/// Resolve the interactive rc file for a shell, keyed on the basename of
/// `$SHELL`. `"bash"` routes to `.bashrc`; everything else (zsh, empty,
/// unknown) defaults to `.zshrc`.
///
/// Matching on the basename — not a substring of the whole path — means a shell
/// at `/usr/local/bin/newbash` is correctly treated as not-bash.
fn interactive_rc_for_shell(shell: &str) -> &'static str {
    let basename = Path::new(shell)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(shell);
    if basename == "bash" {
        ".bashrc"
    } else {
        ".zshrc"
    }
}

fn legacy_shim_dir(home: &Path) -> PathBuf {
    home.join(hooks::PATH_SHIM_REL_DIR)
}

/// Remove the legacy PATH-shim artifacts: managed blocks in every startup file
/// and the `git`/`gh` symlinks (plus their directory when left empty).
///
/// Only symlinks are removed; a regular file a user placed in the directory is
/// left alone, and so is the directory when anything remains in it.
fn remove_legacy_path_shim(home: &Path) -> Result<()> {
    strip_managed_blocks_from_startup_files(home)?;
    remove_legacy_symlinks(home)
}

fn strip_managed_blocks_from_startup_files(home: &Path) -> Result<()> {
    for name in ALL_STARTUP_FILES {
        let path = home.join(name);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !content.lines().any(is_marker_line) {
            continue;
        }
        std::fs::write(&path, strip_all_marker_blocks(&content))
            .with_context(|| format!("failed to update {}", path.display()))?;
    }
    Ok(())
}

fn is_marker_line(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed == BLOCK_START_MARKER || trimmed == BLOCK_END_MARKER
}

fn remove_legacy_symlinks(home: &Path) -> Result<()> {
    let dir = legacy_shim_dir(home);
    for name in hooks::PATH_SHIM_LINK_NAMES {
        let link = dir.join(name);
        let is_symlink = std::fs::symlink_metadata(&link)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if is_symlink {
            std::fs::remove_file(&link)
                .with_context(|| format!("failed to remove symlink {}", link.display()))?;
        }
    }
    let dir_is_empty = dir
        .read_dir()
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false);
    if dir_is_empty {
        std::fs::remove_dir(&dir)
            .with_context(|| format!("failed to remove directory {}", dir.display()))?;
    }
    Ok(())
}

/// Write (or replace) the marker-delimited function block in the given rc file.
///
/// Strips ALL existing managed blocks and stray markers (handles corruption
/// from hand-edits, interrupted writes, or prior buggy runs) then appends
/// exactly one canonical block.  Creates the file if it doesn't exist.
fn write_marker_block(rc_path: &Path) -> Result<()> {
    let existing = if rc_path.exists() {
        std::fs::read_to_string(rc_path)
            .with_context(|| format!("failed to read rc file {}", rc_path.display()))?
    } else {
        String::new()
    };

    let new_block =
        format!("{BLOCK_START_MARKER}\n{SHIM_FUNCTION_BLOCK_BODY}\n{BLOCK_END_MARKER}\n");

    let cleaned = strip_all_marker_blocks(&existing);

    // Append the single canonical block with a blank-line separator.
    let new_content = if cleaned.is_empty() {
        new_block
    } else {
        format!("{cleaned}\n{new_block}")
    };

    std::fs::write(rc_path, new_content)
        .with_context(|| format!("failed to write shim block to {}", rc_path.display()))?;
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
/// Marker detection uses full-line equality (`line.trim() == MARKER`), not
/// substring matching.  A user line that merely *mentions* the marker text
/// (e.g. `echo "# >>> git-prism shim >>>"`) is preserved unchanged.  Only a
/// line whose entire trimmed content is the marker string is treated as a
/// block boundary.
fn strip_all_marker_blocks(content: &str) -> String {
    let mut kept_lines: Vec<&str> = Vec::new();
    let mut inside_block = false;

    // `split_inclusive` keeps each line's own terminator, so a CRLF rc file's
    // user lines come back byte for byte.
    for line in content.split_inclusive('\n') {
        if line.trim() == BLOCK_START_MARKER {
            // Enter managed block; discard this line.
            inside_block = true;
        } else if line.trim() == BLOCK_END_MARKER {
            // Exit managed block; discard this line regardless of whether we
            // saw a matching START (handles orphaned END markers).
            inside_block = false;
        } else if !inside_block {
            kept_lines.push(line);
        }
        // Lines inside a managed block are silently dropped.
    }

    // Trim trailing blank lines introduced by block removal.
    while kept_lines
        .last()
        .is_some_and(|line| line.trim_end_matches(['\r', '\n']).is_empty())
    {
        kept_lines.pop();
    }

    let mut result = kept_lines.concat();
    if !result.is_empty() && !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Remove the function block and every legacy PATH-shim artifact.
pub fn run_uninstall(home: &Path) -> Result<()> {
    run_uninstall_with_io(home, &mut std::io::stdout())
}

fn run_uninstall_with_io(home: &Path, stdout: &mut dyn Write) -> Result<()> {
    remove_legacy_path_shim(home)?;
    writeln!(stdout, "Removed git-prism shim.")?;
    Ok(())
}

/// Report whether the function block is installed and whether legacy PATH-shim
/// artifacts are still present.
pub fn run_status(home: &Path) -> Result<()> {
    run_status_with_io(home, &mut std::io::stdout())
}

fn run_status_with_io(home: &Path, out: &mut dyn Write) -> Result<()> {
    match startup_file_with_function_block(home) {
        Some(name) => writeln!(out, "shim: installed in ~/{name}")?,
        None => writeln!(out, "shim: not installed")?,
    }
    report_legacy_path_shim(home, out)
}

fn startup_file_with_function_block(home: &Path) -> Option<&'static str> {
    ALL_STARTUP_FILES.into_iter().find(|name| {
        std::fs::read_to_string(home.join(name))
            .is_ok_and(|content| has_marker_block(&content) && content.contains("shim-run"))
    })
}

fn has_marker_block(content: &str) -> bool {
    content
        .lines()
        .any(|line| line.trim() == BLOCK_START_MARKER)
}

fn report_legacy_path_shim(home: &Path, out: &mut dyn Write) -> Result<()> {
    let dir = legacy_shim_dir(home);
    let has_symlinks = hooks::PATH_SHIM_LINK_NAMES
        .iter()
        .any(|name| std::fs::symlink_metadata(dir.join(name)).is_ok());
    if has_symlinks {
        writeln!(out, "legacy PATH shim: present at {}", dir.display())?;
    }
    let legacy_block_file = ALL_STARTUP_FILES.into_iter().find(|name| {
        std::fs::read_to_string(home.join(name))
            .is_ok_and(|content| has_marker_block(&content) && !content.contains("shim-run"))
    });
    if let Some(name) = legacy_block_file {
        writeln!(out, "legacy PATH shim: present in ~/{name}")?;
    }
    if has_symlinks || legacy_block_file.is_some() {
        writeln!(
            out,
            "Run `git-prism shim install` to migrate to the shell function."
        )?;
    }
    Ok(())
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// The PATH block that releases before the rc-function design wrote
    /// verbatim; install must migrate it away.
    const LEGACY_PATH_BLOCK_BODY: &str = r#"case ":$PATH:" in
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

    fn legacy_block() -> String {
        format!("{BLOCK_START_MARKER}\n{LEGACY_PATH_BLOCK_BODY}\n{BLOCK_END_MARKER}\n")
    }

    fn read(home: &std::path::Path, name: &str) -> String {
        std::fs::read_to_string(home.join(name)).unwrap_or_default()
    }

    fn install(home: &std::path::Path, shell: &str) -> String {
        let mut out = Vec::new();
        run_install_with_io(home, shell, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn status(home: &std::path::Path) -> String {
        let mut out = Vec::new();
        run_status_with_io(home, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn seed_legacy_symlinks(home: &std::path::Path) -> std::path::PathBuf {
        let dir = home.join(hooks::PATH_SHIM_REL_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        for name in hooks::PATH_SHIM_LINK_NAMES {
            std::os::unix::fs::symlink("/nonexistent/git-prism", dir.join(name)).unwrap();
        }
        dir
    }

    #[test]
    fn it_preserves_crlf_line_endings_of_user_content() {
        let home = TempDir::new().unwrap();
        std::fs::write(home.path().join(".zshrc"), "export A=1\r\nexport B=2\r\n").unwrap();

        install(home.path(), "/bin/zsh");

        assert!(
            read(home.path(), ".zshrc").starts_with("export A=1\r\nexport B=2\r\n"),
            "user lines must keep their CRLF endings"
        );
    }

    #[test]
    fn it_writes_the_function_block_to_zshrc_only() {
        let home = TempDir::new().unwrap();
        install(home.path(), "/bin/zsh");

        let zshrc = read(home.path(), ".zshrc");
        assert!(zshrc.contains(BLOCK_START_MARKER), "zshrc: {zshrc}");
        assert!(zshrc.contains(BLOCK_END_MARKER), "zshrc: {zshrc}");
        assert!(zshrc.contains("function git {"), "zshrc: {zshrc}");
        assert!(!home.path().join(".zshenv").exists());
    }

    #[test]
    fn it_writes_the_function_block_to_bashrc_for_bash() {
        let home = TempDir::new().unwrap();
        install(home.path(), "/usr/bin/bash");

        assert!(read(home.path(), ".bashrc").contains("function git {"));
        assert!(!home.path().join(".bash_profile").exists());
        assert!(!home.path().join(".zshrc").exists());
    }

    #[test]
    fn it_defaults_to_zshrc_for_an_unknown_shell() {
        assert_eq!(interactive_rc_for_shell(""), ".zshrc");
        assert_eq!(interactive_rc_for_shell("/usr/local/bin/newbash"), ".zshrc");
        assert_eq!(interactive_rc_for_shell("/bin/bash"), ".bashrc");
    }

    #[test]
    fn it_installs_the_exact_function_body() {
        let home = TempDir::new().unwrap();
        install(home.path(), "zsh");
        assert!(read(home.path(), ".zshrc").contains(SHIM_FUNCTION_BLOCK_BODY));
        assert!(SHIM_FUNCTION_BLOCK_BODY.contains("command git-prism shim-run git \"$@\""));
        assert!(SHIM_FUNCTION_BLOCK_BODY.contains("command git \"$@\""));
    }

    #[test]
    fn it_migrates_the_legacy_path_blocks_and_symlinks() {
        let home = TempDir::new().unwrap();
        std::fs::write(home.path().join(".zshenv"), legacy_block()).unwrap();
        std::fs::write(
            home.path().join(".zshrc"),
            format!("# mine\n\n{}", legacy_block()),
        )
        .unwrap();
        let dir = seed_legacy_symlinks(home.path());

        install(home.path(), "zsh");

        assert!(!read(home.path(), ".zshenv").contains("git-prism/bin"));
        let zshrc = read(home.path(), ".zshrc");
        assert!(!zshrc.contains("git-prism/bin"), "zshrc: {zshrc}");
        assert_eq!(zshrc.matches(BLOCK_START_MARKER).count(), 1);
        assert!(zshrc.starts_with("# mine\n"));
        assert!(!dir.exists(), "empty shim dir must be removed");
    }

    #[test]
    fn it_leaves_a_non_symlink_file_in_the_shim_dir_alone() {
        let home = TempDir::new().unwrap();
        let dir = home.path().join(hooks::PATH_SHIM_REL_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("git"), "#!/bin/sh\n").unwrap();

        install(home.path(), "zsh");

        assert!(dir.join("git").exists(), "user's regular file must survive");
    }

    #[test]
    fn it_is_idempotent() {
        let home = TempDir::new().unwrap();
        install(home.path(), "zsh");
        let first = read(home.path(), ".zshrc");
        install(home.path(), "zsh");
        assert_eq!(first, read(home.path(), ".zshrc"));
    }

    #[test]
    fn it_preserves_user_content_around_the_block() {
        let home = TempDir::new().unwrap();
        std::fs::write(home.path().join(".zshrc"), "export A=1\n").unwrap();
        install(home.path(), "zsh");
        let mut zshrc = read(home.path(), ".zshrc");
        zshrc.push_str("export B=2\n");
        std::fs::write(home.path().join(".zshrc"), &zshrc).unwrap();

        install(home.path(), "zsh");

        let after = read(home.path(), ".zshrc");
        assert!(after.starts_with("export A=1\n"));
        assert!(after.contains("export B=2\n"));
        assert_eq!(after.matches(BLOCK_START_MARKER).count(), 1);
    }

    #[test]
    fn it_does_not_rewrite_unrelated_rc_files() {
        let home = TempDir::new().unwrap();
        std::fs::write(home.path().join(".zshenv"), "export X=1\n\n\n").unwrap();
        install(home.path(), "zsh");
        assert_eq!(read(home.path(), ".zshenv"), "export X=1\n\n\n");
    }

    #[test]
    fn it_names_the_rc_file_and_asks_for_a_session_restart() {
        let home = TempDir::new().unwrap();
        let out = install(home.path(), "zsh");
        assert!(out.contains("~/.zshrc"), "out: {out}");
        assert!(
            out.contains("Restart your agent session so its shell snapshot picks up the function."),
            "out: {out}"
        );
    }

    #[test]
    fn it_uninstalls_cleanly() {
        let home = TempDir::new().unwrap();
        std::fs::write(home.path().join(".zshenv"), legacy_block()).unwrap();
        install(home.path(), "zsh");
        let dir = seed_legacy_symlinks(home.path());

        run_uninstall_with_io(home.path(), &mut Vec::new()).unwrap();

        for name in [".zshenv", ".zshrc", ".bashrc", ".bash_profile"] {
            let content = read(home.path(), name);
            assert!(!content.contains(BLOCK_START_MARKER), "{name}: {content}");
            assert!(!content.contains(BLOCK_END_MARKER), "{name}: {content}");
        }
        assert!(!dir.exists());
    }

    #[test]
    fn it_uninstalls_when_nothing_is_installed() {
        let home = TempDir::new().unwrap();
        run_uninstall_with_io(home.path(), &mut Vec::new()).unwrap();
    }

    #[test]
    fn status_reports_not_installed_when_absent() {
        let home = TempDir::new().unwrap();
        let out = status(home.path());
        assert!(out.contains("shim: not installed"), "out: {out}");
        assert!(!out.contains("legacy PATH shim"), "out: {out}");
    }

    #[test]
    fn status_reports_the_rc_file_after_install() {
        let home = TempDir::new().unwrap();
        install(home.path(), "zsh");
        let out = status(home.path());
        assert!(out.contains("shim: installed in ~/.zshrc"), "out: {out}");
    }

    #[test]
    fn status_reports_a_legacy_symlink_dir() {
        let home = TempDir::new().unwrap();
        let dir = seed_legacy_symlinks(home.path());
        let out = status(home.path());
        assert!(
            out.contains(&format!("legacy PATH shim: present at {}", dir.display())),
            "out: {out}"
        );
    }

    #[test]
    fn status_reports_a_legacy_path_block_without_symlinks() {
        let home = TempDir::new().unwrap();
        std::fs::write(home.path().join(".zshenv"), legacy_block()).unwrap();
        let out = status(home.path());
        assert!(out.contains("shim: not installed"), "out: {out}");
        assert!(
            out.contains("legacy PATH shim: present in ~/.zshenv"),
            "out: {out}"
        );
    }
}
