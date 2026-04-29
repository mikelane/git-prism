pub mod context;
pub mod history;
pub mod import_scope;
pub mod manifest;
pub mod review_change;
pub mod size;
pub mod snapshots;
pub mod types;

pub use context::{ContextOptions, build_function_context_with_options};
pub use history::build_history;
pub use manifest::{build_manifest, build_worktree_manifest, enforce_token_budget};
pub use review_change::{ReviewChangeArgs, ReviewChangeResponse, build_review_change};
pub use snapshots::build_snapshots;
pub use types::{
    ContextArgs, FunctionContextResponse, HistoryArgs, HistoryResponse, ManifestArgs,
    ManifestOptions, ManifestResponse, SnapshotArgs, SnapshotOptions, SnapshotResponse,
};

use std::path::Path;

use crate::pagination::{self, decode_cursor};

/// Assemble paginated manifest pages into a single combined [`ManifestResponse`],
/// then apply token-budget enforcement on the combined result.
///
/// `fetch_page` is called with `(offset, page_size)` and must return one page.
/// The first call is always at offset 0; subsequent calls use the cursor offset
/// from the previous page's `pagination.next_cursor`.
///
/// Budget enforcement is intentionally deferred to the combined response:
/// per-page enforcement would cap each page individually but the caller would
/// re-assemble them all, defeating the budget.
pub fn collect_manifest_pages(
    fetch_page: impl Fn(usize, usize) -> anyhow::Result<ManifestResponse>,
    options: &ManifestOptions,
    page_size: usize,
) -> anyhow::Result<ManifestResponse> {
    let page_size = pagination::clamp_page_size(page_size);

    let mut all_files = Vec::new();

    let first_page = fetch_page(0, page_size)?;
    all_files.extend(first_page.files);

    let mut next_cursor = first_page.pagination.next_cursor.clone();
    while let Some(ref cursor_str) = next_cursor {
        let cursor = decode_cursor(cursor_str)?;
        let page = fetch_page(cursor.offset, page_size)?;
        all_files.extend(page.files);
        next_cursor = page.pagination.next_cursor.clone();
    }

    let total_items = all_files.len();
    let mut response = ManifestResponse {
        metadata: first_page.metadata,
        summary: first_page.summary,
        files: all_files,
        dependency_changes: first_page.dependency_changes,
        pagination: pagination::PaginationInfo {
            total_items,
            page_start: 0,
            page_size: total_items,
            next_cursor: None,
        },
    };

    if let Some(budget) = options.max_response_tokens.filter(|&b| b > 0)
        && options.include_function_analysis
    {
        let trimmed = enforce_token_budget(&mut response, budget);
        response.metadata.function_analysis_truncated = trimmed;
        response.pagination.page_size = response.files.len();
    }
    response.metadata.token_estimate = size::estimate_response_tokens(&response);

    Ok(response)
}

/// Collect all manifest pages for a commit-to-commit range into a single
/// combined response.
pub fn collect_all_manifest_pages(
    repo_path: &Path,
    base: &str,
    head: &str,
    options: &ManifestOptions,
    page_size: usize,
) -> anyhow::Result<ManifestResponse> {
    let collection_options = ManifestOptions {
        max_response_tokens: None,
        ..options.clone()
    };
    collect_manifest_pages(
        |offset, ps| {
            build_manifest(repo_path, base, head, &collection_options, offset, ps)
                .map_err(anyhow::Error::from)
        },
        options,
        page_size,
    )
}

/// Collect all manifest pages in worktree mode into a single combined response.
pub fn collect_all_worktree_manifest_pages(
    repo_path: &Path,
    base: &str,
    options: &ManifestOptions,
    page_size: usize,
) -> anyhow::Result<ManifestResponse> {
    let collection_options = ManifestOptions {
        max_response_tokens: None,
        ..options.clone()
    };
    collect_manifest_pages(
        |offset, ps| {
            build_worktree_manifest(repo_path, base, &collection_options, offset, ps)
                .map_err(anyhow::Error::from)
        },
        options,
        page_size,
    )
}

/// Collect all history pages into a single combined response.
pub fn collect_all_history_pages(
    repo_path: &Path,
    base: &str,
    head: &str,
    options: &ManifestOptions,
    page_size: usize,
) -> anyhow::Result<HistoryResponse> {
    let page_size = pagination::clamp_page_size(page_size);
    let mut all_commits = Vec::new();

    let first_page = build_history(repo_path, base, head, options, 0, page_size)?;
    all_commits.extend(first_page.commits);

    let mut next_cursor = first_page.pagination.next_cursor.clone();
    while let Some(ref cursor_str) = next_cursor {
        let cursor = decode_cursor(cursor_str)?;
        let page = build_history(repo_path, base, head, options, cursor.offset, page_size)?;
        all_commits.extend(page.commits);
        next_cursor = page.pagination.next_cursor.clone();
    }

    let total_items = all_commits.len();
    Ok(HistoryResponse {
        commits: all_commits,
        pagination: pagination::PaginationInfo {
            total_items,
            page_start: 0,
            page_size: total_items,
            next_cursor: None,
        },
    })
}

/// Extract the file extension from a path.
///
/// Returns the substring after the final `.`, or `""` when the path has no
/// extension or the "extension" is actually a dotfile basename like
/// `.gitignore`. The `path.len() > ext.len() + 1` guard rejects the
/// dotfile case: `.gitignore` splits as `("", "gitignore")` but the full
/// path length equals the candidate extension length plus one character
/// for the leading dot, which means there's no real basename before it.
///
/// Used by the manifest and context tools to look up language analyzers.
/// `pub(crate)` rather than `pub` so the helper stays an internal
/// orchestration detail — downstream MCP clients should not depend on it.
pub(crate) fn extension_from_path(path: &str) -> &str {
    path.rsplit('.')
        .next()
        .filter(|ext| path.len() > ext.len() + 1)
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_from_path_returns_extension_for_simple_file() {
        assert_eq!(extension_from_path("lib.rs"), "rs");
    }

    #[test]
    fn extension_from_path_returns_last_extension_for_double_extension() {
        assert_eq!(extension_from_path("foo.test.ts"), "ts");
    }

    #[test]
    fn extension_from_path_returns_empty_for_no_extension() {
        assert_eq!(extension_from_path("Makefile"), "");
    }

    #[test]
    fn extension_from_path_returns_empty_for_empty_string() {
        assert_eq!(extension_from_path(""), "");
    }

    #[test]
    fn extension_from_path_handles_nested_path() {
        assert_eq!(extension_from_path("src/tools/context.rs"), "rs");
    }

    #[test]
    fn extension_from_path_returns_empty_for_dotfile() {
        // `.gitignore` splits as ("", "gitignore") but path.len() == ext.len() + 1
        // so the filter rejects it and returns "".
        assert_eq!(extension_from_path(".gitignore"), "");
    }

    // --- collect_all_manifest_pages / collect_all_history_pages tests ---

    use std::process::Command;
    use tempfile::TempDir;

    fn create_repo_with_many_files(file_count: usize) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        Command::new("git").args(["init", "--initial-branch=main"]).current_dir(&path).output().unwrap();
        Command::new("git").args(["config", "user.email", "test@test.com"]).current_dir(&path).output().unwrap();
        Command::new("git").args(["config", "user.name", "Test User"]).current_dir(&path).output().unwrap();
        std::fs::write(path.join("README.md"), "# base\n").unwrap();
        Command::new("git").args(["add", "."]).current_dir(&path).output().unwrap();
        Command::new("git").args(["commit", "-m", "base commit"]).current_dir(&path).output().unwrap();
        for i in 0..file_count {
            std::fs::write(path.join(format!("file_{i}.txt")), format!("content {i}\n")).unwrap();
        }
        Command::new("git").args(["add", "."]).current_dir(&path).output().unwrap();
        Command::new("git").args(["commit", "-m", "add many files"]).current_dir(&path).output().unwrap();
        (dir, path)
    }

    fn create_repo_with_many_commits(commit_count: usize) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        Command::new("git").args(["init", "--initial-branch=main"]).current_dir(&path).output().unwrap();
        Command::new("git").args(["config", "user.email", "test@test.com"]).current_dir(&path).output().unwrap();
        Command::new("git").args(["config", "user.name", "Test User"]).current_dir(&path).output().unwrap();
        std::fs::write(path.join("README.md"), "# anchor\n").unwrap();
        Command::new("git").args(["add", "."]).current_dir(&path).output().unwrap();
        Command::new("git").args(["commit", "-m", "anchor"]).current_dir(&path).output().unwrap();
        for i in 0..commit_count {
            std::fs::write(path.join(format!("file_{i}.txt")), format!("content {i}\n")).unwrap();
            Command::new("git").args(["add", "."]).current_dir(&path).output().unwrap();
            Command::new("git").args(["commit", "-m", &format!("commit {i}")]).current_dir(&path).output().unwrap();
        }
        (dir, path)
    }

    #[test]
    fn collect_all_manifest_pages_returns_all_files_when_single_page() {
        let (_dir, path) = create_repo_with_many_files(3);
        let options = ManifestOptions { include_patterns: vec![], exclude_patterns: vec![], include_function_analysis: false, max_response_tokens: None };
        let result = collect_all_manifest_pages(&path, "HEAD~1", "HEAD", &options, 500).unwrap();
        assert_eq!(result.files.len(), 3);
        assert!(result.pagination.next_cursor.is_none());
        assert_eq!(result.pagination.total_items, 3);
    }

    #[test]
    fn collect_all_manifest_pages_collects_across_multiple_pages() {
        let (_dir, path) = create_repo_with_many_files(5);
        let options = ManifestOptions { include_patterns: vec![], exclude_patterns: vec![], include_function_analysis: false, max_response_tokens: None };
        let result = collect_all_manifest_pages(&path, "HEAD~1", "HEAD", &options, 2).unwrap();
        assert_eq!(result.files.len(), 5);
        assert!(result.pagination.next_cursor.is_none());
        assert_eq!(result.pagination.total_items, 5);
    }

    #[test]
    fn collect_all_manifest_pages_preserves_metadata_from_first_page() {
        let (_dir, path) = create_repo_with_many_files(5);
        let options = ManifestOptions { include_patterns: vec![], exclude_patterns: vec![], include_function_analysis: false, max_response_tokens: None };
        let result = collect_all_manifest_pages(&path, "HEAD~1", "HEAD", &options, 2).unwrap();
        assert_eq!(result.metadata.base_ref, "HEAD~1");
        assert_eq!(result.metadata.head_ref, "HEAD");
        assert!(!result.metadata.base_sha.is_empty());
        assert!(!result.metadata.head_sha.is_empty());
    }

    #[test]
    fn collect_all_manifest_pages_with_page_size_1() {
        let (_dir, path) = create_repo_with_many_files(3);
        let options = ManifestOptions { include_patterns: vec![], exclude_patterns: vec![], include_function_analysis: false, max_response_tokens: None };
        let result = collect_all_manifest_pages(&path, "HEAD~1", "HEAD", &options, 1).unwrap();
        assert_eq!(result.files.len(), 3);
        assert!(result.pagination.next_cursor.is_none());
    }

    #[test]
    fn collect_all_history_pages_returns_all_commits_when_single_page() {
        let (_dir, path) = create_repo_with_many_commits(3);
        let options = ManifestOptions { include_patterns: vec![], exclude_patterns: vec![], include_function_analysis: false, max_response_tokens: None };
        let result = collect_all_history_pages(&path, "HEAD~3", "HEAD", &options, 500).unwrap();
        assert_eq!(result.commits.len(), 3);
        assert!(result.pagination.next_cursor.is_none());
        assert_eq!(result.pagination.total_items, 3);
    }

    #[test]
    fn collect_all_history_pages_collects_across_multiple_pages() {
        let (_dir, path) = create_repo_with_many_commits(5);
        let options = ManifestOptions { include_patterns: vec![], exclude_patterns: vec![], include_function_analysis: false, max_response_tokens: None };
        let result = collect_all_history_pages(&path, "HEAD~5", "HEAD", &options, 2).unwrap();
        assert_eq!(result.commits.len(), 5);
        assert!(result.pagination.next_cursor.is_none());
        assert_eq!(result.pagination.total_items, 5);
    }

    #[test]
    fn collect_all_history_pages_preserves_commit_order() {
        let (_dir, path) = create_repo_with_many_commits(4);
        let options = ManifestOptions { include_patterns: vec![], exclude_patterns: vec![], include_function_analysis: false, max_response_tokens: None };
        let result = collect_all_history_pages(&path, "HEAD~4", "HEAD", &options, 2).unwrap();
        assert_eq!(result.commits.len(), 4);
        assert_eq!(result.commits[0].metadata.message, "commit 0");
        assert_eq!(result.commits[1].metadata.message, "commit 1");
        assert_eq!(result.commits[2].metadata.message, "commit 2");
        assert_eq!(result.commits[3].metadata.message, "commit 3");
    }

    #[test]
    fn collect_all_history_pages_with_page_size_1() {
        let (_dir, path) = create_repo_with_many_commits(3);
        let options = ManifestOptions { include_patterns: vec![], exclude_patterns: vec![], include_function_analysis: false, max_response_tokens: None };
        let result = collect_all_history_pages(&path, "HEAD~3", "HEAD", &options, 1).unwrap();
        assert_eq!(result.commits.len(), 3);
        assert!(result.pagination.next_cursor.is_none());
    }
}
