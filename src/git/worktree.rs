use crate::git::diff::{
    ChangeScope, ChangeType, DiffResult, FileChange, count_line_changes, count_lines,
};
use crate::git::reader::{GitError, RepoReader};

impl RepoReader {
    /// Compare HEAD against the working tree (staged + unstaged changes).
    ///
    /// Returns a `DiffResult` where each `FileChange` carries a `change_scope`
    /// of either `Staged` or `Unstaged`, mirroring `git status` semantics.
    pub fn diff_worktree(&self) -> Result<DiffResult, GitError> {
        let _span = tracing::info_span!("git.diff_worktree").entered();
        use gix::status::Item;

        let status_iter = self
            .repo()
            .status(gix::progress::Discard)
            .map_err(obj_err)?
            .into_iter(None)
            .map_err(obj_err)?;

        let mut files = Vec::new();

        for item in status_iter {
            let item = item.map_err(obj_err)?;
            match item {
                Item::TreeIndex(change) => {
                    if let Some(fc) = self.tree_index_to_file_change(change)? {
                        files.push(fc);
                    }
                }
                Item::IndexWorktree(iw) => {
                    if let Some(fc) = self.index_worktree_to_file_change(iw)? {
                        files.push(fc);
                    }
                }
            }
        }

        Ok(DiffResult { files })
    }

    fn tree_index_to_file_change(
        &self,
        change: gix::diff::index::Change,
    ) -> Result<Option<FileChange>, GitError> {
        use gix::diff::index::ChangeRef as C;

        let fc = match change {
            C::Addition {
                location,
                entry_mode: _,
                id,
                ..
            } => {
                let obj = self.repo().find_object(id.as_ref()).map_err(obj_err)?;
                let data = &obj.data;
                let is_binary = data.contains(&0);
                let lines_added = if is_binary { 0 } else { count_lines(data) };

                FileChange {
                    path: location.to_string(),
                    old_path: None,
                    change_type: ChangeType::Added,
                    change_scope: ChangeScope::Staged,
                    is_binary,
                    lines_added,
                    lines_removed: 0,
                    size_before: 0,
                    size_after: data.len(),
                    staged_blob_id: Some(id.to_hex().to_string()),
                }
            }
            C::Deletion {
                location,
                entry_mode: _,
                id,
                ..
            } => {
                let obj = self.repo().find_object(id.as_ref()).map_err(obj_err)?;
                let data = &obj.data;
                let is_binary = data.contains(&0);
                let lines_removed = if is_binary { 0 } else { count_lines(data) };

                FileChange {
                    path: location.to_string(),
                    old_path: None,
                    change_type: ChangeType::Deleted,
                    change_scope: ChangeScope::Staged,
                    is_binary,
                    lines_added: 0,
                    lines_removed,
                    size_before: data.len(),
                    size_after: 0,
                    staged_blob_id: None,
                }
            }
            C::Modification {
                location,
                previous_id,
                id,
                ..
            } => {
                let old_obj = self
                    .repo()
                    .find_object(previous_id.as_ref())
                    .map_err(obj_err)?;
                let new_obj = self.repo().find_object(id.as_ref()).map_err(obj_err)?;
                let is_binary = old_obj.data.contains(&0) || new_obj.data.contains(&0);

                let (lines_added, lines_removed) = if is_binary {
                    (0, 0)
                } else {
                    count_line_changes(Some(&old_obj.data), Some(&new_obj.data))
                };

                FileChange {
                    path: location.to_string(),
                    old_path: None,
                    change_type: ChangeType::Modified,
                    change_scope: ChangeScope::Staged,
                    is_binary,
                    lines_added,
                    lines_removed,
                    size_before: old_obj.data.len(),
                    size_after: new_obj.data.len(),
                    staged_blob_id: Some(id.to_hex().to_string()),
                }
            }
            C::Rewrite {
                source_location,
                location,
                source_id,
                id,
                copy,
                ..
            } => {
                let old_obj = self
                    .repo()
                    .find_object(source_id.as_ref())
                    .map_err(obj_err)?;
                let new_obj = self.repo().find_object(id.as_ref()).map_err(obj_err)?;
                // Rewrite (rename/copy) requires gix rename detection to trigger; no test fixtures
                // exercise this path with one-sided binary. Same logic is tested via Modification.
                let is_binary = old_obj.data.contains(&0) || new_obj.data.contains(&0);

                let (lines_added, lines_removed) = if is_binary {
                    (0, 0)
                } else {
                    count_line_changes(Some(&old_obj.data), Some(&new_obj.data))
                };

                FileChange {
                    path: location.to_string(),
                    old_path: Some(source_location.to_string()),
                    change_type: if copy {
                        ChangeType::Copied
                    } else {
                        ChangeType::Renamed
                    },
                    change_scope: ChangeScope::Staged,
                    is_binary,
                    lines_added,
                    lines_removed,
                    size_before: old_obj.data.len(),
                    size_after: new_obj.data.len(),
                    staged_blob_id: Some(id.to_hex().to_string()),
                }
            }
        };

        Ok(Some(fc))
    }

    fn index_worktree_to_file_change(
        &self,
        item: gix::status::index_worktree::Item,
    ) -> Result<Option<FileChange>, GitError> {
        use gix::status::index_worktree::Item as IW;
        use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

        match item {
            IW::Modification {
                rela_path,
                entry,
                status,
                ..
            } => match status {
                EntryStatus::Change(change) => {
                    let path_str = rela_path.to_string();
                    let workdir = self
                        .repo()
                        .workdir()
                        .ok_or_else(|| GitError::ReadObject("bare repository".into()))?;
                    let full_path = workdir.join(&path_str);

                    match change {
                        Change::Removed => {
                            let old_obj = self.repo().find_object(entry.id).map_err(obj_err)?;
                            let is_binary = old_obj.data.contains(&0);
                            let lines_removed = if is_binary {
                                0
                            } else {
                                count_lines(&old_obj.data)
                            };

                            Ok(Some(FileChange {
                                path: path_str,
                                old_path: None,
                                change_type: ChangeType::Deleted,
                                change_scope: ChangeScope::Unstaged,
                                is_binary,
                                lines_added: 0,
                                lines_removed,
                                size_before: old_obj.data.len(),
                                size_after: 0,
                                staged_blob_id: None,
                            }))
                        }
                        Change::Type { .. } | Change::Modification { .. } => {
                            let old_obj = self.repo().find_object(entry.id).map_err(obj_err)?;
                            let new_data = std::fs::read(&full_path)
                                .map_err(|e| GitError::ReadObject(e.to_string()))?;
                            let is_binary = old_obj.data.contains(&0) || new_data.contains(&0);

                            let (lines_added, lines_removed) = if is_binary {
                                (0, 0)
                            } else {
                                count_line_changes(Some(&old_obj.data), Some(&new_data))
                            };

                            Ok(Some(FileChange {
                                path: path_str,
                                old_path: None,
                                change_type: ChangeType::Modified,
                                change_scope: ChangeScope::Unstaged,
                                is_binary,
                                lines_added,
                                lines_removed,
                                size_before: old_obj.data.len(),
                                size_after: new_data.len(),
                                staged_blob_id: None,
                            }))
                        }
                        Change::SubmoduleModification(..) => Ok(None),
                    }
                }
                EntryStatus::Conflict { .. }
                | EntryStatus::NeedsUpdate(_)
                | EntryStatus::IntentToAdd => Ok(None),
            },
            IW::DirectoryContents { .. } | IW::Rewrite { .. } => Ok(None),
        }
    }
}

fn obj_err(e: impl std::fmt::Display) -> GitError {
    GitError::ReadObject(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn create_repo_with_one_commit() -> (TempDir, std::path::PathBuf) {
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
    fn it_detects_staged_addition() {
        let (_dir, path) = create_repo_with_one_commit();

        std::fs::write(path.join("new.txt"), "hello\n").unwrap();
        Command::new("git")
            .args(["add", "new.txt"])
            .current_dir(&path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_worktree().unwrap();

        let file = diff.files.iter().find(|f| f.path == "new.txt").unwrap();
        assert_eq!(file.change_type, ChangeType::Added);
        assert_eq!(file.change_scope, ChangeScope::Staged);
        assert_eq!(file.lines_added, 1);
        assert_eq!(file.size_after, 6); // "hello\n" = 6 bytes
    }

    #[test]
    fn it_detects_unstaged_modification() {
        let (_dir, path) = create_repo_with_one_commit();

        // Modify a committed file without staging
        std::fs::write(path.join("README.md"), "# Updated\n").unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_worktree().unwrap();

        let file = diff.files.iter().find(|f| f.path == "README.md").unwrap();
        assert_eq!(file.change_type, ChangeType::Modified);
        assert_eq!(file.change_scope, ChangeScope::Unstaged);
        assert!(file.lines_added > 0);
    }

    #[test]
    fn it_detects_staged_modification() {
        let (_dir, path) = create_repo_with_one_commit();

        std::fs::write(path.join("README.md"), "# Updated\n").unwrap();
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(&path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_worktree().unwrap();

        let file = diff.files.iter().find(|f| f.path == "README.md").unwrap();
        assert_eq!(file.change_type, ChangeType::Modified);
        assert_eq!(file.change_scope, ChangeScope::Staged);
    }

    #[test]
    fn it_distinguishes_staged_and_unstaged_for_same_file() {
        let (_dir, path) = create_repo_with_one_commit();

        // Stage a change
        std::fs::write(path.join("README.md"), "# Staged\n").unwrap();
        Command::new("git")
            .args(["add", "README.md"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Make another change on disk without staging
        std::fs::write(path.join("README.md"), "# Unstaged\n").unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_worktree().unwrap();

        let readme_changes: Vec<&FileChange> = diff
            .files
            .iter()
            .filter(|f| f.path == "README.md")
            .collect();
        assert_eq!(readme_changes.len(), 2);

        let scopes: std::collections::HashSet<ChangeScope> =
            readme_changes.iter().map(|f| f.change_scope).collect();
        assert!(scopes.contains(&ChangeScope::Staged));
        assert!(scopes.contains(&ChangeScope::Unstaged));
    }

    #[test]
    fn it_returns_empty_diff_for_clean_worktree() {
        let (_dir, path) = create_repo_with_one_commit();

        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_worktree().unwrap();

        assert!(diff.files.is_empty());
    }

    // --- Gap-closing tests for mutation testing ---

    // Kill mutant: line 109 replace || with && in tree_index_to_file_change (binary detection)
    // Staged modification where only old blob is binary.
    #[test]
    fn it_detects_staged_binary_when_only_old_blob_is_binary() {
        let (_dir, path) = create_repo_with_one_commit();

        // Commit a binary file
        std::fs::write(path.join("data.bin"), [0x00, 0x01, 0x02]).unwrap();
        Command::new("git")
            .args(["add", "data.bin"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add binary"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Stage a text replacement (no null bytes)
        std::fs::write(path.join("data.bin"), "now text\n").unwrap();
        Command::new("git")
            .args(["add", "data.bin"])
            .current_dir(&path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_worktree().unwrap();
        let file = diff.files.iter().find(|f| f.path == "data.bin").unwrap();
        assert!(
            file.is_binary,
            "staged mod should be binary when old blob has null bytes"
        );
        assert_eq!(file.change_scope, ChangeScope::Staged);
        assert_eq!(file.lines_added, 0);
        assert_eq!(file.lines_removed, 0);
    }

    // Kill mutant: line 143 replace || with && in tree_index_to_file_change (Rewrite binary)
    // Staged modification where only new blob is binary.
    #[test]
    fn it_detects_staged_binary_when_only_new_blob_is_binary() {
        let (_dir, path) = create_repo_with_one_commit();

        // Commit a text file
        std::fs::write(path.join("data.bin"), "text content\n").unwrap();
        Command::new("git")
            .args(["add", "data.bin"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add text"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Stage a binary replacement (has null bytes)
        std::fs::write(path.join("data.bin"), [0x89, 0x50, 0x00, 0x47]).unwrap();
        Command::new("git")
            .args(["add", "data.bin"])
            .current_dir(&path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_worktree().unwrap();
        let file = diff.files.iter().find(|f| f.path == "data.bin").unwrap();
        assert!(
            file.is_binary,
            "staged mod should be binary when new blob has null bytes"
        );
        assert_eq!(file.change_scope, ChangeScope::Staged);
        assert_eq!(file.lines_added, 0);
        assert_eq!(file.lines_removed, 0);
    }

    // Kill mutant: line 222 replace || with && in index_worktree_to_file_change (binary detection)
    // Unstaged modification where only the new (disk) file is binary.
    #[test]
    fn it_detects_unstaged_binary_when_only_disk_file_is_binary() {
        let (_dir, path) = create_repo_with_one_commit();

        // Commit a text file
        std::fs::write(path.join("file.dat"), "text\n").unwrap();
        Command::new("git")
            .args(["add", "file.dat"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add text file"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Modify on disk with binary content (do NOT stage)
        std::fs::write(path.join("file.dat"), [0x00, 0xFF, 0x01]).unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_worktree().unwrap();
        let file = diff.files.iter().find(|f| f.path == "file.dat").unwrap();
        assert!(
            file.is_binary,
            "unstaged mod should be binary when disk file has null bytes"
        );
        assert_eq!(file.change_scope, ChangeScope::Unstaged);
        assert_eq!(file.lines_added, 0);
        assert_eq!(file.lines_removed, 0);
    }

    // Unstaged modification where only the old (index) blob is binary.
    #[test]
    fn it_detects_unstaged_binary_when_only_index_blob_is_binary() {
        let (_dir, path) = create_repo_with_one_commit();

        // Commit a binary file
        std::fs::write(path.join("file.dat"), [0x00, 0x01, 0x02]).unwrap();
        Command::new("git")
            .args(["add", "file.dat"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add binary file"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Modify on disk with text content (do NOT stage)
        std::fs::write(path.join("file.dat"), "now text\n").unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_worktree().unwrap();
        let file = diff.files.iter().find(|f| f.path == "file.dat").unwrap();
        assert!(
            file.is_binary,
            "unstaged mod should be binary when index blob has null bytes"
        );
        assert_eq!(file.change_scope, ChangeScope::Unstaged);
        assert_eq!(file.lines_added, 0);
        assert_eq!(file.lines_removed, 0);
    }
}
