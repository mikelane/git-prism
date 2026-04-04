use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error(
        "Not a git repository at '{0}'. Run git-prism from inside a git repo, or use --repo to specify one."
    )]
    OpenRepo(String),

    #[error("failed to resolve ref '{0}': {1}")]
    ResolveRef(String, String),

    #[error("failed to read object: {0}")]
    ReadObject(String),
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CommitInfo {
    pub sha: String,
    pub message: String,
}

#[derive(Debug)]
pub struct RepoReader {
    repo: gix::Repository,
}

impl RepoReader {
    pub fn open(path: &std::path::Path) -> Result<Self, GitError> {
        let repo = gix::open(path).map_err(|_| GitError::OpenRepo(path.display().to_string()))?;
        Ok(Self { repo })
    }

    pub fn resolve_commit(&self, refspec: &str) -> Result<CommitInfo, GitError> {
        let commit = self.peel_to_commit(refspec)?;

        let message = commit
            .message_raw()
            .map_err(|e| GitError::ReadObject(e.to_string()))?
            .to_string();

        Ok(CommitInfo {
            sha: commit.id().to_string(),
            message: message.trim().to_string(),
        })
    }

    pub fn read_file_at_ref(&self, refspec: &str, file_path: &str) -> Result<String, GitError> {
        let commit = self.peel_to_commit(refspec)?;

        let tree = commit
            .tree()
            .map_err(|e| GitError::ReadObject(e.to_string()))?;

        let entry = tree
            .lookup_entry_by_path(file_path)
            .map_err(|e| GitError::ReadObject(e.to_string()))?
            .ok_or_else(|| {
                GitError::ReadObject(format!(
                    "file '{}' not found at ref '{}'",
                    file_path, refspec
                ))
            })?;

        let blob = entry
            .object()
            .map_err(|e| GitError::ReadObject(e.to_string()))?;

        std::str::from_utf8(&blob.data)
            .map(|s| s.to_string())
            .map_err(|e| GitError::ReadObject(e.to_string()))
    }

    pub(crate) fn peel_to_commit(&self, refspec: &str) -> Result<gix::Commit<'_>, GitError> {
        let rev = self
            .repo
            .rev_parse_single(refspec)
            .map_err(|e| GitError::ResolveRef(refspec.to_string(), e.to_string()))?;

        let object = rev
            .object()
            .map_err(|e| GitError::ReadObject(e.to_string()))?;

        object
            .try_into_commit()
            .map_err(|e| GitError::ReadObject(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn create_test_repo() -> (TempDir, std::path::PathBuf) {
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
            .args(["config", "user.name", "Test"])
            .current_dir(&path)
            .output()
            .unwrap();

        std::fs::write(path.join("README.md"), "# Hello\n").unwrap();

        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(&path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["commit", "-m", "initial commit"])
            .current_dir(&path)
            .output()
            .unwrap();

        (dir, path)
    }

    #[test]
    fn it_opens_a_valid_git_repository() {
        let (_dir, path) = create_test_repo();
        let reader = RepoReader::open(&path);
        assert!(reader.is_ok());
    }

    #[test]
    fn it_returns_error_for_non_repository_path() {
        let dir = TempDir::new().unwrap();
        let reader = RepoReader::open(dir.path());
        assert!(reader.is_err());
    }

    #[test]
    fn open_repo_error_message_says_not_a_git_repository() {
        let dir = TempDir::new().unwrap();
        let err = RepoReader::open(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Not a git repository"),
            "expected 'Not a git repository' in: {msg}"
        );
    }

    #[test]
    fn open_repo_error_message_includes_path() {
        let dir = TempDir::new().unwrap();
        let err = RepoReader::open(dir.path()).unwrap_err();
        let msg = err.to_string();
        let expected_path = dir.path().display().to_string();
        assert!(
            msg.contains(&expected_path),
            "expected path '{expected_path}' in: {msg}"
        );
    }

    #[test]
    fn open_repo_error_message_suggests_repo_flag() {
        let dir = TempDir::new().unwrap();
        let err = RepoReader::open(dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--repo"), "expected '--repo' hint in: {msg}");
    }

    #[test]
    fn open_repo_error_for_nonexistent_path_includes_that_path() {
        let path = std::path::Path::new("/nonexistent/fake/path");
        let err = RepoReader::open(path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("/nonexistent/fake/path"),
            "expected path in: {msg}"
        );
        assert!(
            msg.contains("Not a git repository"),
            "expected 'Not a git repository' in: {msg}"
        );
    }

    #[test]
    fn it_resolves_head_to_a_commit() {
        let (_dir, path) = create_test_repo();
        let reader = RepoReader::open(&path).unwrap();
        let commit = reader.resolve_commit("HEAD").unwrap();
        assert!(!commit.sha.is_empty());
        assert_eq!(commit.message, "initial commit");
    }

    #[test]
    fn it_resolves_branch_name() {
        let (_dir, path) = create_test_repo();
        let reader = RepoReader::open(&path).unwrap();
        let commit = reader.resolve_commit("main").unwrap();
        assert_eq!(commit.message, "initial commit");
    }

    #[test]
    fn it_resolves_full_sha() {
        let (_dir, path) = create_test_repo();
        let reader = RepoReader::open(&path).unwrap();
        let head = reader.resolve_commit("HEAD").unwrap();
        let by_sha = reader.resolve_commit(&head.sha).unwrap();
        assert_eq!(head.sha, by_sha.sha);
    }

    #[test]
    fn it_resolves_head_tilde_n() {
        let (_dir, path) = create_test_repo();

        std::fs::write(path.join("file2.txt"), "content\n").unwrap();
        Command::new("git")
            .args(["add", "file2.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "second commit"])
            .current_dir(&path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let parent = reader.resolve_commit("HEAD~1").unwrap();
        assert_eq!(parent.message, "initial commit");
    }

    #[test]
    fn it_returns_error_for_invalid_ref() {
        let (_dir, path) = create_test_repo();
        let reader = RepoReader::open(&path).unwrap();
        let result = reader.resolve_commit("nonexistent-branch");
        assert!(result.is_err());
    }

    #[test]
    fn it_reads_file_content_at_ref() {
        let (_dir, path) = create_test_repo();
        let reader = RepoReader::open(&path).unwrap();
        let content = reader.read_file_at_ref("HEAD", "README.md").unwrap();
        assert_eq!(content, "# Hello\n");
    }

    #[test]
    fn it_returns_error_for_nonexistent_file_at_ref() {
        let (_dir, path) = create_test_repo();
        let reader = RepoReader::open(&path).unwrap();
        let result = reader.read_file_at_ref("HEAD", "nonexistent.txt");
        assert!(result.is_err());
    }
}
