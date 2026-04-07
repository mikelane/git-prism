use std::path::Path;

use chrono::Utc;

use crate::git::reader::RepoReader;
use crate::tools::types::{
    FileContent, SnapshotFileEntry, SnapshotMetadata, SnapshotOptions, SnapshotResponse, ToolError,
    detect_language,
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

    let paths_to_process = if paths.len() > MAX_SNAPSHOT_FILES {
        &paths[..MAX_SNAPSHOT_FILES]
    } else {
        paths
    };

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
        token_estimate: total_chars / 4,
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
        match base_result {
            Ok(content) => build_file_content(content, options),
            Err(_) => None,
        }
    } else {
        None
    };

    let after = if options.include_after {
        match head_result {
            Ok(content) => build_file_content(content, options),
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

    SnapshotFileEntry {
        path: file_path.to_string(),
        language: language.to_string(),
        is_binary,
        before,
        after,
        error: None,
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
    let truncated = size_bytes > options.max_file_size_bytes;

    let mut final_content = if truncated {
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
        truncated,
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
}
