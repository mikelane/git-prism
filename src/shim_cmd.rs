//! `git-prism shim` subcommand: install / uninstall / report status of the
//! PATH-layer git shim.
//!
//! This is a thin CLI wrapper around the path-shim helpers in `hooks.rs`.
//! See ADR-0009 (`docs/decisions/0009-path-shim-architecture.md`) for design
//! rationale.

use anyhow::Result;

use crate::hooks;

/// Install the PATH shim symlink and print the result to stdout.
///
/// Idempotent: if the symlink already exists and points at the current binary,
/// this is a silent no-op. `force` allows overwriting a regular file at the
/// target path.
pub fn run_install(home: &std::path::Path, force: bool) -> Result<()> {
    let symlink_path = hooks::install_path_shim(home, force)?;
    println!("Created symlink: {}", symlink_path.display());
    println!(
        "Add this to your shell init (~/.zshrc or ~/.bashrc):\n  export PATH=\"$HOME/.local/share/git-prism/bin:$PATH\""
    );
    Ok(())
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
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    #[cfg(unix)]
    fn run_install_creates_symlink_under_home() {
        let dir = TempDir::new().unwrap();
        run_install(dir.path(), false).unwrap();
        let link = dir.path().join(".local/share/git-prism/bin/git");
        assert!(link.is_symlink(), "symlink must exist after shim install");
    }

    #[test]
    #[cfg(unix)]
    fn run_install_is_idempotent() {
        let dir = TempDir::new().unwrap();
        run_install(dir.path(), false).unwrap();
        run_install(dir.path(), false).unwrap();
        let link = dir.path().join(".local/share/git-prism/bin/git");
        assert!(
            link.is_symlink(),
            "symlink must remain after second install"
        );
    }

    #[test]
    #[cfg(unix)]
    fn run_uninstall_removes_symlink() {
        let dir = TempDir::new().unwrap();
        run_install(dir.path(), false).unwrap();
        run_uninstall(dir.path()).unwrap();
        let link = dir.path().join(".local/share/git-prism/bin/git");
        assert!(!link.is_symlink() && !link.exists(), "symlink must be gone");
    }

    #[test]
    fn run_status_reports_not_installed_when_absent() {
        let dir = TempDir::new().unwrap();
        // Verifies no panic/error when shim is absent.
        run_status(dir.path()).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn run_status_succeeds_after_install() {
        let dir = TempDir::new().unwrap();
        run_install(dir.path(), false).unwrap();
        run_status(dir.path()).unwrap();
    }
}
