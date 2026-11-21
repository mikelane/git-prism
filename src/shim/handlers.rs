//! Adapters from `Classification` variants to `tools::*` functions.
//!
//! Each handler resolves the repository via `gix::discover`, calls the
//! corresponding tool function, serialises the result to stdout, and returns
//! `Ok(())`.

use std::io::Write;
use std::path::Path;

use crate::git::reader::RepoReader;
use crate::git::refs::RefRange;
use crate::git::refs::parse_range;
use crate::shim::classify::Classification;
use crate::tools::size::estimate_response_tokens;
use crate::tools::types::{
    CommitSignature, ShowCommitDetail, ShowDiffstat, ShowFileEntry, ShowManifestResponse,
};
use crate::tools::{
    ManifestOptions, SnapshotOptions, build_snapshots, collect_all_history_pages,
    collect_all_manifest_pages, collect_all_worktree_manifest_pages,
};

/// Dispatch a classified git command to the appropriate tool function and
/// write the JSON result to `out`.
///
/// Errors are returned, never printed: the caller falls back to real git, so
/// no shim text may reach stderr.  `argv` is the original git command line.
pub(crate) fn handle<W: Write>(
    classification: &Classification<'_>,
    repo_path: &Path,
    argv: &[&str],
    out: &mut W,
) -> anyhow::Result<()> {
    dispatch(classification, repo_path, argv, out).inspect_err(|e| {
        tracing::debug!(
            error_kind = crate::privacy::classify_error_kind(&format!("{e:#}")),
            error = %e,
            "shim handler failed; falling back to real git"
        );
    })
}

const HINT_KEY: &str = "git_prism_hint";

/// Write `value` as pretty JSON with a top-level hint telling the reader how
/// to get the raw git output the shim replaced.  Shim-only: MCP and CLI
/// output never pass through here.
fn write_with_hint<W: Write, T: serde::Serialize>(
    out: &mut W,
    value: &T,
    argv: &[&str],
) -> anyhow::Result<()> {
    let mut json = serde_json::to_value(value)?;
    let raw_git_args = argv
        .get(1..)
        .unwrap_or_default()
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let hint =
        format!("git-prism structured output. For raw git output run: command git {raw_git_args}");
    json.as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("shim output is not a JSON object"))?
        .insert(HINT_KEY.to_string(), serde_json::Value::String(hint));
    serde_json::to_writer_pretty(out, &json)?;
    Ok(())
}

/// POSIX-single-quote `arg` unless it is made only of characters no shell
/// treats specially, so the hint can be pasted back as the same command.
fn shell_quote(arg: &str) -> String {
    let is_plain = |c: char| c.is_ascii_alphanumeric() || "_-./:=,@%+~".contains(c);
    // A leading `=` triggers zsh `=cmd` path lookup. A `~` expands to the home
    // directory at the start of a word and, in bash, after `=` or `:`, so it is
    // plain only directly after an alphanumeric (`HEAD~1`).
    let has_expanding_tilde = arg
        .char_indices()
        .any(|(i, c)| c == '~' && !arg[..i].ends_with(|prev: char| prev.is_ascii_alphanumeric()));
    let needs_quoting =
        arg.is_empty() || arg.starts_with('=') || has_expanding_tilde || !arg.chars().all(is_plain);
    if needs_quoting {
        format!("'{}'", arg.replace('\'', r"'\''"))
    } else {
        arg.to_string()
    }
}

fn dispatch<W: Write>(
    classification: &Classification<'_>,
    repo_path: &Path,
    argv: &[&str],
    out: &mut W,
) -> anyhow::Result<()> {
    match classification {
        Classification::Manifest { range } => handle_manifest(range, repo_path, argv, out),
        Classification::History { range } => handle_history(range, repo_path, argv, out),
        Classification::ShowSnapshot { sha } => handle_show_snapshot(sha, repo_path, argv, out),
        Classification::BlameSnapshot { path } => handle_blame_snapshot(path, repo_path, argv, out),
        Classification::Passthrough => {
            anyhow::bail!("dispatch called with Passthrough — caller bug")
        }
    }
}

fn handle_manifest<W: Write>(
    range: &str,
    repo_path: &Path,
    argv: &[&str],
    out: &mut W,
) -> anyhow::Result<()> {
    let options = ManifestOptions {
        include_patterns: vec![],
        exclude_patterns: vec![],
        include_function_analysis: false,
        max_response_tokens: Some(8192),
    };
    let manifest = match parse_range(range) {
        RefRange::CommitRange { base, head } => {
            collect_all_manifest_pages(repo_path, base, head, &options, 500)?
        }
        RefRange::WorktreeCompare { base } => {
            collect_all_worktree_manifest_pages(repo_path, base, &options, 500)?
        }
    };
    write_with_hint(out, &manifest, argv)?;
    Ok(())
}

fn handle_history<W: Write>(
    range: &str,
    repo_path: &Path,
    argv: &[&str],
    out: &mut W,
) -> anyhow::Result<()> {
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
    let history = collect_all_history_pages(repo_path, base, head, &options, 500)?;
    write_with_hint(out, &history, argv)?;
    Ok(())
}

fn handle_show_snapshot<W: Write>(
    sha: &str,
    repo_path: &Path,
    argv: &[&str],
    out: &mut W,
) -> anyhow::Result<()> {
    let commit = build_show_commit_detail(repo_path, sha)?;
    if commit.parents.len() > 1 {
        // Real git shows a combined diff for a merge commit; a first-parent
        // manifest would list files git omits. Decline so git answers.
        anyhow::bail!("{sha} is a merge commit; real git shows a combined diff");
    }

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

    let mut show_response = ShowManifestResponse {
        commit,
        diffstat,
        files,
        token_estimate: 0,
    };
    show_response.token_estimate = estimate_response_tokens(&show_response);
    write_with_hint(out, &show_response, argv)?;
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

/// True when the working-tree file at `path` is byte-identical to its HEAD blob.
///
/// Real git blame reads the working-tree file, so only an exact match lets the
/// HEAD snapshot stand in for it. Reading the file directly (rather than asking
/// `gix status`) stays correct for skip-worktree, assume-unchanged and
/// sparse-checkout paths, which status skips.
///
/// The path is resolved in the HEAD tree first, so a path that is not a
/// regular-file blob at HEAD (absent, outside the repository, a tracked
/// symlink) never touches the filesystem. The on-disk entry is then `lstat`ed:
/// a symlink, FIFO, device or directory declines (real git reads a link with
/// `readlink` and refuses a FIFO outright; opening either could follow or
/// block). The read is bounded to the HEAD blob size plus one byte, which also
/// closes the window between the `lstat` and the open. Any difference or error
/// declines.
fn working_tree_matches_head(repo_path: &Path, path: &str) -> anyhow::Result<bool> {
    if !is_plain_repo_relative(path) {
        return Ok(false);
    }
    let reader =
        RepoReader::open(repo_path).map_err(|e| anyhow::anyhow!("failed to open repo: {e}"))?;
    let Some(head_blob) = head_regular_file_bytes(&reader, path)? else {
        return Ok(false);
    };
    if content_conversion_may_apply(reader.repo(), path) {
        return Ok(false);
    }
    Ok(working_tree_file_equals(&repo_path.join(path), &head_blob))
}

/// Gitattributes that make git rewrite working-tree content before blaming it
/// (`convert_to_git`): eol normalization, clean filters, ident expansion and
/// re-encoding. `crlf` is the legacy spelling of `text`/`eol`.
const CONVERSION_ATTRIBUTES: [&str; 6] = [
    "text",
    "eol",
    "crlf",
    "filter",
    "ident",
    "working-tree-encoding",
];

/// True when git might convert the working-tree file before blaming it, in
/// which case raw bytes cannot be compared with the HEAD blob.
///
/// Declining is always safe, so any doubt (an attribute lookup that fails, a
/// `core.eol` setting, `core.autocrlf` other than false) answers true.
fn content_conversion_may_apply(repo: &gix::Repository, path: &str) -> bool {
    config_enables_conversion(repo) || attributes_enable_conversion(repo, path).unwrap_or(true)
}

fn config_enables_conversion(repo: &gix::Repository) -> bool {
    let config = repo.config_snapshot();
    let autocrlf_enabled =
        config.string("core.autocrlf").is_some() && config.boolean("core.autocrlf") != Some(false);
    autocrlf_enabled || config.string("core.eol").is_some()
}

fn attributes_enable_conversion(repo: &gix::Repository, path: &str) -> anyhow::Result<bool> {
    use gix::worktree::stack::state::attributes::Source;
    let index = repo.index_or_empty()?;
    let mut stack = repo.attributes_only(&index, Source::WorktreeThenIdMapping)?;
    let mut outcome = stack.selected_attribute_matches(CONVERSION_ATTRIBUTES);
    let platform = stack.at_entry(path, Some(gix::index::entry::Mode::FILE))?;
    platform.matching_attributes(&mut outcome);
    Ok(outcome
        .iter_selected()
        .any(|m| !m.assignment.state.is_unspecified()))
}

fn is_plain_repo_relative(path: &str) -> bool {
    use std::path::Component;
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

/// The HEAD blob's bytes when `path` is a regular (or executable) file there.
fn head_regular_file_bytes(reader: &RepoReader, path: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let tree = reader
        .peel_to_commit("HEAD")
        .and_then(|commit| {
            commit
                .tree()
                .map_err(|e| crate::git::reader::GitError::ReadObject(e.to_string()))
        })
        .map_err(|e| anyhow::anyhow!("failed to read HEAD tree: {e}"))?;
    let Some(entry) = tree
        .lookup_entry_by_path(path)
        .map_err(|e| anyhow::anyhow!("failed to look up {path} at HEAD: {e}"))?
    else {
        return Ok(None);
    };
    if !entry.mode().is_blob() {
        return Ok(None);
    }
    let blob = entry
        .object()
        .map_err(|e| anyhow::anyhow!("failed to read {path} at HEAD: {e}"))?;
    Ok(Some(blob.data.clone()))
}

fn working_tree_file_equals(on_disk_path: &Path, expected: &[u8]) -> bool {
    use std::io::Read;
    let expected_len = expected.len() as u64;
    let Ok(metadata) = std::fs::symlink_metadata(on_disk_path) else {
        return false;
    };
    if !metadata.file_type().is_file() || metadata.len() != expected_len {
        return false;
    }
    let Ok(file) = std::fs::File::open(on_disk_path) else {
        return false;
    };
    let mut on_disk = Vec::with_capacity(expected.len());
    file.take(expected_len + 1)
        .read_to_end(&mut on_disk)
        .is_ok()
        && on_disk == expected
}

fn handle_blame_snapshot<W: Write>(
    path: &str,
    repo_path: &Path,
    argv: &[&str],
    out: &mut W,
) -> anyhow::Result<()> {
    if !working_tree_matches_head(repo_path, path)? {
        // Real git blames the working-tree file, attributing local lines to
        // "Not Committed Yet" (or failing when it is absent); the HEAD
        // snapshot would misreport both.
        anyhow::bail!("{path} differs from HEAD on disk; real git blames the working tree");
    }
    // `git blame <path>` maps to get_file_snapshots for the whole file at HEAD.
    let options = SnapshotOptions {
        include_before: false,
        include_after: true,
        max_file_size_bytes: 100_000,
        line_range: None,
        include_diff_hunks: false,
    };
    let snapshots = build_snapshots(repo_path, "HEAD^", "HEAD", &[path.to_string()], &options)?;
    let path_missing_at_head = snapshots
        .files
        .iter()
        .any(|file| file.after.is_none() && file.error.is_none() && !file.is_binary);
    if path_missing_at_head {
        // Real git blame fails (exit 128) for a path absent at HEAD; decline so
        // the caller gets git's own error and exit code, not an empty success.
        anyhow::bail!("path {path} does not exist at HEAD");
    }
    write_with_hint(out, &snapshots, argv)?;
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
        handle(&classification, &path, &[], &mut out).unwrap();

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
        handle(&classification, &path, &[], &mut out).unwrap();

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
        handle(&classification, &path, &[], &mut out).unwrap();

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
        handle(&classification, &path, &[], &mut out).unwrap();

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
        handle(&classification, &path, &[], &mut out).unwrap();
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
        handle(&classification, &path, &[], &mut out).unwrap();
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
        handle(&classification, &path, &[], &mut out).unwrap();
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
        handle(&classification, &path, &[], &mut out).unwrap();
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
        handle(&classification, &path, &[], &mut out).unwrap();
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
        handle(&classification, &path, &[], &mut out).unwrap();
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
        handle(&classification, &path, &[], &mut out).unwrap();

        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        // build_snapshots returns a response with a "files" array.
        assert!(
            json.get("files").and_then(|f| f.as_array()).is_some(),
            "expected 'files' array in blame output, got: {json}"
        );
    }

    #[test]
    fn it_declines_blame_snapshot_for_a_path_missing_at_head() {
        let (_dir, path) = init_repo_with_two_commits();
        let classification = Classification::BlameSnapshot {
            path: "no-such-file.txt",
        };
        let mut out = Vec::new();

        let result = handle(&classification, &path, &[], &mut out);

        assert!(result.is_err(), "missing path must decline so git errors");
        assert!(
            out.is_empty(),
            "nothing may be written for a declined blame"
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
        handle(&classification, &path, &[], &mut out).unwrap();
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
    fn it_adds_a_raw_git_hint_to_intercepted_json() {
        let (_dir, path) = init_repo_with_two_commits();
        let classification = Classification::Manifest {
            range: "HEAD~1..HEAD",
        };
        let mut out = Vec::new();
        handle(&classification, &path, &["git", "diff", "A..B"], &mut out).unwrap();

        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            json["git_prism_hint"],
            "git-prism structured output. For raw git output run: command git diff A..B"
        );
    }

    const LOG_P_HINT: &str =
        "git-prism structured output. For raw git output run: command git log -p";

    fn hint_for(classification: &Classification<'_>, path: &Path) -> serde_json::Value {
        let mut out = Vec::new();
        handle(classification, path, &["git", "log", "-p"], &mut out).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&out).unwrap();
        json["git_prism_hint"].clone()
    }

    #[test]
    fn it_adds_the_hint_to_history_output() {
        let (_dir, path) = init_repo_with_two_commits();
        let classification = Classification::History {
            range: "HEAD~1..HEAD",
        };

        assert_eq!(hint_for(&classification, &path), LOG_P_HINT);
    }

    #[test]
    fn it_adds_the_hint_to_show_snapshot_output() {
        let (_dir, path) = init_repo_with_two_commits();
        let sha = head_sha(&path);
        let classification = Classification::ShowSnapshot { sha: &sha };

        assert_eq!(hint_for(&classification, &path), LOG_P_HINT);
    }

    #[test]
    fn it_adds_the_hint_to_blame_snapshot_output() {
        let (_dir, path) = init_repo_with_two_commits();
        let classification = Classification::BlameSnapshot { path: "world.txt" };

        assert_eq!(hint_for(&classification, &path), LOG_P_HINT);
    }

    #[test]
    fn it_leaves_tool_output_without_a_hint() {
        let (_dir, path) = init_repo_with_two_commits();
        let result = collect_all_manifest_pages(
            &path,
            "HEAD~1",
            "HEAD",
            &ManifestOptions {
                include_patterns: vec![],
                exclude_patterns: vec![],
                include_function_analysis: false,
                max_response_tokens: Some(8192),
            },
            500,
        )
        .unwrap();
        let json = serde_json::to_value(&result).unwrap();
        assert!(json.get("git_prism_hint").is_none());
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
        handle(&classification, &path, &[], &mut out)
            .expect("handle should succeed for annotated tag");

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
        handle(
            &Classification::ShowSnapshot { sha: "v1.0" },
            &path,
            &[],
            &mut out_tag,
        )
        .unwrap();
        let mut out_sha = Vec::new();
        handle(
            &Classification::ShowSnapshot { sha: &target_sha },
            &path,
            &[],
            &mut out_sha,
        )
        .unwrap();

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
        handle(&classification, &path, &[], &mut out)
            .expect("manifest over annotated tag range must succeed");

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

    #[test]
    fn it_leaves_plain_arguments_unquoted() {
        let args = ["diff", "HEAD~1..HEAD", "--name-only", "src/a_b.rs", "a=b,c"];
        assert_eq!(args.map(shell_quote), args.map(String::from));
    }

    #[test]
    fn it_single_quotes_arguments_a_shell_would_split_or_expand() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("~/x"), "'~/x'");
        assert_eq!(shell_quote("HEAD^"), "'HEAD^'");
        assert_eq!(shell_quote("$(rm -rf x)"), "'$(rm -rf x)'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn it_single_quotes_a_tilde_that_bash_expands_after_an_equals_sign_or_colon() {
        let args = ["k=~", "a:~", "x=~/y", "~", "a..~"];
        assert_eq!(args.map(shell_quote), args.map(|arg| format!("'{arg}'")));
    }

    #[test]
    fn it_single_quotes_every_character_that_expands_at_the_start_of_a_word() {
        let args = ["=ls", "~user", "{a,b}", "!!", "#x", "*.rs", "?x", "[ab]"];
        assert_eq!(args.map(shell_quote), args.map(|arg| format!("'{arg}'")));
    }
}
