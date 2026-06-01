//! Adapters from `Classification` variants to `tools::*` functions.
//!
//! Each handler resolves the repository via `gix::discover`, calls the
//! corresponding tool function, serialises the result to stdout, and returns
//! `ExitCode::SUCCESS`.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use crate::git::reader::RepoReader;
use crate::git::refs::RefRange;
use crate::git::refs::parse_range;
use crate::shim::classify::Classification;
use crate::tools::types::{
    CommitSignature, ShowCommitDetail, ShowDiffstat, ShowFileEntry, ShowManifestResponse,
};
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
    let commit = build_show_commit_detail(repo_path, sha)?;

    let files: Vec<ShowFileEntry> = if commit.parents.is_empty() {
        // Root commit: no parent, so `<sha>^` doesn't resolve.  Walk the
        // commit tree directly and report every blob as Added.
        let reader =
            RepoReader::open(repo_path).map_err(|e| anyhow::anyhow!("failed to open repo: {e}"))?;
        let diff = reader
            .diff_root_commit(sha)
            .map_err(|e| anyhow::anyhow!("failed to diff root commit: {e}"))?;
        diff.files
            .into_iter()
            .map(file_change_to_show_entry)
            .collect()
    } else {
        // Normal commit: diff against first parent via the manifest pipeline.
        let manifest_options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };
        let base_ref = format!("{sha}^");
        let manifest =
            collect_all_manifest_pages(repo_path, &base_ref, sha, &manifest_options, 500)?;
        manifest
            .files
            .into_iter()
            .map(|f| ShowFileEntry {
                path: f.path,
                old_path: f.old_path,
                change_type: f.change_type,
                additions: f.lines_added,
                deletions: f.lines_removed,
                is_binary: f.is_binary,
            })
            .collect()
    };

    // Derive diffstat directly from per-file counts — no net-delta arithmetic.
    let insertions: usize = files.iter().map(|f| f.additions).sum();
    let deletions: usize = files.iter().map(|f| f.deletions).sum();
    let diffstat = ShowDiffstat {
        files_changed: files.len(),
        insertions,
        deletions,
    };

    let result = ShowManifestResponse {
        commit,
        diffstat,
        files,
        token_estimate: 0,
    };
    serde_json::to_writer_pretty(out, &result)?;
    Ok(())
}

/// Map a raw `FileChange` (from `diff_root_commit`) to the show-specific
/// `ShowFileEntry` shape.
fn file_change_to_show_entry(f: crate::git::diff::FileChange) -> ShowFileEntry {
    ShowFileEntry {
        path: f.path,
        old_path: f.old_path,
        change_type: f.change_type,
        additions: f.lines_added,
        deletions: f.lines_removed,
        is_binary: f.is_binary,
    }
}

/// Extract structured commit metadata for `sha` using gix (no shell out).
fn build_show_commit_detail(repo_path: &Path, sha: &str) -> anyhow::Result<ShowCommitDetail> {
    let reader =
        RepoReader::open(repo_path).map_err(|e| anyhow::anyhow!("failed to open repo: {e}"))?;
    let commit = reader
        .peel_to_commit(sha)
        .map_err(|e| anyhow::anyhow!("failed to resolve commit {sha}: {e}"))?;

    let full_sha = commit.id().to_string();
    let short_sha = full_sha.chars().take(8).collect::<String>();

    let parents = commit
        .parent_ids()
        .map(|id| id.to_string())
        .collect::<Vec<_>>();

    let author_sig = commit
        .author()
        .map_err(|e| anyhow::anyhow!("failed to read author: {e}"))?;
    let committer_sig = commit
        .committer()
        .map_err(|e| anyhow::anyhow!("failed to read committer: {e}"))?;

    let author = signature_to_commit_signature(&author_sig);
    let committer = signature_to_commit_signature(&committer_sig);

    let raw_message = commit
        .message_raw()
        .map_err(|e| anyhow::anyhow!("failed to read message: {e}"))?;
    let message = std::str::from_utf8(raw_message.as_ref())
        .map_err(|e| anyhow::anyhow!("commit message not UTF-8: {e}"))?;
    let (subject, body) = split_commit_message(message);

    Ok(ShowCommitDetail {
        sha: full_sha,
        short_sha,
        parents,
        author,
        committer,
        subject,
        body,
    })
}

/// Convert a gix `SignatureRef` into our serialisable `CommitSignature`.
///
/// `SignatureRef.time` is a raw `&str` like `"1780329644 +0000"`.
/// We call `sig.time()` (the decode method) to get `gix_date::Time` with
/// typed `seconds: i64` and `offset: i32` fields.
fn signature_to_commit_signature(sig: &gix::actor::SignatureRef<'_>) -> CommitSignature {
    // sig.time() decodes the raw time string; fall back to epoch 0 on parse failure.
    let gix_time = sig.time().unwrap_or_default();
    let epoch = gix_time.seconds;
    let offset_seconds = gix_time.offset;
    let naive = chrono::DateTime::from_timestamp(epoch, 0)
        .unwrap_or_default()
        .naive_utc();
    let offset = chrono::FixedOffset::east_opt(offset_seconds)
        .unwrap_or(chrono::FixedOffset::east_opt(0).unwrap());
    let dt = chrono::DateTime::<chrono::FixedOffset>::from_naive_utc_and_offset(naive, offset);
    CommitSignature {
        name: sig.name.to_string(),
        email: sig.email.to_string(),
        date_iso: dt.to_rfc3339(),
        date_epoch: epoch,
    }
}

/// Split a raw commit message into (subject, body).
///
/// The subject is the first non-empty line. The body is everything after the
/// first blank line following the subject, trimmed. Returns `None` for body
/// when the message has no body paragraph.
fn split_commit_message(message: &str) -> (String, Option<String>) {
    let mut lines = message.lines();
    let subject = lines.next().unwrap_or("").trim().to_string();
    // Skip blank lines separating subject from body
    let body_lines: Vec<&str> = lines.skip_while(|l| l.trim().is_empty()).collect();
    let body = if body_lines.is_empty() {
        None
    } else {
        Some(body_lines.join("\n").trim().to_string())
    };
    (subject, body)
}

/// Handle `gh pr diff <N>` by resolving the PR's base..head ref range via the
/// `gh` CLI, then feeding it through the existing manifest pipeline.
///
/// Execs `gh pr view <N> --json baseRefOid,headRefOid` to obtain the commit
/// SHAs, then calls `handle_manifest` with `"base_sha..head_sha"`.
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
/// calling `gh pr view <N> --json baseRefOid,headRefOid`.
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
    fn it_handles_show_snapshot_and_returns_commit_metadata() {
        let (_dir, path) = init_repo_with_two_commits();
        let sha = head_sha(&path);
        let classification = Classification::ShowSnapshot { sha: &sha };
        let mut out = Vec::new();
        let code = handle(&classification, &path, &mut out);
        assert_eq!(code, ExitCode::SUCCESS);

        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        // Enriched show response must include a top-level "commit" object.
        let commit = json
            .get("commit")
            .expect("expected 'commit' key in show output");
        assert!(
            commit.get("sha").and_then(|v| v.as_str()).is_some(),
            "commit.sha must be present, got: {json}"
        );
        assert!(
            commit.get("short_sha").and_then(|v| v.as_str()).is_some(),
            "commit.short_sha must be present"
        );
        assert!(
            commit.get("subject").and_then(|v| v.as_str()).is_some(),
            "commit.subject must be present"
        );
        let author = commit.get("author").expect("commit.author must be present");
        assert!(
            author.get("name").and_then(|v| v.as_str()).is_some(),
            "commit.author.name must be present"
        );
        assert!(
            author.get("email").and_then(|v| v.as_str()).is_some(),
            "commit.author.email must be present"
        );
        assert!(
            author.get("date_epoch").and_then(|v| v.as_i64()).is_some(),
            "commit.author.date_epoch must be an integer"
        );
        // Diffstat must be present.
        let diffstat = json
            .get("diffstat")
            .expect("expected 'diffstat' key in show output");
        assert!(
            diffstat
                .get("files_changed")
                .and_then(|v| v.as_u64())
                .is_some(),
            "diffstat.files_changed must be present"
        );
    }

    #[test]
    fn it_handles_show_snapshot_commit_sha_is_full_40_chars() {
        let (_dir, path) = init_repo_with_two_commits();
        let sha = head_sha(&path);
        let classification = Classification::ShowSnapshot { sha: &sha };
        let mut out = Vec::new();
        handle(&classification, &path, &mut out);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let commit_sha = json["commit"]["sha"].as_str().unwrap();
        assert_eq!(commit_sha.len(), 40, "sha must be 40 hex chars");
        let short_sha = json["commit"]["short_sha"].as_str().unwrap();
        assert_eq!(short_sha.len(), 8, "short_sha must be 8 hex chars");
        assert!(
            commit_sha.starts_with(short_sha),
            "short_sha must be a prefix of sha"
        );
    }

    #[test]
    fn it_handles_show_snapshot_subject_matches_commit_message() {
        let (_dir, path) = init_repo_with_two_commits();
        let sha = head_sha(&path);
        let classification = Classification::ShowSnapshot { sha: &sha };
        let mut out = Vec::new();
        handle(&classification, &path, &mut out);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        // The fixture commits "second commit" as HEAD.
        assert_eq!(json["commit"]["subject"].as_str().unwrap(), "second commit");
        // Single-line message — body must be absent (null).
        assert!(
            json["commit"]["body"].is_null(),
            "single-line commit must have null body"
        );
    }

    #[test]
    fn it_handles_show_snapshot_parents_array_has_one_entry_for_normal_commit() {
        let (_dir, path) = init_repo_with_two_commits();
        let sha = head_sha(&path);
        let classification = Classification::ShowSnapshot { sha: &sha };
        let mut out = Vec::new();
        handle(&classification, &path, &mut out);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let parents = json["commit"]["parents"].as_array().unwrap();
        assert_eq!(
            parents.len(),
            1,
            "non-root commit must have exactly one parent"
        );
        assert_eq!(
            parents[0].as_str().unwrap().len(),
            40,
            "parent sha must be 40 chars"
        );
    }

    #[test]
    fn it_handles_show_snapshot_author_epoch_is_positive_integer() {
        let (_dir, path) = init_repo_with_two_commits();
        let sha = head_sha(&path);
        let classification = Classification::ShowSnapshot { sha: &sha };
        let mut out = Vec::new();
        handle(&classification, &path, &mut out);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let epoch = json["commit"]["author"]["date_epoch"].as_i64().unwrap();
        assert!(epoch > 0, "date_epoch must be a positive unix timestamp");
    }

    #[test]
    fn it_handles_show_snapshot_committer_fields_present() {
        let (_dir, path) = init_repo_with_two_commits();
        let sha = head_sha(&path);
        let classification = Classification::ShowSnapshot { sha: &sha };
        let mut out = Vec::new();
        handle(&classification, &path, &mut out);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let committer = &json["commit"]["committer"];
        assert!(committer.get("name").and_then(|v| v.as_str()).is_some());
        assert!(committer.get("email").and_then(|v| v.as_str()).is_some());
        assert!(committer.get("date_iso").and_then(|v| v.as_str()).is_some());
        assert!(
            committer
                .get("date_epoch")
                .and_then(|v| v.as_i64())
                .is_some()
        );
    }

    #[test]
    fn it_handles_show_snapshot_diffstat_files_changed_matches_files_array() {
        let (_dir, path) = init_repo_with_two_commits();
        let sha = head_sha(&path);
        let classification = Classification::ShowSnapshot { sha: &sha };
        let mut out = Vec::new();
        handle(&classification, &path, &mut out);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let files_changed = json["diffstat"]["files_changed"].as_u64().unwrap();
        let files_count = json["files"].as_array().unwrap().len() as u64;
        assert_eq!(
            files_changed, files_count,
            "diffstat.files_changed must equal files array length"
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

    #[test]
    fn it_handles_show_snapshot_for_annotated_tag_without_error() {
        // Before the peel fix, `git show <annotated-tag>` would panic or exit
        // non-zero because peel_to_commit failed with "was kind tag". This test
        // confirms the handler completes successfully (exit SUCCESS, valid JSON
        // with a "files" array) after the fix.
        let (_dir, path) = init_repo_with_two_commits();

        Command::new("git")
            .args(["tag", "-a", "v1.0", "-m", "release v1.0"])
            .current_dir(&path)
            .output()
            .unwrap();

        let classification = Classification::ShowSnapshot { sha: "v1.0" };
        let mut out = Vec::new();
        let code = handle(&classification, &path, &mut out);
        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "handle should exit SUCCESS for annotated tag, got non-zero"
        );

        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert!(
            json.get("files").and_then(|f| f.as_array()).is_some(),
            "expected 'files' array in show output for annotated tag, got: {json}"
        );
    }

    // ===== ADVERSARIAL QA PROBES (issue #337 pen-test) =====

    // SUSPICIOUS (pre-existing, NOT a #337 regression): `git show <ref>` via the
    // shim always returns files: [] because handle_show_snapshot calls
    // build_snapshots with an empty `paths` slice (&[]). build_snapshots only
    // processes files explicitly listed in `paths`; it never enumerates the
    // changed files of the range. This is true for annotated tags, lightweight
    // tags, AND raw SHAs identically — the empty `&[]` predates this PR
    // (origin/main:src/shim/handlers.rs:134). The peel change is therefore
    // correct and unaffected here; the dev's
    // it_handles_show_snapshot_for_annotated_tag_without_error test passes only
    // because the snapshot is empty for everything, which masks whether the
    // peel actually surfaces the right commit's content. Reported as SUSPICIOUS,
    // not blocked, because it is out of scope for the #337 peel fix.

    /// Regression guard within #337 scope: `git show <annotated-tag>` and
    /// `git show <target-sha>` produce IDENTICAL output (both empty today, but
    /// must not diverge — peeling must map the tag to the same range the SHA
    /// produces). This stays green and protects the equivalence the peel change
    /// is supposed to guarantee.
    #[test]
    fn qa_show_annotated_tag_output_equals_target_sha_output() {
        let (_dir, path) = init_repo_with_two_commits();
        Command::new("git")
            .args(["tag", "-a", "v1.0", "-m", "release v1.0"])
            .current_dir(&path)
            .output()
            .unwrap();
        let target_sha = head_sha(&path);

        let mut out_tag = Vec::new();
        assert_eq!(
            handle(
                &Classification::ShowSnapshot { sha: "v1.0" },
                &path,
                &mut out_tag
            ),
            ExitCode::SUCCESS
        );
        let mut out_sha = Vec::new();
        assert_eq!(
            handle(
                &Classification::ShowSnapshot { sha: &target_sha },
                &path,
                &mut out_sha
            ),
            ExitCode::SUCCESS
        );

        let json_tag: serde_json::Value = serde_json::from_slice(&out_tag).unwrap();
        let json_sha: serde_json::Value = serde_json::from_slice(&out_sha).unwrap();
        // Compare the `files` arrays (metadata.base_ref/head_ref legitimately
        // differ: "v1.0^"/"v1.0" vs "<sha>^"/"<sha>"; generated_at also differs).
        assert_eq!(
            json_tag.get("files"),
            json_sha.get("files"),
            "annotated-tag show must produce the same files as target-sha show"
        );
    }

    /// The `Classification::Manifest` path peels BOTH ends of a range.  This
    /// test creates two annotated tags on consecutive commits and asserts that
    /// the manifest over `v0.1..v0.2` returns a non-empty `files` array that
    /// contains the file added in the second commit.  A "was kind tag" peel
    /// failure would surface here as either an error exit or an empty file list.
    #[test]
    fn qa_manifest_over_annotated_tag_range_returns_changed_files() {
        let (dir, path) = init_repo_with_two_commits();

        let git = |args: &[&str]| {
            Command::new("git")
                .args(args)
                .current_dir(&path)
                .output()
                .unwrap()
        };

        // v0.1 tags the first commit (HEAD~1 after init_repo_with_two_commits)
        git(&["tag", "-a", "v0.1", "-m", "v0.1 release", "HEAD~1"]);
        // v0.2 tags the current HEAD (second commit, which added world.txt)
        git(&["tag", "-a", "v0.2", "-m", "v0.2 release", "HEAD"]);

        let classification = Classification::Manifest {
            range: "v0.1..v0.2",
        };
        let mut out = Vec::new();
        let code = handle(&classification, &path, &mut out);
        assert_eq!(
            code,
            ExitCode::SUCCESS,
            "manifest over annotated tag range must exit SUCCESS"
        );

        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        let files = json
            .get("files")
            .and_then(|f| f.as_array())
            .expect("expected 'files' array in manifest output");

        assert!(
            !files.is_empty(),
            "manifest over v0.1..v0.2 must be non-empty (expected world.txt)"
        );

        let paths: Vec<&str> = files
            .iter()
            .filter_map(|f| f.get("path").and_then(|p| p.as_str()))
            .collect();
        assert!(
            paths.contains(&"world.txt"),
            "expected world.txt in manifest files, got: {paths:?}"
        );

        drop(dir);
    }
}
