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
