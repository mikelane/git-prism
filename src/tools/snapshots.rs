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
        content[..options.max_file_size_bytes].to_string()
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
    fn it_computes_token_estimate() {
        let (_dir, path) = create_snapshot_test_repo();
        let options = SnapshotOptions {
            include_before: true,
            include_after: true,
            max_file_size_bytes: 100_000,
            line_range: None,
        };
        let result =
            build_snapshots(&path, "HEAD~1", "HEAD", &["hello.rs".into()], &options).unwrap();

        assert!(result.token_estimate > 0);
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
}
