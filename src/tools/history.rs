use std::path::Path;

use crate::git::reader::RepoReader;
use crate::pagination::{CURSOR_VERSION, PaginationCursor, PaginationInfo, encode_cursor};
use crate::tools::manifest::build_manifest;
use crate::tools::types::{
    CommitManifest, CommitMetadata, HistoryResponse, ManifestOptions, ToolError,
};

pub fn build_history(
    repo_path: &Path,
    base_ref: &str,
    head_ref: &str,
    options: &ManifestOptions,
    offset: usize,
    page_size: usize,
) -> Result<HistoryResponse, ToolError> {
    let reader = RepoReader::open(repo_path)?;
    let commit_infos = reader.walk_commits(base_ref, head_ref)?;
    let total_commits = commit_infos.len();

    let page_end = (offset + page_size).min(total_commits);
    // cargo-mutants: skip -- equivalent mutant: when offset == total_commits,
    // page_end clamps to total_commits, so &commit_infos[total_commits..total_commits]
    // is an empty slice — observably identical to the explicit `&[]` branch.
    // No black-box test can distinguish `<` from `<=` at this boundary.
    #[rustfmt::skip]
    let page_commits = if offset < total_commits { &commit_infos[offset..page_end] } else { &[] };

    let mut commits = Vec::new();

    for (page_idx, info) in page_commits.iter().enumerate() {
        let global_idx = offset + page_idx;
        let parent_ref = if global_idx == 0 {
            base_ref.to_string()
        } else {
            commit_infos[global_idx - 1].sha.clone()
        };

        let manifest = build_manifest(repo_path, &parent_ref, &info.sha, options, 0, 500)?;

        commits.push(CommitManifest {
            metadata: CommitMetadata {
                sha: info.sha.clone(),
                message: info.message.clone(),
                author: reader.commit_author(&info.sha)?,
                timestamp: reader.commit_timestamp(&info.sha)?,
            },
            files: manifest.files,
            summary: manifest.summary,
        });
    }

    let base_sha = reader.resolve_commit(base_ref)?.sha;
    let head_sha = reader.resolve_commit(head_ref)?.sha;

    let next_cursor = if page_end < total_commits {
        Some(encode_cursor(&PaginationCursor {
            version: CURSOR_VERSION,
            offset: page_end,
            base_sha,
            head_sha,
        }))
    } else {
        None
    };

    let pagination = PaginationInfo {
        total_items: total_commits,
        page_start: offset,
        page_size,
        next_cursor,
    };

    Ok(HistoryResponse {
        commits,
        pagination,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn create_repo_with_three_commits() -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();

        Command::new("git")
            .args(["init", "--initial-branch=main"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Anchor commit so HEAD~3 resolves
        std::fs::write(path.join("README.md"), "# anchor\n").unwrap();
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "anchor commit"])
            .current_dir(&path)
            .output()
            .unwrap();

        std::fs::write(path.join("file_a.txt"), "first version\n").unwrap();
        Command::new("git")
            .args(["add", "file_a.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "commit one"])
            .current_dir(&path)
            .output()
            .unwrap();

        std::fs::write(path.join("file_b.txt"), "second file\n").unwrap();
        Command::new("git")
            .args(["add", "file_b.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "commit two"])
            .current_dir(&path)
            .output()
            .unwrap();

        std::fs::write(path.join("file_a.txt"), "updated version\n").unwrap();
        Command::new("git")
            .args(["add", "file_a.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "commit three"])
            .current_dir(&path)
            .output()
            .unwrap();

        (dir, path)
    }

    #[test]
    fn it_produces_one_manifest_per_commit_in_range() {
        let (_dir, path) = create_repo_with_three_commits();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let history = build_history(&path, "HEAD~2", "HEAD", &options, 0, 500).unwrap();

        assert_eq!(history.commits.len(), 2);
    }

    #[test]
    fn it_returns_three_manifests_for_three_commit_range() {
        let (_dir, path) = create_repo_with_three_commits();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let history = build_history(&path, "HEAD~3", "HEAD", &options, 0, 500).unwrap();

        assert_eq!(history.commits.len(), 3);
        assert_eq!(history.commits[0].metadata.message, "commit one");
        assert_eq!(history.commits[1].metadata.message, "commit two");
        assert_eq!(history.commits[2].metadata.message, "commit three");
    }

    #[test]
    fn it_populates_commit_metadata_with_author_and_timestamp() {
        let (_dir, path) = create_repo_with_three_commits();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let history = build_history(&path, "HEAD~1", "HEAD", &options, 0, 500).unwrap();

        assert_eq!(history.commits.len(), 1);
        let commit = &history.commits[0];
        assert_eq!(commit.metadata.author, "Test User");
        assert!(!commit.metadata.sha.is_empty());
        assert!(!commit.metadata.timestamp.is_empty());
    }

    #[test]
    fn it_includes_correct_files_per_commit() {
        let (_dir, path) = create_repo_with_three_commits();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let history = build_history(&path, "HEAD~2", "HEAD", &options, 0, 500).unwrap();

        // Second commit added file_b.txt
        assert_eq!(history.commits[0].files.len(), 1);
        assert_eq!(history.commits[0].files[0].path, "file_b.txt");

        // Third commit modified file_a.txt
        assert_eq!(history.commits[1].files.len(), 1);
        assert_eq!(history.commits[1].files[0].path, "file_a.txt");
    }

    #[test]
    fn it_returns_error_for_invalid_base_ref() {
        let (_dir, path) = create_repo_with_three_commits();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let result = build_history(&path, "nonexistent", "HEAD", &options, 0, 500);
        assert!(result.is_err());
    }

    #[test]
    fn it_returns_error_for_invalid_head_ref() {
        let (_dir, path) = create_repo_with_three_commits();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let result = build_history(&path, "HEAD~1", "nonexistent", &options, 0, 500);
        assert!(result.is_err());
    }

    #[test]
    fn it_returns_empty_commits_when_base_equals_head() {
        let (_dir, path) = create_repo_with_three_commits();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let history = build_history(&path, "HEAD", "HEAD", &options, 0, 500).unwrap();
        assert!(history.commits.is_empty());
    }

    #[test]
    fn it_paginates_with_all_commits_on_single_page() {
        let (_dir, path) = create_repo_with_three_commits();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let history = build_history(&path, "HEAD~3", "HEAD", &options, 0, 10).unwrap();

        assert_eq!(history.commits.len(), 3);
        assert_eq!(history.pagination.total_items, 3);
        assert_eq!(history.pagination.page_start, 0);
        assert_eq!(history.pagination.page_size, 10);
        assert!(history.pagination.next_cursor.is_none());
    }

    #[test]
    fn it_paginates_first_page_with_cursor_when_more_commits_exist() {
        let (_dir, path) = create_repo_with_three_commits();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let history = build_history(&path, "HEAD~3", "HEAD", &options, 0, 2).unwrap();

        assert_eq!(history.commits.len(), 2);
        assert_eq!(history.commits[0].metadata.message, "commit one");
        assert_eq!(history.commits[1].metadata.message, "commit two");
        assert_eq!(history.pagination.total_items, 3);
        assert_eq!(history.pagination.page_start, 0);
        assert_eq!(history.pagination.page_size, 2);
        assert!(history.pagination.next_cursor.is_some());
    }

    #[test]
    fn it_returns_second_page_with_no_cursor_on_last_page() {
        let (_dir, path) = create_repo_with_three_commits();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let history = build_history(&path, "HEAD~3", "HEAD", &options, 2, 2).unwrap();

        assert_eq!(history.commits.len(), 1);
        assert_eq!(history.commits[0].metadata.message, "commit three");
        assert_eq!(history.pagination.total_items, 3);
        assert_eq!(history.pagination.page_start, 2);
        assert_eq!(history.pagination.page_size, 2);
        assert!(history.pagination.next_cursor.is_none());
    }

    #[test]
    fn it_returns_complete_files_in_each_paginated_commit() {
        let (_dir, path) = create_repo_with_three_commits();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let history = build_history(&path, "HEAD~3", "HEAD", &options, 0, 1).unwrap();

        assert_eq!(history.commits.len(), 1);
        assert_eq!(history.commits[0].files.len(), 1);
        assert_eq!(history.commits[0].files[0].path, "file_a.txt");
    }

    #[test]
    fn it_encodes_next_cursor_with_resolved_shas() {
        use crate::pagination::decode_cursor;

        let (_dir, path) = create_repo_with_three_commits();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let history = build_history(&path, "HEAD~3", "HEAD", &options, 0, 1).unwrap();

        let cursor_str = history.pagination.next_cursor.as_ref().unwrap();
        let cursor = decode_cursor(cursor_str).unwrap();
        assert_eq!(cursor.offset, 1);
        assert!(!cursor.base_sha.is_empty());
        assert!(!cursor.head_sha.is_empty());
        assert_eq!(cursor.version, 1);
    }

    #[test]
    fn it_includes_summary_counts_per_commit() {
        let (_dir, path) = create_repo_with_three_commits();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        let history = build_history(&path, "HEAD~1", "HEAD", &options, 0, 500).unwrap();

        assert_eq!(history.commits.len(), 1);
        let summary = &history.commits[0].summary;
        assert_eq!(summary.total_files_changed, 1);
        assert!(summary.total_lines_added > 0 || summary.total_lines_removed > 0);
    }
}
