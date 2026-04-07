use std::path::Path;

use crate::git::reader::RepoReader;
use crate::tools::manifest::build_manifest;
use crate::tools::types::{
    CommitManifest, CommitMetadata, HistoryResponse, ManifestOptions, ToolError,
};

pub fn build_history(
    repo_path: &Path,
    base_ref: &str,
    head_ref: &str,
    options: &ManifestOptions,
) -> Result<HistoryResponse, ToolError> {
    let reader = RepoReader::open(repo_path)?;
    let commit_infos = reader.walk_commits(base_ref, head_ref)?;

    let mut commits = Vec::new();

    for (i, info) in commit_infos.iter().enumerate() {
        let parent_ref = if i == 0 {
            base_ref.to_string()
        } else {
            commit_infos[i - 1].sha.clone()
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

    Ok(HistoryResponse { commits })
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
        };

        let history = build_history(&path, "HEAD~2", "HEAD", &options).unwrap();

        assert_eq!(history.commits.len(), 2);
    }

    #[test]
    fn it_returns_three_manifests_for_three_commit_range() {
        let (_dir, path) = create_repo_with_three_commits();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };

        let history = build_history(&path, "HEAD~3", "HEAD", &options).unwrap();

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
        };

        let history = build_history(&path, "HEAD~1", "HEAD", &options).unwrap();

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
        };

        let history = build_history(&path, "HEAD~2", "HEAD", &options).unwrap();

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
        };

        let result = build_history(&path, "nonexistent", "HEAD", &options);
        assert!(result.is_err());
    }

    #[test]
    fn it_returns_error_for_invalid_head_ref() {
        let (_dir, path) = create_repo_with_three_commits();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };

        let result = build_history(&path, "HEAD~1", "nonexistent", &options);
        assert!(result.is_err());
    }

    #[test]
    fn it_returns_empty_commits_when_base_equals_head() {
        let (_dir, path) = create_repo_with_three_commits();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };

        let history = build_history(&path, "HEAD", "HEAD", &options).unwrap();
        assert!(history.commits.is_empty());
    }

    #[test]
    fn it_includes_summary_counts_per_commit() {
        let (_dir, path) = create_repo_with_three_commits();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };

        let history = build_history(&path, "HEAD~1", "HEAD", &options).unwrap();

        assert_eq!(history.commits.len(), 1);
        let summary = &history.commits[0].summary;
        assert_eq!(summary.total_files_changed, 1);
        assert!(summary.total_lines_added > 0 || summary.total_lines_removed > 0);
    }
}
