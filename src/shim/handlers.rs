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
use crate::tools::size::estimate_response_tokens;
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
        // Use the resolved commit SHA (not the raw `sha` input) so that
        // annotated tags — where `sha` is e.g. "v1.0" and "v1.0^" is not a
        // resolvable ref — still produce a valid parent ref.
        let manifest_options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };
        let base_ref = format!("{}^", commit.sha);
        let manifest =
            collect_all_manifest_pages(repo_path, &base_ref, &commit.sha, &manifest_options, 500)?;
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

    let mut result = ShowManifestResponse {
        commit,
        diffstat,
        files,
        token_estimate: 0,
    };
    result.token_estimate = estimate_response_tokens(&result);
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

    let author = signature_to_commit_signature(&author_sig)?;
    let committer = signature_to_commit_signature(&committer_sig)?;

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
///
/// Returns an error instead of silently substituting epoch 0 so callers
/// see a real failure instead of corrupted `%ct`-equivalent data.
fn signature_to_commit_signature(
    sig: &gix::actor::SignatureRef<'_>,
) -> anyhow::Result<CommitSignature> {
    let gix_time = sig
        .time()
        .map_err(|e| anyhow::anyhow!("failed to parse commit time for {}: {e}", sig.email))?;
    let epoch = gix_time.seconds;
    let offset_seconds = gix_time.offset;
    let naive = chrono::DateTime::from_timestamp(epoch, 0)
        .ok_or_else(|| anyhow::anyhow!("commit timestamp {epoch} out of representable range"))?
        .naive_utc();
    let offset = chrono::FixedOffset::east_opt(offset_seconds)
        .ok_or_else(|| anyhow::anyhow!("commit tz offset {offset_seconds}s out of range"))?;
    let dt = chrono::DateTime::<chrono::FixedOffset>::from_naive_utc_and_offset(naive, offset);
    Ok(CommitSignature {
        name: sig.name.to_string(),
        email: sig.email.to_string(),
        date_iso: dt.to_rfc3339(),
        date_epoch: epoch,
    })
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

/// Check whether `sha` is present in the local object store at `repo_path`.
/// If it is absent, fetch it from `origin` using the real git binary at
/// `real_git`.
///
/// Used by tests to exercise the fetch path with a concrete SHA.  Production
/// code calls [`ensure_sha_present_for_pr`] which fetches via the more
/// reliable `refs/pull/<N>/head` refspec.
fn ensure_sha_present(
    repo_path: &std::path::Path,
    head_sha: &str,
    real_git: &std::path::Path,
) -> anyhow::Result<()> {
    // Use gix to check object presence — avoids a subprocess for the common case.
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow::anyhow!("failed to open repo at {}: {e}", repo_path.display()))?;

    if repo.rev_parse_single(head_sha).is_ok() {
        return Ok(());
    }

    // SHA absent locally — fetch it directly by SHA from origin.
    // Note: bare SHA fetch requires uploadpack.allowReachableSHA1InWant on the
    // server; when the PR number is known, prefer ensure_sha_present_for_pr.
    let status = std::process::Command::new(real_git)
        .args(["fetch", "origin", head_sha])
        .current_dir(repo_path)
        .env("GIT_PRISM_INSIDE_SHIM", "1")
        .status()
        .map_err(|e| anyhow::anyhow!("failed to spawn git fetch: {e}"))?;

    if status.success() {
        return Ok(());
    }

    anyhow::bail!(
        "git fetch origin {head_sha} failed with status {status} — \
         check network connectivity and that 'origin' remote is configured"
    )
}

/// Ensure `head_sha` is present locally, fetching `refs/pull/<pr_number>/head`
/// from `origin` via the real git binary if needed.
///
/// NOTE: This is the sanctioned shim-scoped exception to the gix-only rule.
/// The shim already hard-depends on and execs the real git binary for
/// passthrough; requiring it for a fetch adds no new runtime dependency.
/// A pure-gix fetch would require enabling `blocking-network-client` /
/// `async-network-client` Cargo features that this project deliberately omits.
///
/// The `refs/pull/<N>/head` refspec covers both same-repo and fork PRs because
/// GitHub populates it for every PR regardless of where the head branch lives.
fn ensure_sha_present_for_pr(
    repo_path: &std::path::Path,
    pr_number: &str,
    head_sha: &str,
    real_git: &std::path::Path,
) -> anyhow::Result<()> {
    // Fast path: SHA already present.
    let repo = gix::open(repo_path)
        .map_err(|e| anyhow::anyhow!("failed to open repo at {}: {e}", repo_path.display()))?;
    if repo.rev_parse_single(head_sha).is_ok() {
        return Ok(());
    }

    // Fetch via PR refspec — covers same-repo and fork PRs.
    let pr_refspec = format!("refs/pull/{pr_number}/head");
    let status = std::process::Command::new(real_git)
        .args(["fetch", "origin", &pr_refspec])
        .current_dir(repo_path)
        .env("GIT_PRISM_INSIDE_SHIM", "1")
        .status()
        .map_err(|e| anyhow::anyhow!("failed to spawn git fetch: {e}"))?;

    if status.success() {
        return Ok(());
    }

    anyhow::bail!(
        "git fetch origin {pr_refspec} failed with status {status} — \
         check network connectivity and that 'origin' remote is configured"
    )
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

    // Extract the head SHA from "base_sha..head_sha" so we can check local
    // object presence before calling handle_manifest.
    let head_sha = range
        .split("..")
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("unexpected range format from resolve_pr_range: {range}"))?;

    // Locate the real git binary for the potential fetch.
    // find_real_git() uses current_exe() + $PATH so it works outside passthrough.
    if let Some(real_git) = crate::shim::real_git::find_real_git() {
        ensure_sha_present_for_pr(&git_root, pr_number, head_sha, &real_git)?;
    } else {
        // No real git found — attempt the manifest anyway; if the object is
        // missing the manifest pipeline will surface a clear "Could not find
        // ref" error that tells the user what to do.
        tracing::warn!(
            "git-prism shim: could not locate real git binary; \
             skipping pre-fetch for PR #{pr_number}"
        );
    }

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
        let commit = json
            .get("commit")
            .expect("expected 'commit' key in show output");
        // Exact-value assertions (F7: strengthen weak shape checks).
        assert_eq!(
            commit["author"]["name"].as_str().unwrap(),
            "Test",
            "commit.author.name must be 'Test'"
        );
        assert_eq!(
            commit["author"]["email"].as_str().unwrap(),
            "test@test.com",
            "commit.author.email must be 'test@test.com'"
        );
        // The fixture adds world.txt in the second commit (1 file, 1 insertion).
        let diffstat = json
            .get("diffstat")
            .expect("expected 'diffstat' key in show output");
        assert_eq!(
            diffstat["files_changed"].as_u64().unwrap(),
            1,
            "diffstat.files_changed must be 1"
        );
        assert_eq!(
            diffstat["insertions"].as_u64().unwrap(),
            1,
            "diffstat.insertions must be 1"
        );
        assert_eq!(
            diffstat["deletions"].as_u64().unwrap(),
            0,
            "diffstat.deletions must be 0"
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
        // Exact-value assertions — the fixture sets user.name=Test, user.email=test@test.com.
        assert_eq!(committer["name"].as_str().unwrap(), "Test");
        assert_eq!(committer["email"].as_str().unwrap(), "test@test.com");
        // date_iso must be parseable as RFC-3339.
        let date_iso = committer["date_iso"]
            .as_str()
            .expect("date_iso must be a string");
        chrono::DateTime::parse_from_rfc3339(date_iso)
            .unwrap_or_else(|e| panic!("date_iso '{date_iso}' must parse as RFC-3339: {e}"));
        // date_epoch must be a positive unix timestamp.
        assert!(
            committer["date_epoch"].as_i64().unwrap() > 0,
            "date_epoch must be a positive unix timestamp"
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

    // ---- ensure_sha_present tests ----

    /// Build a bare remote + clone fixture where the clone is missing one commit.
    ///
    /// Returns (remote_dir, clone_dir, missing_sha):
    ///   - remote has two commits
    ///   - clone has only the first commit (head SHA is absent locally)
    fn make_clone_missing_head() -> (TempDir, TempDir, String) {
        let remote_dir = TempDir::new().unwrap();
        let remote_path = remote_dir.path().to_path_buf();

        // Create a bare remote with two commits.
        let git = |args: &[&str], cwd: &std::path::Path| {
            Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap()
        };
        git(&["init", "--bare", "-b", "main"], &remote_path);

        // Work repo to push two commits into the bare remote.
        let work_dir = TempDir::new().unwrap();
        let work_path = work_dir.path().to_path_buf();
        git(&["init", "-b", "main"], &work_path);
        git(&["config", "user.email", "t@t.com"], &work_path);
        git(&["config", "user.name", "T"], &work_path);
        git(
            &["remote", "add", "origin", &remote_path.to_string_lossy()],
            &work_path,
        );
        std::fs::write(work_path.join("a.txt"), "hello\n").unwrap();
        git(&["add", "a.txt"], &work_path);
        git(&["commit", "-m", "first"], &work_path);
        git(&["push", "origin", "main"], &work_path);

        std::fs::write(work_path.join("b.txt"), "world\n").unwrap();
        git(&["add", "b.txt"], &work_path);
        git(&["commit", "-m", "second"], &work_path);

        // Capture the head SHA (second commit) before pushing.
        let head_out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&work_path)
            .output()
            .unwrap();
        let head_sha = String::from_utf8(head_out.stdout)
            .unwrap()
            .trim()
            .to_string();

        // Push the second commit to the remote.
        git(&["push", "origin", "main"], &work_path);

        // Get the SHA of the first commit so we can clone shallowly at that ref.
        let first_sha_out = Command::new("git")
            .args(["rev-parse", "HEAD~1"])
            .current_dir(&work_path)
            .output()
            .unwrap();
        let first_sha = String::from_utf8(first_sha_out.stdout)
            .unwrap()
            .trim()
            .to_string();

        // Clone shallowly at the first commit only — the second commit's objects
        // are never downloaded, so head_sha is genuinely absent from the clone.
        let clone_dir = TempDir::new().unwrap();
        let clone_path = clone_dir.path().to_path_buf();
        Command::new("git")
            .args([
                "clone",
                "--depth=1",
                "--branch",
                "main",
                &remote_path.to_string_lossy(),
                &clone_path.to_string_lossy(),
            ])
            .output()
            .unwrap();

        // Reset the shallow clone to the first commit. The shallow clone already
        // has HEAD at the tip (second commit) — reset back to first_sha so the
        // second commit object was never needed.
        // Actually with --depth=1 we only get the latest commit (second commit).
        // We need a clone that stopped BEFORE the second commit. The reliable
        // approach: clone fully but then expire reflog + gc to remove the object.
        // First expire the reflog to make the second commit unreachable:
        Command::new("git")
            .args(["update-ref", "-d", "refs/remotes/origin/main"])
            .current_dir(&clone_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["update-ref", "-d", "refs/heads/main"])
            .current_dir(&clone_path)
            .output()
            .unwrap();
        // Point HEAD at first commit instead.
        Command::new("git")
            .args(["update-ref", "refs/heads/main", &first_sha])
            .current_dir(&clone_path)
            .output()
            .unwrap();
        // Expire all reflogs so gc can prune the second commit.
        Command::new("git")
            .args(["reflog", "expire", "--expire=now", "--all"])
            .current_dir(&clone_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["gc", "--prune=now", "--aggressive"])
            .current_dir(&clone_path)
            .output()
            .unwrap();

        (remote_dir, clone_dir, head_sha)
    }

    #[test]
    fn ensure_sha_present_fetches_missing_object_and_succeeds() {
        let (remote_dir, clone_dir, head_sha) = make_clone_missing_head();
        let clone_path = clone_dir.path();

        // Confirm the SHA is genuinely absent before calling ensure_sha_present.
        let check = Command::new("git")
            .args(["cat-file", "-e", &head_sha])
            .current_dir(clone_path)
            .status()
            .unwrap();
        assert!(
            !check.success(),
            "head SHA must be absent from the clone before the fetch"
        );

        let real_git = crate::shim::real_git::find_real_git()
            .expect("real git binary must be locatable for this test");

        ensure_sha_present(clone_path, &head_sha, &real_git)
            .expect("ensure_sha_present must succeed when objects are fetchable");

        // Confirm the SHA is now present.
        let check_after = Command::new("git")
            .args(["cat-file", "-e", &head_sha])
            .current_dir(clone_path)
            .status()
            .unwrap();
        assert!(
            check_after.success(),
            "head SHA must be present after ensure_sha_present"
        );

        drop(remote_dir);
        drop(clone_dir);
    }

    #[test]
    fn ensure_sha_present_is_noop_when_sha_already_present() {
        let (_dir, path) = init_repo_with_two_commits();
        let sha = head_sha(&path);

        let real_git = crate::shim::real_git::find_real_git()
            .expect("real git binary must be locatable for this test");

        // Must not error even though no remote fetch is needed.
        ensure_sha_present(&path, &sha, &real_git)
            .expect("ensure_sha_present must be a no-op when SHA is already present");
    }

    #[test]
    fn ensure_sha_present_returns_error_when_fetch_fails() {
        // A repo with no remote configured — fetch must fail with a clear error.
        let (_dir, path) = init_repo_with_two_commits();
        // Use a fake SHA that is absent locally.
        let fake_sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

        let real_git = crate::shim::real_git::find_real_git()
            .expect("real git binary must be locatable for this test");

        let result = ensure_sha_present(&path, fake_sha, &real_git);
        assert!(
            result.is_err(),
            "ensure_sha_present must return an error when fetch fails (no remote)"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("fetch") || msg.contains("failed"),
            "error message must mention fetch failure, got: {msg}"
        );
    }

    // ===== ADVERSARIAL QA PROBES (issue #349 pen-test) =====

    /// ensure_sha_present_for_pr: SHA already present → genuine no-op, no fetch.
    /// (A repo with NO remote configured: if the function tried to fetch it
    /// would error; success proves the fast path returned before any fetch.)
    #[test]
    fn qa_ensure_sha_present_for_pr_is_noop_when_sha_present_no_remote() {
        let (_dir, path) = init_repo_with_two_commits();
        let sha = head_sha(&path);
        let real_git = crate::shim::real_git::find_real_git()
            .expect("real git binary must be locatable for this test");
        // No `origin` remote exists. If this attempted a fetch it would fail;
        // it must short-circuit on the present-SHA fast path instead.
        ensure_sha_present_for_pr(&path, "349", &sha, &real_git)
            .expect("must be a no-op when the SHA is already present, even with no remote");
    }

    /// ensure_sha_present_for_pr: origin exists (local bare remote) but the
    /// requested PR ref `refs/pull/<N>/head` does NOT exist (closed/deleted PR,
    /// or wrong number). The fetch must fail with a clear, actionable error —
    /// not hang and not silently succeed.
    #[test]
    fn qa_ensure_sha_present_for_pr_errors_when_pr_ref_absent() {
        // Build a clone that is missing a SHA, with a working `origin` remote,
        // but ask for a PR refspec that the remote does not publish.
        let (remote_dir, clone_dir, missing_sha) = make_clone_missing_head();
        let clone_path = clone_dir.path();

        let real_git = crate::shim::real_git::find_real_git()
            .expect("real git binary must be locatable for this test");

        // PR 999999 has no refs/pull/999999/head on this bare remote.
        let result = ensure_sha_present_for_pr(clone_path, "999999", &missing_sha, &real_git);

        assert!(
            result.is_err(),
            "fetch of a non-existent PR ref must return an error, not succeed"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("refs/pull/999999/head") && msg.contains("failed"),
            "error must name the failing refspec and indicate failure, got: {msg}"
        );

        drop(remote_dir);
        drop(clone_dir);
    }

    /// ensure_sha_present_for_pr: no `origin` remote at all → clear error,
    /// non-zero (Err), no hang.
    #[test]
    fn qa_ensure_sha_present_for_pr_errors_when_no_origin_remote() {
        let (_dir, path) = init_repo_with_two_commits();
        let fake_sha = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let real_git = crate::shim::real_git::find_real_git()
            .expect("real git binary must be locatable for this test");

        let result = ensure_sha_present_for_pr(&path, "1", fake_sha, &real_git);
        assert!(
            result.is_err(),
            "fetch with no origin remote must return an error"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("origin") || msg.contains("failed"),
            "error must be actionable (mention origin/failure), got: {msg}"
        );
    }

    /// ensure_sha_present_for_pr SUCCESS via PR refspec: a local bare remote
    /// that publishes `refs/pull/<N>/head` pointing at the missing commit.
    /// Proves the production fetch path actually retrieves the object and that
    /// the test stays hermetic (local bare remote, never github.com).
    #[test]
    fn qa_ensure_sha_present_for_pr_fetches_via_pr_refspec() {
        let (remote_dir, clone_dir, missing_sha) = make_clone_missing_head();
        let remote_path = remote_dir.path();
        let clone_path = clone_dir.path();

        // Publish refs/pull/42/head -> missing_sha on the bare remote.
        // The remote already has the object (it was pushed there); we only need
        // to create the ref that GitHub would normally create for a PR.
        Command::new("git")
            .args(["update-ref", "refs/pull/42/head", &missing_sha])
            .current_dir(remote_path)
            .output()
            .unwrap();

        // Confirm absent locally first.
        let check = Command::new("git")
            .args(["cat-file", "-e", &missing_sha])
            .current_dir(clone_path)
            .status()
            .unwrap();
        assert!(!check.success(), "SHA must be absent before the fetch");

        let real_git = crate::shim::real_git::find_real_git()
            .expect("real git binary must be locatable for this test");

        ensure_sha_present_for_pr(clone_path, "42", &missing_sha, &real_git)
            .expect("fetch via refs/pull/42/head must retrieve the missing object");

        let check_after = Command::new("git")
            .args(["cat-file", "-e", &missing_sha])
            .current_dir(clone_path)
            .status()
            .unwrap();
        assert!(
            check_after.success(),
            "SHA must be present after fetching the PR refspec"
        );

        drop(remote_dir);
        drop(clone_dir);
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

    // --- Item 5: split_commit_message unit tests ---

    #[test]
    fn split_commit_message_single_line_has_no_body() {
        let (subject, body) = split_commit_message("fix: correct the thing");
        assert_eq!(subject, "fix: correct the thing");
        assert!(body.is_none());
    }

    #[test]
    fn split_commit_message_subject_and_single_paragraph_body() {
        let (subject, body) = split_commit_message("feat: add widget\n\nThis adds the widget.");
        assert_eq!(subject, "feat: add widget");
        assert_eq!(body.as_deref(), Some("This adds the widget."));
    }

    #[test]
    fn split_commit_message_multi_paragraph_body_preserves_internal_blank_line() {
        let msg = "feat: multi\n\nParagraph one.\n\nParagraph two.";
        let (subject, body) = split_commit_message(msg);
        assert_eq!(subject, "feat: multi");
        let b = body.expect("body must be Some");
        assert!(b.contains("Paragraph one."), "body must contain first para");
        assert!(
            b.contains("Paragraph two."),
            "body must contain second para"
        );
    }

    #[test]
    fn split_commit_message_trailing_blank_lines_only_gives_no_body() {
        let (subject, body) = split_commit_message("fix: thing\n\n  \n  ");
        assert_eq!(subject, "fix: thing");
        assert!(body.is_none(), "trailing blanks only must yield None body");
    }

    #[test]
    fn split_commit_message_empty_string_gives_empty_subject_no_body() {
        let (subject, body) = split_commit_message("");
        assert_eq!(subject, "");
        assert!(body.is_none());
    }

    #[test]
    fn split_commit_message_trims_subject_whitespace() {
        let (subject, _) = split_commit_message("  padded subject  \n\nbody");
        assert_eq!(subject, "padded subject");
    }

    // --- Item 6: e2e show with multi-paragraph body ---

    #[test]
    fn it_handles_show_snapshot_multi_paragraph_body_is_present_and_not_in_subject() {
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
        std::fs::write(path.join("a.txt"), "a\n").unwrap();
        run(&["add", "a.txt"]);
        // Commit with subject + two body paragraphs via multiple -m flags.
        run(&[
            "commit",
            "-m",
            "feat: the subject",
            "-m",
            "First paragraph of body.",
            "-m",
            "Second paragraph of body.",
        ]);
        let sha = head_sha(&path);
        let classification = Classification::ShowSnapshot { sha: &sha };
        let mut out = Vec::new();
        let code = handle(&classification, &path, &mut out);
        assert_eq!(code, ExitCode::SUCCESS);
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            json["commit"]["subject"].as_str().unwrap(),
            "feat: the subject",
            "subject must not contain body paragraphs"
        );
        let body = json["commit"]["body"]
            .as_str()
            .expect("body must be non-null for multi-paragraph commit");
        assert!(
            body.contains("First paragraph"),
            "body must contain first paragraph"
        );
        assert!(
            body.contains("Second paragraph"),
            "body must contain second paragraph"
        );
        assert!(
            !body.contains("feat: the subject"),
            "subject must not leak into body"
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
