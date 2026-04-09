use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error(
        "Not a git repository at '{0}'. Run git-prism from inside a git repo, or use --repo to specify one."
    )]
    OpenRepo(String),

    #[error("Could not find ref '{0}'. Check that the branch, tag, or SHA exists.")]
    ResolveRef(String),

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
    pub(crate) fn repo(&self) -> &gix::Repository {
        &self.repo
    }

    pub fn open(path: &std::path::Path) -> Result<Self, GitError> {
        let _span = tracing::info_span!("git.open_repo").entered();
        // Raw gix error omitted from user-facing message — it contains internal
        // paths and format that aren't actionable for the caller.
        let repo = gix::open(path).map_err(|_| GitError::OpenRepo(path.display().to_string()))?;
        Ok(Self { repo })
    }

    pub fn resolve_commit(&self, refspec: &str) -> Result<CommitInfo, GitError> {
        let _span = tracing::info_span!("git.resolve_ref").entered();
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
        let _span = tracing::info_span!("git.read_blob").entered();
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

    pub fn read_blob(&self, hex_id: &str) -> Result<String, GitError> {
        let id = gix::ObjectId::from_hex(hex_id.as_bytes())
            .map_err(|e| GitError::ReadObject(e.to_string()))?;
        let obj = self
            .repo
            .find_object(id)
            .map_err(|e| GitError::ReadObject(e.to_string()))?;
        std::str::from_utf8(&obj.data)
            .map(|s| s.to_string())
            .map_err(|e| GitError::ReadObject(e.to_string()))
    }

    pub fn commit_author(&self, refspec: &str) -> Result<String, GitError> {
        let commit = self.peel_to_commit(refspec)?;
        let author = commit
            .author()
            .map_err(|e| GitError::ReadObject(e.to_string()))?;
        Ok(author.name.to_string())
    }

    pub fn commit_timestamp(&self, refspec: &str) -> Result<String, GitError> {
        let commit = self.peel_to_commit(refspec)?;
        let author = commit
            .author()
            .map_err(|e| GitError::ReadObject(e.to_string()))?;
        Ok(author.time.to_string())
    }

    pub fn walk_commits(
        &self,
        base_ref: &str,
        head_ref: &str,
    ) -> Result<Vec<CommitInfo>, GitError> {
        let _span = tracing::info_span!("git.walk_commits").entered();
        let base_commit = self.peel_to_commit(base_ref)?;
        let head_commit = self.peel_to_commit(head_ref)?;

        let base_id = base_commit.id();
        let mut current = head_commit;
        let mut commits = Vec::new();

        loop {
            let info = CommitInfo {
                sha: current.id().to_string(),
                message: current
                    .message_raw()
                    .map_err(|e| GitError::ReadObject(e.to_string()))?
                    .to_string()
                    .trim()
                    .to_string(),
            };

            if current.id() == base_id {
                break;
            }

            commits.push(info);

            let parent = current
                .parent_ids()
                .next()
                .ok_or_else(|| {
                    GitError::ReadObject(format!(
                        "commit {} has no parent but base {} not yet reached",
                        current.id(),
                        base_id
                    ))
                })?
                .object()
                .map_err(|e| GitError::ReadObject(e.to_string()))?
                .try_into_commit()
                .map_err(|e| GitError::ReadObject(e.to_string()))?;

            current = parent;
        }

        commits.reverse();
        Ok(commits)
    }

    pub fn list_files_at_ref(&self, refspec: &str) -> Result<Vec<String>, GitError> {
        let _span = tracing::info_span!("git.list_files").entered();
        let commit = self.peel_to_commit(refspec)?;
        let tree = commit
            .tree()
            .map_err(|e| GitError::ReadObject(e.to_string()))?;

        let mut files = Vec::new();
        Self::walk_tree(&self.repo, &tree, String::new(), &mut files)?;
        files.sort();
        Ok(files)
    }

    fn walk_tree(
        repo: &gix::Repository,
        tree: &gix::Tree<'_>,
        prefix: String,
        files: &mut Vec<String>,
    ) -> Result<(), GitError> {
        for entry_ref in tree.iter() {
            let entry = entry_ref.map_err(|e| GitError::ReadObject(e.to_string()))?;
            let name = entry.filename().to_string();
            let path = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let mode = entry.mode();
            if mode.is_tree() {
                let obj = entry
                    .object()
                    .map_err(|e| GitError::ReadObject(e.to_string()))?;
                let sub_tree = obj
                    .try_into_tree()
                    .map_err(|e| GitError::ReadObject(e.to_string()))?;
                // Borrow workaround: pass repo explicitly
                let sub_tree_ref = repo
                    .find_object(sub_tree.id)
                    .map_err(|e| GitError::ReadObject(e.to_string()))?
                    .try_into_tree()
                    .map_err(|e| GitError::ReadObject(e.to_string()))?;
                Self::walk_tree(repo, &sub_tree_ref, path, files)?;
            } else if mode.is_blob() {
                files.push(path);
            }
        }
        Ok(())
    }

    pub(crate) fn peel_to_commit(&self, refspec: &str) -> Result<gix::Commit<'_>, GitError> {
        let rev = self
            .repo
            .rev_parse_single(refspec)
            // Raw gix error omitted — see OpenRepo for rationale.
            .map_err(|_| GitError::ResolveRef(refspec.to_string()))?;

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

    // Each OpenRepo error test creates its own TempDir — intentionally not extracted
    // because the setup is a one-liner and each test asserts a distinct facet.
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

    #[test]
    fn it_walks_commits_in_range_returning_chronological_order() {
        let (_dir, path) = create_test_repo();

        // Add two more commits (3 total with initial)
        std::fs::write(path.join("file2.txt"), "content2\n").unwrap();
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

        std::fs::write(path.join("file3.txt"), "content3\n").unwrap();
        Command::new("git")
            .args(["add", "file3.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "third commit"])
            .current_dir(&path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let commits = reader.walk_commits("HEAD~2", "HEAD").unwrap();

        assert_eq!(commits.len(), 2);
        // Chronological: second commit first, third commit last
        assert_eq!(commits[0].message, "second commit");
        assert_eq!(commits[1].message, "third commit");
    }

    #[test]
    fn it_walks_single_commit_range() {
        let (_dir, path) = create_test_repo();

        std::fs::write(path.join("file2.txt"), "content2\n").unwrap();
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
        let commits = reader.walk_commits("HEAD~1", "HEAD").unwrap();

        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].message, "second commit");
    }

    #[test]
    fn it_returns_empty_when_base_equals_head() {
        let (_dir, path) = create_test_repo();
        let reader = RepoReader::open(&path).unwrap();
        let commits = reader.walk_commits("HEAD", "HEAD").unwrap();
        assert!(commits.is_empty());
    }

    #[test]
    fn resolve_ref_error_says_could_not_find_ref() {
        let (_dir, path) = create_test_repo();
        let reader = RepoReader::open(&path).unwrap();
        let err = reader.resolve_commit("nonexistent-branch").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Could not find ref"),
            "expected 'Could not find ref' in: {msg}"
        );
    }

    #[test]
    fn resolve_ref_error_includes_ref_name() {
        let (_dir, path) = create_test_repo();
        let reader = RepoReader::open(&path).unwrap();
        let err = reader.resolve_commit("nonexistent-branch").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("nonexistent-branch"),
            "expected ref name in: {msg}"
        );
    }

    #[test]
    fn resolve_ref_error_suggests_checking_ref_exists() {
        let (_dir, path) = create_test_repo();
        let reader = RepoReader::open(&path).unwrap();
        let err = reader.resolve_commit("nonexistent-branch").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("branch, tag, or SHA"),
            "expected suggestion in: {msg}"
        );
    }

    // --- Gap-closing tests for mutation testing ---

    // Kill mutant: line 105 replace commit_timestamp -> Result<String, GitError> with Ok("xyzzy".into())
    #[test]
    fn it_returns_real_timestamp_not_placeholder() {
        let (_dir, path) = create_test_repo();
        let reader = RepoReader::open(&path).unwrap();
        let timestamp = reader.commit_timestamp("HEAD").unwrap();
        assert_ne!(
            timestamp, "xyzzy",
            "commit_timestamp must return a real timestamp"
        );
        // A real git timestamp contains digits (unix epoch seconds)
        assert!(
            timestamp.chars().any(|c| c.is_ascii_digit()),
            "timestamp should contain digits, got: {timestamp}"
        );
    }

    #[test]
    fn it_lists_files_at_ref() {
        let (_dir, path) = create_test_repo();

        // Add a nested file
        std::fs::create_dir_all(path.join("src")).unwrap();
        std::fs::write(path.join("src/main.rs"), "fn main() {}").unwrap();
        Command::new("git")
            .args(["add", "src/main.rs"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add source file"])
            .current_dir(&path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let files = reader.list_files_at_ref("HEAD").unwrap();
        assert!(files.contains(&"README.md".to_string()));
        assert!(files.contains(&"src/main.rs".to_string()));
        assert_eq!(files.len(), 2);
    }
}
