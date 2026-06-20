use serde::Serialize;
use thiserror::Error;

#[derive(Debug)]
pub struct ResolveRefError {
    pub refspec: String,
    pub resolution: Option<String>,
}

impl std::fmt::Display for ResolveRefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let error_msg = format!(
            "Could not find ref '{}'. Check that the branch, tag, or SHA exists.",
            self.refspec
        );
        match &self.resolution {
            Some(res) => write!(
                f,
                "{}",
                serde_json::json!({"error": error_msg, "resolution": res})
            ),
            None => write!(f, "{error_msg}"),
        }
    }
}

#[derive(Debug, Error)]
pub enum GitError {
    #[error(
        "Not a git repository at '{0}'. Run git-prism from inside a git repo, or use --repo to specify one."
    )]
    OpenRepo(String),

    #[error("{0}")]
    ResolveRef(ResolveRefError),

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
                GitError::ReadObject(format!("file '{file_path}' not found at ref '{refspec}'"))
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

        let base_id = base_commit.id().detach();
        let head_id = head_commit.id().detach();

        // Use gix's revision walk with `with_hidden` to exclude base and all its
        // ancestors — equivalent to `git rev-list base..head`. This correctly
        // handles diverged history, merge commits (all parents), and disjoint
        // histories, unlike the old hand-rolled first-parent loop.
        //
        // `with_hidden` marks base as a "hidden tip": the walker traverses from
        // head but excludes any commit reachable from base (including base itself),
        // which is exactly the two-dot range semantics of `git rev-list base..head`.
        let walk = self
            .repo
            .rev_walk([head_id])
            .with_hidden([base_id])
            .all()
            .map_err(|e| GitError::ReadObject(e.to_string()))?;

        // gix default walk order is newest-first (BreadthFirst). Reverse to
        // oldest-first to match `git rev-list --reverse` and the existing test
        // contract.
        let mut commits = walk
            .map(|result| {
                let info = result.map_err(|e| GitError::ReadObject(e.to_string()))?;
                let message = info
                    .object()
                    .map_err(|e| GitError::ReadObject(e.to_string()))?
                    .message_raw()
                    .map_err(|e| GitError::ReadObject(e.to_string()))?
                    .to_string()
                    .trim()
                    .to_string();
                Ok(CommitInfo {
                    sha: info.id.to_string(),
                    message,
                })
            })
            .collect::<Result<Vec<_>, GitError>>()?;
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
            .map_err(|_| self.resolve_ref_with_fallback(refspec))?;

        let object = rev
            .object()
            .map_err(|e| GitError::ReadObject(e.to_string()))?;

        object
            .peel_to_commit()
            .map_err(|e| GitError::ReadObject(e.to_string()))
    }

    fn resolve_ref_with_fallback(&self, refspec: &str) -> GitError {
        let is_bare_branch = !refspec.contains('~')
            && !refspec.contains('^')
            && !refspec.contains(':')
            && !refspec.contains("@{")
            && !refspec.starts_with("refs/");

        if is_bare_branch {
            // Check all remotes (not just origin) for the tracking ref
            if let Some(remote) = self.find_remote_for_branch(refspec) {
                return GitError::ResolveRef(ResolveRefError {
                    refspec: refspec.to_string(),
                    resolution: Some(format!("git fetch {remote} {refspec}")),
                });
            }
        }

        GitError::ResolveRef(ResolveRefError {
            refspec: refspec.to_string(),
            resolution: None,
        })
    }

    fn find_remote_for_branch(&self, refspec: &str) -> Option<String> {
        let remotes_path = self.repo.path().join("refs").join("remotes");
        let dirs = std::fs::read_dir(remotes_path).ok()?;
        for entry in dirs.filter_map(|e| e.ok()) {
            let remote_name = entry.file_name().to_string_lossy().to_string();
            let remote_ref = format!("refs/remotes/{remote_name}/{refspec}");
            if self.repo.rev_parse_single(remote_ref.as_str()).is_ok() {
                return Some(remote_name);
            }
        }
        None
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
    fn it_suggests_fetch_when_branch_exists_on_origin() {
        let (local_dir, local_path) = create_test_repo();

        // Create a bare remote repo with branch "feature/foo"
        let remote_dir = TempDir::new().unwrap();
        let remote_path = remote_dir.path().to_path_buf();
        Command::new("git")
            .args(["init", "--bare"])
            .current_dir(&remote_path)
            .output()
            .unwrap();

        // Push local main to remote so refs/remotes/origin/feature/foo exists
        Command::new("git")
            .args(["remote", "add", "origin", remote_path.to_str().unwrap()])
            .current_dir(&local_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["push", "origin", "main"])
            .current_dir(&local_path)
            .output()
            .unwrap();

        // Create feature/foo on remote by pushing from local
        Command::new("git")
            .args(["checkout", "-b", "feature/foo"])
            .current_dir(&local_path)
            .output()
            .unwrap();
        std::fs::write(local_path.join("feature.txt"), "feature content\n").unwrap();
        Command::new("git")
            .args(["add", "feature.txt"])
            .current_dir(&local_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add feature"])
            .current_dir(&local_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["push", "-u", "origin", "feature/foo"])
            .current_dir(&local_path)
            .output()
            .unwrap();

        // Fetch to ensure remote tracking ref exists locally
        Command::new("git")
            .args(["fetch", "origin"])
            .current_dir(&local_path)
            .output()
            .unwrap();

        // Now go back to main and delete the local feature/foo branch
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(&local_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["branch", "-D", "feature/foo"])
            .current_dir(&local_path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&local_path).unwrap();
        let err = reader.resolve_commit("feature/foo").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("\"resolution\""),
            "expected JSON resolution field in: {msg}"
        );
        assert!(
            msg.contains("git fetch origin feature/foo"),
            "expected fetch suggestion in: {msg}"
        );

        drop(local_dir);
        drop(remote_dir);
    }

    #[test]
    fn it_resolves_branch_with_at_symbol_when_remote_tracking_exists() {
        let (local_dir, local_path) = create_test_repo();

        // Create a bare remote repo
        let remote_dir = TempDir::new().unwrap();
        let remote_path = remote_dir.path().to_path_buf();
        Command::new("git")
            .args(["init", "--bare"])
            .current_dir(&remote_path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["remote", "add", "origin", remote_path.to_str().unwrap()])
            .current_dir(&local_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["push", "origin", "main"])
            .current_dir(&local_path)
            .output()
            .unwrap();

        // Create branch with @ in name
        Command::new("git")
            .args(["checkout", "-b", "feature@team"])
            .current_dir(&local_path)
            .output()
            .unwrap();
        std::fs::write(local_path.join("feature.txt"), "feature content\n").unwrap();
        Command::new("git")
            .args(["add", "feature.txt"])
            .current_dir(&local_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add feature"])
            .current_dir(&local_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["push", "-u", "origin", "feature@team"])
            .current_dir(&local_path)
            .output()
            .unwrap();

        // Fetch to ensure remote tracking ref exists locally
        Command::new("git")
            .args(["fetch", "origin"])
            .current_dir(&local_path)
            .output()
            .unwrap();

        // Go back to main and delete the local branch
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(&local_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["branch", "-D", "feature@team"])
            .current_dir(&local_path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&local_path).unwrap();
        let err = reader.resolve_commit("feature@team").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("\"resolution\""),
            "expected JSON resolution field in: {msg}"
        );
        assert!(
            msg.contains("git fetch origin feature@team"),
            "expected fetch suggestion in: {msg}"
        );

        drop(local_dir);
        drop(remote_dir);
    }

    #[test]
    fn it_returns_plain_error_when_branch_not_found_anywhere() {
        let (_dir, path) = create_test_repo();
        let reader = RepoReader::open(&path).unwrap();
        let err = reader.resolve_commit("totally-unknown").unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains('{'),
            "expected plain error without JSON braces, got: {msg}"
        );
        assert!(
            !msg.contains("\"resolution\""),
            "expected no resolution field in: {msg}"
        );
    }

    #[test]
    fn it_returns_plain_error_for_sha() {
        let (_dir, path) = create_test_repo();
        let reader = RepoReader::open(&path).unwrap();
        let err = reader
            .resolve_commit("deadbeef1234567890abcdef1234567890abcdef12")
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains('{'),
            "expected plain error without JSON braces, got: {msg}"
        );
        assert!(
            !msg.contains("\"resolution\""),
            "expected no resolution field in: {msg}"
        );
    }

    #[test]
    fn it_returns_plain_error_for_qualified_ref() {
        let (_dir, path) = create_test_repo();
        let reader = RepoReader::open(&path).unwrap();
        let err = reader.resolve_commit("refs/heads/nonexistent").unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains('{'),
            "expected plain error without JSON braces, got: {msg}"
        );
        assert!(
            !msg.contains("\"resolution\""),
            "expected no resolution field in: {msg}"
        );
    }

    // ===== DIVERGED HISTORY TESTS (issue #382) =====

    /// Build a repo where main and a feature branch have DIVERGED — main advanced
    /// after the branch point so the main tip is NOT an ancestor of feature HEAD.
    ///
    /// Returns (dir, path, main_tip_sha, feature_head_sha).
    /// `main_tip_sha` is NOT reachable from `feature_head_sha` via parent walk,
    /// which is the condition that triggers the old walk_commits bug.
    fn create_diverged_repo() -> (TempDir, std::path::PathBuf, String, String) {
        let (dir, path) = create_test_repo();

        // Add a commit on main (this becomes the common ancestor)
        std::fs::write(path.join("shared.txt"), "shared\n").unwrap();
        git(&path, &["add", "shared.txt"]);
        git(&path, &["commit", "-m", "shared commit"]);

        // Create feature branch from common ancestor, add 2 commits
        git(&path, &["checkout", "-b", "feature"]);
        std::fs::write(path.join("feat1.txt"), "feat1\n").unwrap();
        git(&path, &["add", "feat1.txt"]);
        git(&path, &["commit", "-m", "feature commit 1"]);
        std::fs::write(path.join("feat2.txt"), "feat2\n").unwrap();
        git(&path, &["add", "feat2.txt"]);
        git(&path, &["commit", "-m", "feature commit 2"]);

        let feature_head = String::from_utf8(git(&path, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();

        // Go back to main and add a commit — now main tip has diverged from feature
        git(&path, &["checkout", "main"]);
        std::fs::write(path.join("main_extra.txt"), "main advance\n").unwrap();
        git(&path, &["add", "main_extra.txt"]);
        git(&path, &["commit", "-m", "main advance"]);

        // The main tip is NOT an ancestor of feature_head — this triggers the bug
        let main_tip = String::from_utf8(git(&path, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();

        (dir, path, main_tip, feature_head)
    }

    /// walk_commits(base, head) where base is NOT an ancestor of head must not
    /// error, and must return exactly the commits from `git rev-list base..head`.
    #[test]
    fn it_walks_diverged_history_without_error() {
        let (_dir, path, base_sha, feature_head) = create_diverged_repo();

        // What git itself says is the expected set (order matters: oldest first)
        let git_out = Command::new("git")
            .args([
                "rev-list",
                "--reverse",
                &format!("{base_sha}..{feature_head}"),
            ])
            .current_dir(&path)
            .output()
            .unwrap();
        let expected_shas: Vec<String> = String::from_utf8(git_out.stdout)
            .unwrap()
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        let reader = RepoReader::open(&path).unwrap();
        let result = reader.walk_commits(&base_sha, &feature_head);

        assert!(
            result.is_ok(),
            "walk_commits on diverged history must not error: {:?}",
            result.err()
        );
        let commits = result.unwrap();
        let actual_shas: Vec<String> = commits.iter().map(|c| c.sha.clone()).collect();

        assert_eq!(
            actual_shas, expected_shas,
            "walk_commits must return same commits as git rev-list --reverse base..head"
        );
    }

    /// Linear history: the result must match `git rev-list --reverse base..head` exactly.
    #[test]
    fn it_matches_git_rev_list_for_linear_history() {
        let (_dir, path) = create_test_repo();

        // Commit A (already exists as initial), add B and C
        std::fs::write(path.join("b.txt"), "b\n").unwrap();
        git(&path, &["add", "b.txt"]);
        git(&path, &["commit", "-m", "commit B"]);

        std::fs::write(path.join("c.txt"), "c\n").unwrap();
        git(&path, &["add", "c.txt"]);
        git(&path, &["commit", "-m", "commit C"]);

        let base = String::from_utf8(git(&path, &["rev-parse", "HEAD~2"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        let head = String::from_utf8(git(&path, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();

        let git_out = Command::new("git")
            .args(["rev-list", "--reverse", &format!("{base}..{head}")])
            .current_dir(&path)
            .output()
            .unwrap();
        let expected_shas: Vec<String> = String::from_utf8(git_out.stdout)
            .unwrap()
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        let reader = RepoReader::open(&path).unwrap();
        let commits = reader.walk_commits(&base, &head).unwrap();
        let actual_shas: Vec<String> = commits.iter().map(|c| c.sha.clone()).collect();

        assert_eq!(
            actual_shas, expected_shas,
            "walk_commits linear history must equal git rev-list --reverse"
        );
    }

    /// Range spanning a merge commit whose second parent is NOT reachable from
    /// base: every commit in `git rev-list base..head` must be in the output.
    /// This proves all-parents traversal, not first-parent-only.
    #[test]
    fn it_traverses_all_parents_through_merge_commits() {
        let (_dir, path) = create_test_repo();

        // Create a side branch with a unique commit
        git(&path, &["checkout", "-b", "side"]);
        std::fs::write(path.join("side.txt"), "side\n").unwrap();
        git(&path, &["add", "side.txt"]);
        git(&path, &["commit", "-m", "side commit"]);

        let side_sha = String::from_utf8(git(&path, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();

        // Back to main, add a commit, then merge side
        git(&path, &["checkout", "main"]);
        std::fs::write(path.join("main2.txt"), "main2\n").unwrap();
        git(&path, &["add", "main2.txt"]);
        git(&path, &["commit", "-m", "main commit 2"]);

        // Merge side into main (no-ff to force a merge commit)
        git(&path, &["merge", "--no-ff", "-m", "merge side", "side"]);

        // After merge: HEAD=merge, HEAD~1=main2, HEAD~2=initial
        // Use initial commit as base so range includes main2, side, and the merge commit.
        let base = String::from_utf8(git(&path, &["rev-parse", "HEAD~2"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        let head = String::from_utf8(git(&path, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();

        let git_out = Command::new("git")
            .args(["rev-list", "--reverse", &format!("{base}..{head}")])
            .current_dir(&path)
            .output()
            .unwrap();
        let expected_shas: std::collections::HashSet<String> = String::from_utf8(git_out.stdout)
            .unwrap()
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        let reader = RepoReader::open(&path).unwrap();
        let commits = reader.walk_commits(&base, &head).unwrap();
        let actual_shas: std::collections::HashSet<String> =
            commits.iter().map(|c| c.sha.clone()).collect();

        // The side commit must be present (proves all-parents traversal)
        assert!(
            actual_shas.contains(&side_sha),
            "side commit must appear in walk (all-parents traversal): {side_sha}"
        );
        assert_eq!(
            actual_shas, expected_shas,
            "walk_commits must return same commit set as git rev-list base..head"
        );
    }

    /// Disjoint histories: head is on an orphan branch with NO common ancestor
    /// with base. walk_commits must return the commits on the orphan branch
    /// without erroring on the root commit.
    #[test]
    fn it_handles_disjoint_histories_without_error() {
        let (_dir, path) = create_test_repo();

        // Add a commit on main so base is not the very root
        std::fs::write(path.join("main2.txt"), "main2\n").unwrap();
        git(&path, &["add", "main2.txt"]);
        git(&path, &["commit", "-m", "main commit 2"]);

        let base = String::from_utf8(git(&path, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();

        // Create a completely unrelated orphan branch
        git(&path, &["checkout", "--orphan", "orphan"]);
        // git checkout --orphan stages the previous tree; clear it
        git(&path, &["rm", "-rf", "."]);

        std::fs::write(path.join("orphan.txt"), "orphan\n").unwrap();
        git(&path, &["add", "orphan.txt"]);
        git(&path, &["commit", "-m", "orphan commit 1"]);
        std::fs::write(path.join("orphan2.txt"), "orphan2\n").unwrap();
        git(&path, &["add", "orphan2.txt"]);
        git(&path, &["commit", "-m", "orphan commit 2"]);

        let head = String::from_utf8(git(&path, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();

        let git_out = Command::new("git")
            .args(["rev-list", "--reverse", &format!("{base}..{head}")])
            .current_dir(&path)
            .output()
            .unwrap();
        let expected_shas: Vec<String> = String::from_utf8(git_out.stdout)
            .unwrap()
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();

        let reader = RepoReader::open(&path).unwrap();
        let result = reader.walk_commits(&base, &head);

        assert!(
            result.is_ok(),
            "walk_commits on disjoint histories must not error: {:?}",
            result.err()
        );
        let commits = result.unwrap();
        let actual_shas: Vec<String> = commits.iter().map(|c| c.sha.clone()).collect();

        assert_eq!(
            actual_shas, expected_shas,
            "walk_commits on disjoint histories must match git rev-list output"
        );
    }

    #[test]
    fn it_identifies_base_ref_as_missing() {
        let (_dir, path) = create_test_repo();
        let reader = RepoReader::open(&path).unwrap();
        // manifest with missing base_ref and valid head_ref
        let result = reader.walk_commits("feature/foo", "HEAD");
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("feature/foo"),
            "expected base ref name in error: {msg}"
        );
    }

    #[test]
    fn it_suggests_fetch_from_non_origin_remote() {
        let (local_dir, local_path) = create_test_repo();

        // Create a bare remote repo
        let remote_dir = TempDir::new().unwrap();
        let remote_path = remote_dir.path().to_path_buf();
        Command::new("git")
            .args(["init", "--bare"])
            .current_dir(&remote_path)
            .output()
            .unwrap();

        // Push to non-origin remote named "upstream"
        Command::new("git")
            .args(["remote", "add", "upstream", remote_path.to_str().unwrap()])
            .current_dir(&local_path)
            .output()
            .unwrap();

        // Create branch and push to upstream
        Command::new("git")
            .args(["checkout", "-b", "feature/foo"])
            .current_dir(&local_path)
            .output()
            .unwrap();
        std::fs::write(local_path.join("feature.txt"), "feature content\n").unwrap();
        Command::new("git")
            .args(["add", "feature.txt"])
            .current_dir(&local_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add feature"])
            .current_dir(&local_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["push", "-u", "upstream", "feature/foo"])
            .current_dir(&local_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["fetch", "upstream"])
            .current_dir(&local_path)
            .output()
            .unwrap();

        // Delete the local branch so only the remote tracking ref remains
        Command::new("git")
            .args(["checkout", "main"])
            .current_dir(&local_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["branch", "-D", "feature/foo"])
            .current_dir(&local_path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&local_path).unwrap();
        let err = reader.resolve_commit("feature/foo").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("\"resolution\""),
            "expected JSON resolution field in: {msg}"
        );
        // Should use the actual remote name "upstream", NOT hardcoded "origin"
        assert!(
            msg.contains("git fetch upstream feature/foo"),
            "expected fetch suggestion with actual remote name 'upstream', got: {msg}"
        );

        drop(local_dir);
        drop(remote_dir);
    }

    #[test]
    fn it_peels_annotated_tag_to_commit() {
        let (_dir, path) = create_test_repo();

        Command::new("git")
            .args(["tag", "-a", "v1.0", "-m", "release v1.0"])
            .current_dir(&path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let result = reader.resolve_commit("v1.0");
        assert!(
            result.is_ok(),
            "peel_to_commit failed for annotated tag: {:?}",
            result.err()
        );
        let commit = result.unwrap();
        assert_eq!(
            commit.message, "initial commit",
            "annotated tag should resolve to the tagged commit"
        );
    }

    #[test]
    fn it_resolves_lightweight_tag_to_commit() {
        let (_dir, path) = create_test_repo();

        Command::new("git")
            .args(["tag", "v0.1"])
            .current_dir(&path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let commit = reader.resolve_commit("v0.1").unwrap();
        assert_eq!(
            commit.message, "initial commit",
            "lightweight tag should also resolve to the tagged commit"
        );
    }

    #[test]
    fn annotated_tag_and_branch_resolve_to_same_commit_sha() {
        let (_dir, path) = create_test_repo();

        Command::new("git")
            .args(["tag", "-a", "v1.0", "-m", "release v1.0"])
            .current_dir(&path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let via_tag = reader.resolve_commit("v1.0").unwrap();
        let via_branch = reader.resolve_commit("main").unwrap();
        assert_eq!(
            via_tag.sha, via_branch.sha,
            "annotated tag and branch should resolve to the same commit SHA"
        );
    }

    // ===== ADVERSARIAL QA PROBES (issue #337 pen-test) =====

    fn git(path: &std::path::Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .unwrap()
    }

    /// A tag pointing at ANOTHER annotated tag (`git tag -a v2 v1`). gix's
    /// peel_to_commit must follow the full chain tag -> tag -> commit.
    #[test]
    fn qa_nested_annotated_tag_chain_resolves_to_commit() {
        let (_dir, path) = create_test_repo();
        git(&path, &["tag", "-a", "v1", "-m", "first tag"]);
        git(&path, &["tag", "-a", "v2", "-m", "tag of a tag", "v1"]);

        let reader = RepoReader::open(&path).unwrap();
        let via_chain = reader.resolve_commit("v2");
        assert!(
            via_chain.is_ok(),
            "nested annotated tag (tag->tag->commit) failed to peel: {:?}",
            via_chain.err()
        );
        let head = reader.resolve_commit("HEAD").unwrap();
        assert_eq!(
            via_chain.unwrap().sha,
            head.sha,
            "nested annotated tag must resolve to the underlying commit"
        );
    }

    /// An annotated tag whose target is a TREE (not a commit). peel_to_commit
    /// cannot reach a commit, so it must return a clean GitError::ReadObject —
    /// NOT panic and NOT silently succeed.
    #[test]
    fn qa_annotated_tag_pointing_at_tree_errors_cleanly() {
        let (_dir, path) = create_test_repo();
        // Resolve HEAD's tree SHA, then create an annotated tag pointing at it.
        let tree_sha = String::from_utf8(git(&path, &["rev-parse", "HEAD^{tree}"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        let tag_result = git(
            &path,
            &["tag", "-a", "treetag", "-m", "points at a tree", &tree_sha],
        );
        assert!(
            tag_result.status.success(),
            "fixture setup failed (could not tag a tree): {}",
            String::from_utf8_lossy(&tag_result.stderr)
        );

        let reader = RepoReader::open(&path).unwrap();
        // Must not panic; must be an Err.
        let result = reader.resolve_commit("treetag");
        assert!(
            result.is_err(),
            "annotated tag pointing at a tree must NOT resolve to a commit, got: {:?}",
            result.ok()
        );
        assert!(
            matches!(result.unwrap_err(), GitError::ReadObject(_)),
            "tag->tree peel failure must surface as GitError::ReadObject"
        );
    }

    /// An annotated tag whose target is a BLOB. Same contract: clean error.
    #[test]
    fn qa_annotated_tag_pointing_at_blob_errors_cleanly() {
        let (_dir, path) = create_test_repo();
        let blob_sha = String::from_utf8(git(&path, &["rev-parse", "HEAD:README.md"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        let tag_result = git(
            &path,
            &["tag", "-a", "blobtag", "-m", "points at a blob", &blob_sha],
        );
        assert!(
            tag_result.status.success(),
            "fixture setup failed (could not tag a blob): {}",
            String::from_utf8_lossy(&tag_result.stderr)
        );

        let reader = RepoReader::open(&path).unwrap();
        let result = reader.resolve_commit("blobtag");
        assert!(
            result.is_err(),
            "annotated tag pointing at a blob must NOT resolve to a commit, got: {:?}",
            result.ok()
        );
        assert!(
            matches!(result.unwrap_err(), GitError::ReadObject(_)),
            "tag->blob peel failure must surface as GitError::ReadObject"
        );
    }

    /// Regression guard: a raw full SHA must still resolve to itself after the
    /// peel change (peel_to_commit on an already-commit object is a no-op).
    #[test]
    fn qa_raw_full_sha_still_resolves_unchanged() {
        let (_dir, path) = create_test_repo();
        let full_sha = String::from_utf8(git(&path, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        let reader = RepoReader::open(&path).unwrap();
        let by_sha = reader.resolve_commit(&full_sha).unwrap();
        assert_eq!(by_sha.sha, full_sha, "raw full SHA must resolve to itself");
        assert_eq!(by_sha.message, "initial commit");
    }

    /// tag..tag manifest (annotated) must equal the equivalent commit..commit
    /// manifest and be non-empty. This is the real-world reviewer use case:
    /// `git-prism manifest v_old..v_new` over annotated release tags.
    #[test]
    fn qa_annotated_tag_range_manifest_matches_commit_range_and_is_nonempty() {
        let (_dir, path) = create_test_repo();
        // base = initial commit (tagged v_old)
        git(&path, &["tag", "-a", "v_old", "-m", "old release"]);
        let base_sha = String::from_utf8(git(&path, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();
        // add a second commit, tag it v_new
        std::fs::write(path.join("feature.rs"), "fn added() {}\n").unwrap();
        git(&path, &["add", "feature.rs"]);
        git(&path, &["commit", "-m", "add feature"]);
        git(&path, &["tag", "-a", "v_new", "-m", "new release"]);
        let head_sha = String::from_utf8(git(&path, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
            .to_string();

        let reader = RepoReader::open(&path).unwrap();

        let via_tags = reader.diff_commits("v_old", "v_new").unwrap();
        let via_commits = reader.diff_commits(&base_sha, &head_sha).unwrap();

        let tag_files: Vec<String> = via_tags.files.iter().map(|f| f.path.clone()).collect();
        let commit_files: Vec<String> = via_commits.files.iter().map(|f| f.path.clone()).collect();

        assert!(
            !tag_files.is_empty(),
            "annotated tag..tag diff must be non-empty (expected feature.rs)"
        );
        assert_eq!(
            tag_files, commit_files,
            "annotated tag range must produce the same file set as the commit range"
        );
        assert!(
            tag_files.iter().any(|p| p == "feature.rs"),
            "expected feature.rs in tag-range diff, got: {tag_files:?}"
        );
    }

    /// Mixed range: annotated tag .. branch must resolve both ends and produce
    /// the correct diff (regression that peel didn't break non-tag end).
    #[test]
    fn qa_mixed_annotated_tag_dotdot_branch_range() {
        let (_dir, path) = create_test_repo();
        git(&path, &["tag", "-a", "v_old", "-m", "old release"]);
        std::fs::write(path.join("feature.rs"), "fn added() {}\n").unwrap();
        git(&path, &["add", "feature.rs"]);
        git(&path, &["commit", "-m", "add feature"]);

        let reader = RepoReader::open(&path).unwrap();
        let via_mixed = reader.diff_commits("v_old", "main").unwrap();
        let files: Vec<String> = via_mixed.files.iter().map(|f| f.path.clone()).collect();
        assert!(
            files.iter().any(|p| p == "feature.rs"),
            "mixed tag..branch range must include feature.rs, got: {files:?}"
        );
    }
}
