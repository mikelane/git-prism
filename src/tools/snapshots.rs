use std::path::Path;

use chrono::Utc;

use crate::git::diff;
use crate::git::reader::RepoReader;
use crate::tools::size;
use crate::tools::types::{
    DiffHunk, FileContent, SnapshotFileEntry, SnapshotMetadata, SnapshotOptions, SnapshotResponse,
    ToolError, detect_language,
};

const MAX_SNAPSHOT_FILES: usize = 20;

pub fn build_snapshots(
    repo_path: &Path,
    base_ref: &str,
    head_ref: &str,
    paths: &[String],
    options: &SnapshotOptions,
) -> Result<SnapshotResponse, ToolError> {
    let reader = RepoReader::open(repo_path)?;

    // cargo-mutants: skip -- equivalent mutant: when paths.len() == MAX_SNAPSHOT_FILES,
    // both `> MAX` (false → use full slice) and `>= MAX` (true → take &paths[..MAX])
    // yield the same 20-element slice, so no black-box test can distinguish them.
    #[rustfmt::skip]
    let paths_to_process = if paths.len() > MAX_SNAPSHOT_FILES { &paths[..MAX_SNAPSHOT_FILES] } else { paths };

    let mut files = Vec::new();
    let mut total_chars: usize = 0;

    for file_path in paths_to_process {
        let entry = build_snapshot_entry(&reader, base_ref, head_ref, file_path, options);
        if let Some(ref before) = entry.before {
            total_chars += before.content.len();
        }
        if let Some(ref after) = entry.after {
            total_chars += after.content.len();
        }
        files.push(entry);
    }

    Ok(SnapshotResponse {
        metadata: SnapshotMetadata {
            repo_path: repo_path.display().to_string(),
            base_ref: base_ref.to_string(),
            head_ref: head_ref.to_string(),
            generated_at: Utc::now(),
        },
        files,
        token_estimate: size::estimate_tokens(total_chars),
    })
}

fn build_snapshot_entry(
    reader: &RepoReader,
    base_ref: &str,
    head_ref: &str,
    file_path: &str,
    options: &SnapshotOptions,
) -> SnapshotFileEntry {
    let language = detect_language(file_path);

    let base_result = reader.read_file_at_ref(base_ref, file_path);
    let head_result = reader.read_file_at_ref(head_ref, file_path);

    let before = if options.include_before {
        match &base_result {
            Ok(content) => build_file_content(content.clone(), options),
            Err(_) => None,
        }
    } else {
        None
    };

    let after = if options.include_after {
        match &head_result {
            Ok(content) => build_file_content(content.clone(), options),
            Err(_) => None,
        }
    } else {
        None
    };

    let is_binary = before
        .as_ref()
        .map(|c| c.content.is_empty() && c.size_bytes > 0)
        .unwrap_or(false)
        || after
            .as_ref()
            .map(|c| c.content.is_empty() && c.size_bytes > 0)
            .unwrap_or(false);

    let diff_hunks = if options.include_diff_hunks && !is_binary {
        compute_diff_hunks(&base_result, &head_result)
    } else {
        None
    };

    SnapshotFileEntry {
        path: file_path.to_string(),
        language: language.to_string(),
        is_binary,
        before,
        after,
        diff_hunks,
        error: None,
    }
}

fn compute_diff_hunks(
    base_result: &Result<String, crate::git::reader::GitError>,
    head_result: &Result<String, crate::git::reader::GitError>,
) -> Option<Vec<DiffHunk>> {
    let (Ok(old), Ok(new)) = (base_result, head_result) else {
        return None;
    };
    let raw_hunks = diff::extract_hunks(old.as_bytes(), new.as_bytes());
    if raw_hunks.is_empty() {
        None
    } else {
        Some(
            raw_hunks
                .into_iter()
                .map(|h| DiffHunk {
                    old_start: h.old_start,
                    old_lines: h.old_lines,
                    new_start: h.new_start,
                    new_lines: h.new_lines,
                })
                .collect(),
        )
    }
}

fn build_file_content(content: String, options: &SnapshotOptions) -> Option<FileContent> {
    // Check for binary (null bytes)
    if content.as_bytes().contains(&0) {
        return Some(FileContent {
            content: String::new(),
            line_count: 0,
            size_bytes: content.len(),
            truncated: false,
        });
    }

    let size_bytes = content.len();
    let is_truncated = size_bytes > options.max_file_size_bytes;

    let mut final_content = if is_truncated {
        let safe_len = content.floor_char_boundary(options.max_file_size_bytes);
        content[..safe_len].to_string()
    } else {
        content
    };

    // Apply line range if specified
    if let Some((start, end)) = options.line_range {
        let selected: Vec<&str> = final_content
            .lines()
            .skip(start.saturating_sub(1))
            .take(end - start.saturating_sub(1))
            .collect();
        final_content = selected.join("\n");
        if !final_content.is_empty() {
            final_content.push('\n');
        }
    }

    let line_count = final_content.lines().count();

    Some(FileContent {
        content: final_content,
        line_count,
        size_bytes,
        truncated: is_truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn create_snapshot_test_repo() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
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

        std::fs::write(
            path.join("hello.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();

        Command::new("git")
            .args(["add", "hello.rs"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        std::fs::write(
            path.join("hello.rs"),
            "fn main() {\n    println!(\"goodbye\");\n}\n",
        )
        .unwrap();
        std::fs::write(path.join("new.py"), "print('hi')\n").unwrap();

        Command::new("git")
            .args(["add", "hello.rs", "new.py"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "modify and add"])
            .current_dir(&path)
            .output()
            .unwrap();

        (dir, path)
    }

    #[test]
    fn it_returns_before_and_after_content() {
        let (_dir, path) = create_snapshot_test_repo();
        let options = SnapshotOptions {
            include_before: true,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: false,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["hello.rs".into()], &options).unwrap();

        assert_eq!(result.files.len(), 1);
        let file = &result.files[0];
        assert_eq!(file.language, "rust");
        assert!(file.before.is_some());
        assert!(file.after.is_some());
        assert!(file.before.as_ref().unwrap().content.contains("hello"));
        assert!(file.after.as_ref().unwrap().content.contains("goodbye"));
    }

    #[test]
    fn it_returns_none_for_added_file_before() {
        let (_dir, path) = create_snapshot_test_repo();
        let options = SnapshotOptions {
            include_before: true,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: false,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["new.py".into()], &options).unwrap();

        let file = &result.files[0];
        assert!(file.before.is_none());
        assert!(file.after.is_some());
    }

    #[test]
    fn it_respects_include_before_false() {
        let (_dir, path) = create_snapshot_test_repo();
        let options = SnapshotOptions {
            include_before: false,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: false,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["hello.rs".into()], &options).unwrap();

        assert!(result.files[0].before.is_none());
        assert!(result.files[0].after.is_some());
    }

    #[test]
    fn it_truncates_large_files() {
        let (_dir, path) = create_snapshot_test_repo();
        let options = SnapshotOptions {
            include_before: true,
            include_after: true,
            max_file_size_bytes: 10, // very small limit
            line_range: None,
            include_diff_hunks: false,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["hello.rs".into()], &options).unwrap();

        let after = result.files[0].after.as_ref().unwrap();
        assert!(after.truncated);
        assert!(after.content.len() <= 10);
        assert!(after.size_bytes > 10);
    }

    #[test]
    fn it_computes_token_estimate_from_content_length() {
        let (_dir, path) = create_snapshot_test_repo();
        let options = SnapshotOptions {
            include_before: true,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: false,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["hello.rs".into()], &options).unwrap();

        // before: "fn main() {\n    println!(\"hello\");\n}\n" = 38 chars
        // after:  "fn main() {\n    println!(\"goodbye\");\n}\n" = 40 chars
        // total = 78, estimate = 78 / 4 = 19
        assert_eq!(result.token_estimate, 19);
    }

    #[test]
    fn it_detects_binary_file_with_null_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
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

        // Write a text file first
        std::fs::write(path.join("data.bin"), "placeholder\n").unwrap();
        Command::new("git")
            .args(["add", "data.bin"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Replace with binary content containing null bytes
        std::fs::write(path.join("data.bin"), b"hello\x00world").unwrap();
        Command::new("git")
            .args(["add", "data.bin"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "make binary"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = SnapshotOptions {
            include_before: false,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: false,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["data.bin".into()], &options).unwrap();

        let file = &result.files[0];
        assert!(file.is_binary);
        let after = file.after.as_ref().unwrap();
        assert!(after.content.is_empty());
        assert!(after.size_bytes > 0);
    }

    #[test]
    fn it_applies_line_range_option() {
        let (_dir, path) = create_snapshot_test_repo();
        let options = SnapshotOptions {
            include_before: false,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: Some((2, 2)),
            include_diff_hunks: false,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["hello.rs".into()], &options).unwrap();

        let after = result.files[0].after.as_ref().unwrap();
        // Line 2 of "fn main() {\n    println!(\"goodbye\");\n}\n" is '    println!("goodbye");'
        assert!(after.content.contains("println!"));
        assert!(!after.content.contains("fn main"));
        assert_eq!(after.line_count, 1);
    }

    #[test]
    fn it_enforces_max_20_files() {
        let (_dir, path) = create_snapshot_test_repo();
        let options = SnapshotOptions {
            include_before: true,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: false,
        };
        let paths: Vec<String> = (0..25).map(|i| format!("file{i}.rs")).collect();
        let result = build_snapshots(&path, "HEAD~1", "HEAD", &paths, &options).unwrap();

        assert_eq!(result.files.len(), 20);
    }

    // --- Gap-closing tests for mutation testing ---

    #[test]
    fn it_does_not_truncate_at_exactly_max_snapshot_files() {
        // Kills: replace > with >= at line 22 (MAX_SNAPSHOT_FILES boundary)
        let (_dir, path) = create_snapshot_test_repo();
        let options = SnapshotOptions {
            include_before: true,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: false,
        };
        // Request exactly MAX_SNAPSHOT_FILES (20) paths
        let paths: Vec<String> = (0..MAX_SNAPSHOT_FILES)
            .map(|i| format!("file{i}.rs"))
            .collect();
        let result = build_snapshots(&path, "HEAD~1", "HEAD", &paths, &options).unwrap();

        assert_eq!(
            result.files.len(),
            MAX_SNAPSHOT_FILES,
            "should include all 20 files when exactly at the limit"
        );
    }

    #[test]
    fn it_truncates_at_21_snapshot_files() {
        // Confirms > vs >= boundary: 21 files should be truncated to 20
        let (_dir, path) = create_snapshot_test_repo();
        let options = SnapshotOptions {
            include_before: true,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: false,
        };
        let paths: Vec<String> = (0..MAX_SNAPSHOT_FILES + 1)
            .map(|i| format!("file{i}.rs"))
            .collect();
        let result = build_snapshots(&path, "HEAD~1", "HEAD", &paths, &options).unwrap();

        assert_eq!(
            result.files.len(),
            MAX_SNAPSHOT_FILES,
            "should truncate to MAX_SNAPSHOT_FILES when given 21 paths"
        );
    }

    #[test]
    fn it_detects_binary_via_before_content_only() {
        // Kills: replace && with || at line 86, replace > with == / < / >= at line 86
        // Tests the is_binary detection logic: before has empty content with size > 0
        let dir = tempfile::TempDir::new().unwrap();
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

        // Initial commit: binary file (null bytes)
        std::fs::write(path.join("img.bin"), b"binary\x00data").unwrap();
        Command::new("git")
            .args(["add", "img.bin"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add binary"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Second commit: replace with text
        std::fs::write(path.join("img.bin"), "now text").unwrap();
        Command::new("git")
            .args(["add", "img.bin"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "convert to text"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = SnapshotOptions {
            include_before: true,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: false,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["img.bin".into()], &options).unwrap();

        let file = &result.files[0];
        // before is binary (empty content, size > 0), after is text
        assert!(
            file.is_binary,
            "should detect binary from before content even when after is text"
        );
        let before = file.before.as_ref().unwrap();
        assert!(
            before.content.is_empty(),
            "binary before content should be empty"
        );
        assert!(
            before.size_bytes > 0,
            "binary before should have non-zero size"
        );
    }

    #[test]
    fn it_detects_binary_via_after_content_only() {
        // Kills: replace && with || at line 90, replace > with >= at line 90
        // Tests is_binary detection via the after side
        let dir = tempfile::TempDir::new().unwrap();
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

        // Initial commit: text file
        std::fs::write(path.join("data.bin"), "text content").unwrap();
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

        // Second commit: binary content
        std::fs::write(path.join("data.bin"), b"now\x00binary").unwrap();
        Command::new("git")
            .args(["add", "data.bin"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "make binary"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = SnapshotOptions {
            include_before: true,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: false,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["data.bin".into()], &options).unwrap();

        let file = &result.files[0];
        assert!(
            file.is_binary,
            "should detect binary from after content even when before is text"
        );
        let after = file.after.as_ref().unwrap();
        assert!(
            after.content.is_empty(),
            "binary after content should be empty"
        );
        assert!(
            after.size_bytes > 0,
            "binary after should have non-zero size"
        );
        // Confirm before is NOT binary
        let before = file.before.as_ref().unwrap();
        assert!(
            !before.content.is_empty(),
            "text before should have content"
        );
    }

    #[test]
    fn it_is_not_binary_when_content_is_empty_and_size_is_zero() {
        // Kills: replace > with == and replace > with >= at line 86 for size_bytes check
        // A truly empty file (0 bytes) should NOT be detected as binary
        let dir = tempfile::TempDir::new().unwrap();
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

        // Initial commit: non-empty file
        std::fs::write(path.join("empty.txt"), "something\n").unwrap();
        Command::new("git")
            .args(["add", "empty.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Second commit: truly empty file (0 bytes)
        std::fs::write(path.join("empty.txt"), "").unwrap();
        Command::new("git")
            .args(["add", "empty.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "make empty"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = SnapshotOptions {
            include_before: false,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: false,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["empty.txt".into()], &options).unwrap();

        let file = &result.files[0];
        assert!(
            !file.is_binary,
            "empty file (0 bytes) should NOT be detected as binary"
        );
        let after = file.after.as_ref().unwrap();
        assert_eq!(after.size_bytes, 0, "empty file should have size 0");
    }

    #[test]
    fn it_truncates_at_exact_boundary() {
        // Kills: replace > with >= at line 115 (build_file_content truncation)
        let (_dir, path) = create_snapshot_test_repo();

        // "fn main() {\n    println!(\"goodbye\");\n}\n" is 39 bytes
        // Set max to exactly 39 — should NOT truncate
        let options_exact = SnapshotOptions {
            include_before: false,
            include_after: true,
            max_file_size_bytes: 39,
            line_range: None,
            include_diff_hunks: false,
        };
        let result = build_snapshots(
            &path,
            "HEAD~1",
            "HEAD",
            &["hello.rs".into()],
            &options_exact,
        )
        .unwrap();

        let after = result.files[0].after.as_ref().unwrap();
        assert!(
            !after.truncated,
            "should NOT truncate when size == max_file_size_bytes"
        );
        assert_eq!(after.size_bytes, 39);

        // Set max to 38 — should truncate
        let options_minus_one = SnapshotOptions {
            include_before: false,
            include_after: true,
            max_file_size_bytes: 38,
            line_range: None,
            include_diff_hunks: false,
        };
        let result = build_snapshots(
            &path,
            "HEAD~1",
            "HEAD",
            &["hello.rs".into()],
            &options_minus_one,
        )
        .unwrap();

        let after = result.files[0].after.as_ref().unwrap();
        assert!(
            after.truncated,
            "should truncate when size > max_file_size_bytes"
        );
    }

    #[test]
    fn it_returns_non_empty_content_for_non_empty_text_file() {
        // Kills: delete ! at line 132 in build_file_content (empty content check)
        // The `!` negation matters: if content is NOT empty, push newline
        let (_dir, path) = create_snapshot_test_repo();
        let options = SnapshotOptions {
            include_before: false,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: Some((1, 1)),
            include_diff_hunks: false,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["hello.rs".into()], &options).unwrap();

        let after = result.files[0].after.as_ref().unwrap();
        // Line 1 of hello.rs is "fn main() {"
        assert!(
            !after.content.is_empty(),
            "content for line range should not be empty"
        );
        assert!(
            after.content.ends_with('\n'),
            "non-empty line-range content should end with newline"
        );
    }

    #[test]
    fn it_marks_normal_text_file_as_not_binary_via_before_check() {
        // Kills: replace && with || at line 85 (before-side check).
        // For a normal text `before` (is_empty=false, size>0), the && short-circuits
        // to false (not binary), but `||` evaluates to true (incorrectly binary).
        // Both before AND after are plain text — neither should trigger is_binary.
        let (_dir, path) = create_snapshot_test_repo();
        let options = SnapshotOptions {
            include_before: true,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: false,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["hello.rs".into()], &options).unwrap();

        let file = &result.files[0];
        // before is non-empty text, after is non-empty text — neither is binary
        let before = file.before.as_ref().unwrap();
        assert!(
            !before.content.is_empty(),
            "before should be non-empty text"
        );
        assert!(before.size_bytes > 0, "before should have non-zero size");
        let after = file.after.as_ref().unwrap();
        assert!(!after.content.is_empty(), "after should be non-empty text");
        assert!(after.size_bytes > 0, "after should have non-zero size");
        assert!(
            !file.is_binary,
            "plain text file should NOT be detected as binary"
        );
    }

    #[test]
    fn it_does_not_mark_zero_byte_before_as_binary() {
        // Kills: replace > with >= at line 85 (before-side `c.size_bytes > 0`).
        // When `before` is a truly empty file (is_empty=true, size_bytes==0):
        //   Original: true && (0 > 0)  = true && false = false → not binary
        //   Mutated:  true && (0 >= 0) = true && true  = true  → IS binary (wrong!)
        // The existing zero-byte test uses include_before:false (skips this branch);
        // this one explicitly enables the before side.
        let dir = tempfile::TempDir::new().unwrap();
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

        // Initial commit: truly empty file (0 bytes)
        std::fs::write(path.join("zero.txt"), "").unwrap();
        Command::new("git")
            .args(["add", "zero.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add empty"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Second commit: non-empty text
        std::fs::write(path.join("zero.txt"), "now has content\n").unwrap();
        Command::new("git")
            .args(["add", "zero.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "fill in"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = SnapshotOptions {
            include_before: true,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: false,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["zero.txt".into()], &options).unwrap();

        let file = &result.files[0];
        let before = file.before.as_ref().unwrap();
        assert_eq!(
            before.size_bytes, 0,
            "pre-flight: before must be a 0-byte file"
        );
        assert!(
            before.content.is_empty(),
            "pre-flight: before content must be empty"
        );
        assert!(
            !file.is_binary,
            "0-byte before with empty content should NOT be detected as binary"
        );
    }

    #[test]
    fn it_excludes_diff_hunks_when_include_diff_hunks_is_false() {
        let (_dir, path) = create_snapshot_test_repo();
        let options = SnapshotOptions {
            include_before: true,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: false,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["hello.rs".into()], &options).unwrap();

        let file = &result.files[0];
        assert!(file.diff_hunks.is_none());
    }

    #[test]
    fn it_includes_diff_hunks_for_modified_file_when_requested() {
        let (_dir, path) = create_snapshot_test_repo();
        let options = SnapshotOptions {
            include_before: true,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: true,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["hello.rs".into()], &options).unwrap();

        let file = &result.files[0];
        let hunks = file.diff_hunks.as_ref().expect("should have diff_hunks");
        assert!(!hunks.is_empty(), "should have at least one hunk");
        let h = &hunks[0];
        assert_eq!(
            h.old_start, 2,
            "old_start should be 2 (second line changed)"
        );
        assert_eq!(h.old_lines, 1, "old_lines should be 1 (one line removed)");
        assert_eq!(
            h.new_start, 2,
            "new_start should be 2 (second line inserted)"
        );
        assert_eq!(h.new_lines, 1, "new_lines should be 1 (one line inserted)");
    }

    #[test]
    fn it_includes_diff_hunks_when_include_before_is_false() {
        // diff_hunks are computed from blob reads that happen regardless of
        // include_before/include_after. Even when before content is excluded
        // from the response, hunks should still be available.
        let (_dir, path) = create_snapshot_test_repo();
        let options = SnapshotOptions {
            include_before: false,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: true,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["hello.rs".into()], &options).unwrap();

        let file = &result.files[0];
        assert!(file.before.is_none(), "before should be excluded");
        let hunks = file.diff_hunks.as_ref().expect("diff_hunks should exist even when include_before is false");
        assert!(!hunks.is_empty(), "should have at least one hunk");
    }

    #[test]
    fn it_excludes_diff_hunks_for_binary_file() {
        // Binary files should not produce diff hunks even when requested,
        // because extract_hunks returns empty for binary content.
        let dir = tempfile::TempDir::new().unwrap();
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

        std::fs::write(path.join("data.bin"), "text\n").unwrap();
        Command::new("git")
            .args(["add", "data.bin"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "text"])
            .current_dir(&path)
            .output()
            .unwrap();

        std::fs::write(path.join("data.bin"), b"bin\x00data").unwrap();
        Command::new("git")
            .args(["add", "data.bin"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "binary"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = SnapshotOptions {
            include_before: true,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: true,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["data.bin".into()], &options).unwrap();

        let file = &result.files[0];
        assert!(file.is_binary, "file should be detected as binary");
        assert!(file.diff_hunks.is_none(), "binary files should have no diff_hunks");
    }

    #[test]
    fn it_excludes_diff_hunks_for_deleted_file() {
        // Deleted files have no new content, so extract_hunks returns empty
        // and diff_hunks should be None.
        let (_dir, path) = create_snapshot_test_repo();

        // Delete new.py (added in second commit)
        std::fs::remove_file(path.join("new.py")).unwrap();
        Command::new("git")
            .args(["add", "new.py"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "delete new.py"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = SnapshotOptions {
            include_before: true,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: true,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["new.py".into()], &options).unwrap();

        let file = &result.files[0];
        assert!(file.before.is_some(), "deleted file should have before content");
        assert!(file.after.is_none(), "deleted file should have no after content");
        assert!(file.diff_hunks.is_none(), "deleted file should have no diff_hunks");
    }

    #[test]
    fn it_excludes_diff_hunks_for_added_file() {
        let (_dir, path) = create_snapshot_test_repo();
        let options = SnapshotOptions {
            include_before: true,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
            include_diff_hunks: true,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["new.py".into()], &options).unwrap();

        let file = &result.files[0];
        assert!(file.before.is_none());
        assert!(file.diff_hunks.is_none());
    }
}
