//! Adapters from `Classification` variants to `tools::*` functions.
//!
//! Each handler resolves the repository via `gix::discover`, calls the
//! corresponding tool function, serialises the result to stdout, and returns
//! `ExitCode::SUCCESS`.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use crate::git::refs::RefRange;
use crate::git::refs::parse_range;
use crate::shim::classify::Classification;
use crate::tools::{
    ContextOptions, ManifestOptions, SnapshotOptions, build_function_context_with_options,
    build_snapshots, collect_all_history_pages, collect_all_manifest_pages,
    collect_all_worktree_manifest_pages,
};

/// Dispatch a classified git command to the appropriate tool function and
/// write the JSON result to `out`.  Returns `ExitCode::SUCCESS` on success
/// or `ExitCode::FAILURE` on error.
pub(crate) fn handle<W: Write>(
    classification: &Classification<'_>,
    repo_path: &Path,
    out: &mut W,
) -> ExitCode {
    match dispatch(classification, repo_path, out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("git-prism shim: handler error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch<W: Write>(
    classification: &Classification<'_>,
    repo_path: &Path,
    out: &mut W,
) -> anyhow::Result<()> {
    match classification {
        Classification::Manifest { range } => handle_manifest(range, repo_path, out),
        Classification::History { range } => handle_history(range, repo_path, out),
        Classification::FunctionContext {
            range,
            pickaxe_term,
        } => handle_function_context(*range, pickaxe_term, repo_path, out),
        Classification::ShowSnapshot { sha } => handle_show_snapshot(sha, repo_path, out),
        Classification::BlameSnapshot { path } => handle_blame_snapshot(path, repo_path, out),
        Classification::GhPrDiff { pr_number } => handle_gh_pr_diff(pr_number, repo_path, out),
        Classification::Passthrough => {
            anyhow::bail!("dispatch called with Passthrough — caller bug")
        }
    }
}

fn handle_manifest<W: Write>(range: &str, repo_path: &Path, out: &mut W) -> anyhow::Result<()> {
    let options = ManifestOptions {
        include_patterns: vec![],
        exclude_patterns: vec![],
        include_function_analysis: false,
        max_response_tokens: Some(8192),
    };
    let result = match parse_range(range) {
        RefRange::CommitRange { base, head } => {
            collect_all_manifest_pages(repo_path, base, head, &options, 500)?
        }
        RefRange::WorktreeCompare { base } => {
            collect_all_worktree_manifest_pages(repo_path, base, &options, 500)?
        }
    };
    serde_json::to_writer_pretty(out, &result)?;
    Ok(())
}

fn handle_history<W: Write>(range: &str, repo_path: &Path, out: &mut W) -> anyhow::Result<()> {
    let (base, head) = match parse_range(range) {
        RefRange::CommitRange { base, head } => (base, head),
        RefRange::WorktreeCompare { .. } => {
            anyhow::bail!("history requires a commit range")
        }
    };
    let options = ManifestOptions {
        include_patterns: vec![],
        exclude_patterns: vec![],
        include_function_analysis: true,
        max_response_tokens: None,
    };
    let result = collect_all_history_pages(repo_path, base, head, &options, 500)?;
    serde_json::to_writer_pretty(out, &result)?;
    Ok(())
}

fn handle_function_context<W: Write>(
    range: Option<&str>,
    _pickaxe_term: &str,
    repo_path: &Path,
    out: &mut W,
) -> anyhow::Result<()> {
    let effective_range = range.unwrap_or("HEAD~1..HEAD");
    let (base, head) = match parse_range(effective_range) {
        RefRange::CommitRange { base, head } => (base, head),
        RefRange::WorktreeCompare { .. } => {
            anyhow::bail!("function context requires a commit range")
        }
    };
    let options = ContextOptions {
        cursor: None,
        page_size: 25,
        function_names: None,
        max_response_tokens: Some(8192),
    };
    let result = build_function_context_with_options(repo_path, base, head, &options)?;
    serde_json::to_writer_pretty(out, &result)?;
    Ok(())
}

fn handle_show_snapshot<W: Write>(sha: &str, repo_path: &Path, out: &mut W) -> anyhow::Result<()> {
    // `git show <sha>` maps to get_file_snapshots with range `<sha>^..<sha>`.
    let base = format!("{sha}^");
    let range_str = format!("{base}..{sha}");
    let (base_ref, head_ref) = match parse_range(&range_str) {
        RefRange::CommitRange { base, head } => (base.to_string(), head.to_string()),
        RefRange::WorktreeCompare { .. } => unreachable!("range_str always has .."),
    };
    let options = SnapshotOptions {
        include_before: true,
        include_after: true,
        max_file_size_bytes: 100_000,
        line_range: None,
        include_diff_hunks: false,
    };
    let result = build_snapshots(repo_path, &base_ref, &head_ref, &[], &options)?;
    serde_json::to_writer_pretty(out, &result)?;
    Ok(())
}

/// Handle `gh pr diff <N>` by resolving the PR's base..head ref range via the
/// `gh` CLI, then feeding it through the existing manifest pipeline.
///
/// Execs `gh pr view <N> --json baseRefName,headRefName` to obtain the branch
/// names, then calls `handle_manifest` with `"base..head"`.
///
/// `repo_path` may be a subdirectory of the git repo (e.g. `bdd/`); this
/// function discovers the actual git root before opening it, mirroring what
/// `git rev-parse --show-toplevel` does.
fn handle_gh_pr_diff<W: Write>(
    pr_number: &str,
    repo_path: &Path,
    out: &mut W,
) -> anyhow::Result<()> {
    let range = resolve_pr_range(pr_number)?;
    // Discover the real git root so gix::open() doesn't fail on subdirectories.
    let git_root = discover_git_root(repo_path)?;
    handle_manifest(&range, &git_root, out)
}

/// Walk up the directory tree from `start` until we find a directory that
/// contains a `.git` entry (file or directory), and return that directory.
///
/// This mirrors what `git rev-parse --show-toplevel` does and is necessary
/// because `gix::open` requires the exact repo root, not a subdirectory.
fn discover_git_root(start: &Path) -> anyhow::Result<std::path::PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        if current.join(".git").exists() {
            return Ok(current);
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => anyhow::bail!(
                "could not find git repository root from {}",
                start.display()
            ),
        }
    }
}

/// Resolve a PR number to a `"base_sha..head_sha"` ref range string by
/// calling `gh pr view <N> --json baseRefName,headRefName,baseRefOid,headRefOid`.
///
/// Uses commit SHAs (not branch names) so the range works even after the PR
/// branch has been deleted (merged PRs).  The SHAs are permanent; branch names
/// are ephemeral.
///
/// Returns an error when `gh` is not on PATH, exits non-zero, or returns
/// JSON that cannot be parsed.
fn resolve_pr_range(pr_number: &str) -> anyhow::Result<String> {
    let output = std::process::Command::new("gh")
        .args(["pr", "view", pr_number, "--json", "baseRefOid,headRefOid"])
        .output()
        .map_err(|e| anyhow::anyhow!("failed to run gh pr view: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "gh pr view {pr_number} failed with status {}: {stderr}",
            output.status
        );
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| anyhow::anyhow!("gh pr view returned invalid JSON: {e}"))?;

    let base_sha = json["baseRefOid"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("gh pr view JSON missing baseRefOid"))?;
    let head_sha = json["headRefOid"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("gh pr view JSON missing headRefOid"))?;

    Ok(format!("{base_sha}..{head_sha}"))
}

fn handle_blame_snapshot<W: Write>(
    path: &str,
    repo_path: &Path,
    out: &mut W,
) -> anyhow::Result<()> {
    // `git blame <path>` maps to get_file_snapshots for the whole file at HEAD.
    let options = SnapshotOptions {
        include_before: false,
        include_after: true,
        max_file_size_bytes: 100_000,
        line_range: None,
        include_diff_hunks: false,
    };
    let result = build_snapshots(repo_path, "HEAD^", "HEAD", &[path.to_string()], &options)?;
    serde_json::to_writer_pretty(out, &result)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;

    use tempfile::TempDir;

    use super::*;

    // ---- fixture helpers ----

    fn init_repo_with_two_commits() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();

        let run = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&path)
                .output()
                .unwrap()
        };

        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "test@test.com"]);
        run(&["config", "user.name", "Test"]);

        std::fs::write(path.join("hello.txt"), "hello\n").unwrap();
        run(&["add", "hello.txt"]);
        run(&["commit", "-m", "first commit"]);

        std::fs::write(path.join("world.txt"), "world\n").unwrap();
        run(&["add", "world.txt"]);
        run(&["commit", "-m", "second commit"]);

        (dir, path)
    }

    fn head_sha(repo: &Path) -> String {
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repo)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    // ---- tests ----

    #[test]
    fn it_handles_manifest_and_returns_files_array() {
        let (_dir, path) = init_repo_with_two_commits();
        let classification = Classification::Manifest {
            range: "HEAD~1..HEAD",
        };
        let mut out = Vec::new();
        let code = handle(&classification, &path, &mut out);
        assert_eq!(code, ExitCode::SUCCESS);

        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert!(
            json.get("files").and_then(|f| f.as_array()).is_some(),
            "expected 'files' array in manifest output"
        );
    }

    #[test]
    fn it_handles_history_and_returns_commits_array() {
        let (_dir, path) = init_repo_with_two_commits();
        let classification = Classification::History {
            range: "HEAD~1..HEAD",
        };
        let mut out = Vec::new();
        let code = handle(&classification, &path, &mut out);
        assert_eq!(code, ExitCode::SUCCESS);

        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert!(
            json.get("commits").and_then(|c| c.as_array()).is_some(),
            "expected 'commits' array in history output"
        );
    }

    #[test]
    fn it_handles_show_snapshot_and_returns_snapshots_key() {
        let (_dir, path) = init_repo_with_two_commits();
        let sha = head_sha(&path);
        let classification = Classification::ShowSnapshot { sha: &sha };
        let mut out = Vec::new();
        let code = handle(&classification, &path, &mut out);
        assert_eq!(code, ExitCode::SUCCESS);

        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        // build_snapshots returns a response with a "files" array.
        assert!(
            json.get("files").and_then(|f| f.as_array()).is_some(),
            "expected 'files' array in show output, got: {json}"
        );
    }

    #[test]
    fn it_handles_blame_snapshot_and_returns_files_array() {
        let (_dir, path) = init_repo_with_two_commits();
        let classification = Classification::BlameSnapshot { path: "hello.txt" };
        let mut out = Vec::new();
        let code = handle(&classification, &path, &mut out);
        assert_eq!(code, ExitCode::SUCCESS);

        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        // build_snapshots returns a response with a "files" array.
        assert!(
            json.get("files").and_then(|f| f.as_array()).is_some(),
            "expected 'files' array in blame output, got: {json}"
        );
    }

    #[test]
    fn it_handles_function_context_and_returns_functions_key() {
        let (_dir, path) = init_repo_with_two_commits();
        let classification = Classification::FunctionContext {
            range: Some("HEAD~1..HEAD"),
            pickaxe_term: "hello",
        };
        let mut out = Vec::new();
        let code = handle(&classification, &path, &mut out);
        assert_eq!(code, ExitCode::SUCCESS);

        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        // build_function_context_with_options returns a response with a "functions" array.
        assert!(
            json.get("functions").is_some(),
            "expected 'functions' key in function_context output, got: {json}"
        );
    }
}
