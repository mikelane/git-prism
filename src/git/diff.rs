use serde::Serialize;

use crate::git::reader::{GitError, RepoReader};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub old_path: Option<String>,
    pub change_type: ChangeType,
    pub is_binary: bool,
    pub lines_added: usize,
    pub lines_removed: usize,
    pub size_before: usize,
    pub size_after: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DiffResult {
    pub files: Vec<FileChange>,
}

impl RepoReader {
    pub fn diff_commits(
        &self,
        base_ref: &str,
        head_ref: &str,
    ) -> Result<DiffResult, GitError> {
        let base_commit = self.peel_to_commit(base_ref)?;
        let head_commit = self.peel_to_commit(head_ref)?;

        let base_tree = base_commit.tree().map_err(obj_err)?;
        let head_tree = head_commit.tree().map_err(obj_err)?;

        let mut files: Vec<FileChange> = Vec::new();

        base_tree.changes().map_err(obj_err)?
            .for_each_to_obtain_tree(&head_tree, |change| {
                use gix::object::tree::diff::Change as C;

                let file_change = match change {
                    C::Addition {
                        location,
                        id,
                        entry_mode: _,
                        relation: _,
                    } => {
                        let (size_after, is_binary, lines_added) = blob_stats(&id);
                        FileChange {
                            path: location.to_string(),
                            old_path: None,
                            change_type: ChangeType::Added,
                            is_binary,
                            lines_added,
                            lines_removed: 0,
                            size_before: 0,
                            size_after,
                        }
                    }
                    C::Deletion {
                        location,
                        id,
                        entry_mode: _,
                        relation: _,
                    } => {
                        let (size_before, is_binary, lines_removed) = blob_stats(&id);
                        FileChange {
                            path: location.to_string(),
                            old_path: None,
                            change_type: ChangeType::Deleted,
                            is_binary,
                            lines_added: 0,
                            lines_removed,
                            size_before,
                            size_after: 0,
                        }
                    }
                    C::Modification {
                        location,
                        previous_id,
                        id,
                        previous_entry_mode: _,
                        entry_mode: _,
                    } => {
                        let old_obj = previous_id.object().ok();
                        let new_obj = id.object().ok();

                        let size_before = old_obj.as_ref().map_or(0, |o| o.data.len());
                        let size_after = new_obj.as_ref().map_or(0, |o| o.data.len());

                        let is_binary = old_obj
                            .as_ref()
                            .is_some_and(|o| o.data.contains(&0))
                            || new_obj
                                .as_ref()
                                .is_some_and(|o| o.data.contains(&0));

                        let (lines_added, lines_removed) = if is_binary {
                            (0, 0)
                        } else {
                            count_line_changes(
                                old_obj.as_ref().map(|o| o.data.as_ref()),
                                new_obj.as_ref().map(|o| o.data.as_ref()),
                            )
                        };

                        FileChange {
                            path: location.to_string(),
                            old_path: None,
                            change_type: ChangeType::Modified,
                            is_binary,
                            lines_added,
                            lines_removed,
                            size_before,
                            size_after,
                        }
                    }
                    C::Rewrite {
                        source_location,
                        source_id,
                        location,
                        id,
                        diff,
                        copy,
                        source_entry_mode: _,
                        source_relation: _,
                        entry_mode: _,
                        relation: _,
                    } => {
                        let old_obj = source_id.object().ok();
                        let new_obj = id.object().ok();

                        let size_before = old_obj.as_ref().map_or(0, |o| o.data.len());
                        let size_after = new_obj.as_ref().map_or(0, |o| o.data.len());

                        let is_binary = old_obj
                            .as_ref()
                            .is_some_and(|o| o.data.contains(&0))
                            || new_obj
                                .as_ref()
                                .is_some_and(|o| o.data.contains(&0));

                        let (lines_added, lines_removed) = match diff {
                            Some(stats) => (stats.insertions as usize, stats.removals as usize),
                            None => (0, 0),
                        };

                        FileChange {
                            path: location.to_string(),
                            old_path: Some(source_location.to_string()),
                            change_type: if copy { ChangeType::Copied } else { ChangeType::Renamed },
                            is_binary,
                            lines_added,
                            lines_removed,
                            size_before,
                            size_after,
                        }
                    }
                };

                files.push(file_change);
                Ok::<gix::object::tree::diff::Action, std::convert::Infallible>(
                    gix::object::tree::diff::Action::Continue,
                )
            })
            .map_err(obj_err)?;

        Ok(DiffResult { files })
    }
}

fn obj_err(e: impl std::fmt::Display) -> GitError {
    GitError::ReadObject(e.to_string())
}

fn blob_stats(id: &gix::Id<'_>) -> (usize, bool, usize) {
    match id.object() {
        Ok(obj) => {
            let is_binary = obj.data.contains(&0);
            let lines = if is_binary || obj.data.is_empty() {
                0
            } else {
                let newline_count = obj.data.iter().filter(|&&b| b == b'\n').count();
                let last_byte = obj.data[obj.data.len() - 1];
                newline_count + if last_byte != b'\n' { 1 } else { 0 }
            };
            (obj.data.len(), is_binary, lines)
        }
        Err(_) => (0, false, 0),
    }
}

fn count_line_changes(old_data: Option<&[u8]>, new_data: Option<&[u8]>) -> (usize, usize) {
    let old_text = old_data.map_or(String::new(), |d| String::from_utf8_lossy(d).to_string());
    let new_text = new_data.map_or(String::new(), |d| String::from_utf8_lossy(d).to_string());

    let input = gix::diff::blob::intern::InternedInput::new(
        old_text.as_str(),
        new_text.as_str(),
    );

    let counter = gix::diff::blob::diff(
        gix::diff::blob::Algorithm::Myers,
        &input,
        LineCounter::default(),
    );

    (counter.added, counter.removed)
}

#[derive(Default)]
struct LineCounter {
    added: usize,
    removed: usize,
}

impl gix::diff::blob::Sink for LineCounter {
    type Out = Self;

    fn process_change(&mut self, before: std::ops::Range<u32>, after: std::ops::Range<u32>) {
        self.removed += (before.end - before.start) as usize;
        self.added += (after.end - after.start) as usize;
    }

    fn finish(self) -> Self::Out {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn create_repo_with_two_commits() -> (TempDir, std::path::PathBuf) {
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

        std::fs::write(path.join("existing.txt"), "hello\n").unwrap();

        Command::new("git")
            .args(["add", "existing.txt"])
            .current_dir(&path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["commit", "-m", "initial commit"])
            .current_dir(&path)
            .output()
            .unwrap();

        std::fs::write(path.join("added.txt"), "new file\n").unwrap();

        Command::new("git")
            .args(["add", "added.txt"])
            .current_dir(&path)
            .output()
            .unwrap();

        Command::new("git")
            .args(["commit", "-m", "add a file"])
            .current_dir(&path)
            .output()
            .unwrap();

        (dir, path)
    }

    #[test]
    fn it_detects_added_file() {
        let (_dir, path) = create_repo_with_two_commits();
        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_commits("HEAD~1", "HEAD").unwrap();
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].path, "added.txt");
        assert_eq!(diff.files[0].change_type, ChangeType::Added);
    }

    #[test]
    fn it_reports_size_and_lines_for_added_file() {
        let (_dir, path) = create_repo_with_two_commits();
        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_commits("HEAD~1", "HEAD").unwrap();
        assert_eq!(diff.files[0].size_before, 0);
        assert_eq!(diff.files[0].size_after, 9); // "new file\n" = 9 bytes
        assert_eq!(diff.files[0].lines_added, 1);
        assert_eq!(diff.files[0].lines_removed, 0);
    }

    #[test]
    fn it_detects_deleted_file() {
        let (_dir, path) = create_repo_with_two_commits();

        std::fs::remove_file(path.join("existing.txt")).unwrap();
        Command::new("git")
            .args(["add", "existing.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "delete a file"])
            .current_dir(&path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_commits("HEAD~1", "HEAD").unwrap();
        assert_eq!(diff.files.len(), 1);
        assert_eq!(diff.files[0].path, "existing.txt");
        assert_eq!(diff.files[0].change_type, ChangeType::Deleted);
        assert!(diff.files[0].size_before > 0);
        assert_eq!(diff.files[0].size_after, 0);
    }

    #[test]
    fn it_detects_modified_file() {
        let (_dir, path) = create_repo_with_two_commits();

        std::fs::write(path.join("existing.txt"), "hello\nworld\n").unwrap();
        Command::new("git")
            .args(["add", "existing.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "modify a file"])
            .current_dir(&path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_commits("HEAD~1", "HEAD").unwrap();

        let modified = diff
            .files
            .iter()
            .find(|f| f.path == "existing.txt")
            .unwrap();
        assert_eq!(modified.change_type, ChangeType::Modified);
        assert!(modified.lines_added > 0);
    }

    #[test]
    fn it_detects_renamed_file() {
        let (_dir, path) = create_repo_with_two_commits();

        Command::new("git")
            .args(["mv", "existing.txt", "renamed.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "rename a file"])
            .current_dir(&path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_commits("HEAD~1", "HEAD").unwrap();

        let renamed = diff
            .files
            .iter()
            .find(|f| f.path == "renamed.txt")
            .unwrap();
        assert_eq!(renamed.change_type, ChangeType::Renamed);
        assert_eq!(renamed.old_path.as_deref(), Some("existing.txt"));
    }

    #[test]
    fn it_counts_lines_for_file_without_trailing_newline() {
        let (_dir, path) = create_repo_with_two_commits();

        // Write a file with no trailing newline: "hello" is 1 line, not 0
        std::fs::write(path.join("no_newline.txt"), "hello").unwrap();
        Command::new("git")
            .args(["add", "no_newline.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add file without trailing newline"])
            .current_dir(&path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_commits("HEAD~1", "HEAD").unwrap();

        let file = diff
            .files
            .iter()
            .find(|f| f.path == "no_newline.txt")
            .unwrap();
        assert_eq!(file.lines_added, 1, "non-empty file without trailing newline should count as 1 line");
        assert_eq!(file.size_after, 5);
    }

    #[test]
    fn it_counts_lines_for_deleted_file_without_trailing_newline() {
        let (_dir, path) = create_repo_with_two_commits();

        // Write and commit a file without trailing newline, then delete it
        std::fs::write(path.join("ephemeral.txt"), "one\ntwo\nthree").unwrap();
        Command::new("git")
            .args(["add", "ephemeral.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add ephemeral file"])
            .current_dir(&path)
            .output()
            .unwrap();

        std::fs::remove_file(path.join("ephemeral.txt")).unwrap();
        Command::new("git")
            .args(["add", "ephemeral.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "delete ephemeral file"])
            .current_dir(&path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_commits("HEAD~1", "HEAD").unwrap();

        let file = diff
            .files
            .iter()
            .find(|f| f.path == "ephemeral.txt")
            .unwrap();
        assert_eq!(file.lines_removed, 3, "three lines without trailing newline: 'one\\ntwo\\nthree'");
        assert_eq!(file.change_type, ChangeType::Deleted);
    }

    #[test]
    fn it_counts_lines_for_multiline_file_with_trailing_newline() {
        let (_dir, path) = create_repo_with_two_commits();

        // "one\ntwo\n" has trailing newline -> 2 lines (not 3)
        std::fs::write(path.join("twolines.txt"), "one\ntwo\n").unwrap();
        Command::new("git")
            .args(["add", "twolines.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add two-line file"])
            .current_dir(&path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_commits("HEAD~1", "HEAD").unwrap();

        let file = diff
            .files
            .iter()
            .find(|f| f.path == "twolines.txt")
            .unwrap();
        assert_eq!(file.lines_added, 2, "'one\\ntwo\\n' is 2 lines, not 3");
    }

    #[test]
    fn it_flags_binary_files() {
        let (_dir, path) = create_repo_with_two_commits();

        std::fs::write(path.join("image.png"), &[0x89, 0x50, 0x4E, 0x47, 0x00, 0x00]).unwrap();
        Command::new("git")
            .args(["add", "image.png"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add binary file"])
            .current_dir(&path)
            .output()
            .unwrap();

        let reader = RepoReader::open(&path).unwrap();
        let diff = reader.diff_commits("HEAD~1", "HEAD").unwrap();

        let binary = diff
            .files
            .iter()
            .find(|f| f.path == "image.png")
            .unwrap();
        assert!(binary.is_binary);
        assert_eq!(binary.lines_added, 0);
    }
}
