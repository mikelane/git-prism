use std::collections::HashMap;
use std::path::Path;

use chrono::Utc;

use crate::git::depfiles::{diff_dependencies, is_dependency_file};
use crate::git::diff::ChangeType;
use crate::git::generated::GeneratedFileDetector;
use crate::git::reader::RepoReader;
use crate::tools::types::{
    FunctionChange, FunctionChangeType, ImportChange, ManifestFileEntry, ManifestMetadata,
    ManifestOptions, ManifestResponse, ManifestSummary, ToolError, TruncationInfo, detect_language,
};
use crate::treesitter::{Function, analyzer_for_extension};

const MAX_FILES: usize = 200;

pub fn diff_functions(base_fns: &[Function], head_fns: &[Function]) -> Vec<FunctionChange> {
    let base_map: HashMap<&str, &Function> =
        base_fns.iter().map(|f| (f.name.as_str(), f)).collect();
    let head_map: HashMap<&str, &Function> =
        head_fns.iter().map(|f| (f.name.as_str(), f)).collect();

    let mut changes = Vec::new();

    for (name, head_fn) in &head_map {
        match base_map.get(name) {
            None => changes.push(FunctionChange {
                name: name.to_string(),
                change_type: FunctionChangeType::Added,
                start_line: head_fn.start_line,
                end_line: head_fn.end_line,
                signature: head_fn.signature.clone(),
            }),
            Some(base_fn) => {
                if base_fn.signature != head_fn.signature {
                    changes.push(FunctionChange {
                        name: name.to_string(),
                        change_type: FunctionChangeType::SignatureChanged,
                        start_line: head_fn.start_line,
                        end_line: head_fn.end_line,
                        signature: head_fn.signature.clone(),
                    });
                } else if base_fn.start_line != head_fn.start_line
                    || base_fn.end_line != head_fn.end_line
                {
                    changes.push(FunctionChange {
                        name: name.to_string(),
                        change_type: FunctionChangeType::Modified,
                        start_line: head_fn.start_line,
                        end_line: head_fn.end_line,
                        signature: head_fn.signature.clone(),
                    });
                }
            }
        }
    }

    for (name, base_fn) in &base_map {
        if !head_map.contains_key(name) {
            changes.push(FunctionChange {
                name: name.to_string(),
                change_type: FunctionChangeType::Deleted,
                start_line: base_fn.start_line,
                end_line: base_fn.end_line,
                signature: base_fn.signature.clone(),
            });
        }
    }

    changes.sort_by(|a, b| a.name.cmp(&b.name));
    changes
}

pub fn diff_imports(base_imports: &[String], head_imports: &[String]) -> ImportChange {
    let base_set: std::collections::HashSet<&str> =
        base_imports.iter().map(|s| s.as_str()).collect();
    let head_set: std::collections::HashSet<&str> =
        head_imports.iter().map(|s| s.as_str()).collect();

    let mut added: Vec<String> = head_set
        .difference(&base_set)
        .map(|s| s.to_string())
        .collect();
    let mut removed: Vec<String> = base_set
        .difference(&head_set)
        .map(|s| s.to_string())
        .collect();

    added.sort();
    removed.sort();

    ImportChange { added, removed }
}

fn extension_from_path(path: &str) -> &str {
    path.rsplit('.')
        .next()
        .filter(|ext| path.len() > ext.len() + 1)
        .unwrap_or("")
}

fn matches_glob_pattern(path: &str, pattern: &str) -> bool {
    glob::Pattern::new(pattern)
        .map(|p| {
            p.matches_with(
                path,
                glob::MatchOptions {
                    require_literal_separator: false,
                    require_literal_leading_dot: false,
                    case_sensitive: true,
                },
            )
        })
        .unwrap_or(false)
}

pub fn build_manifest(
    repo_path: &Path,
    base_ref: &str,
    head_ref: &str,
    options: &ManifestOptions,
) -> Result<ManifestResponse, ToolError> {
    let reader = RepoReader::open(repo_path)?;

    let base_commit = reader.resolve_commit(base_ref)?;
    let head_commit = reader.resolve_commit(head_ref)?;

    let diff_result = reader.diff_commits(base_ref, head_ref)?;

    let mut files_to_process = diff_result.files;

    // Apply include patterns
    if !options.include_patterns.is_empty() {
        files_to_process.retain(|f| {
            options
                .include_patterns
                .iter()
                .any(|p| matches_glob_pattern(&f.path, p))
        });
    }

    // Apply exclude patterns
    if !options.exclude_patterns.is_empty() {
        files_to_process.retain(|f| {
            !options
                .exclude_patterns
                .iter()
                .any(|p| matches_glob_pattern(&f.path, p))
        });
    }

    // Truncation
    let total_files = files_to_process.len();
    let truncated = total_files > MAX_FILES;
    let truncation_info = if truncated {
        files_to_process.truncate(MAX_FILES);
        Some(TruncationInfo {
            total_files,
            files_included: MAX_FILES,
            files_omitted: total_files - MAX_FILES,
        })
    } else {
        None
    };

    let mut manifest_files = Vec::new();
    let mut dependency_changes = Vec::new();
    let mut languages_set = std::collections::HashSet::new();
    let mut total_functions_changed: Option<usize> = None;

    for file_change in &files_to_process {
        let language = detect_language(&file_change.path);
        let ext = extension_from_path(&file_change.path);
        let is_generated = GeneratedFileDetector::is_generated(&file_change.path, None);

        if language != "unknown" {
            languages_set.insert(language.to_string());
        }

        // Function and import analysis
        let (functions_changed, imports_changed) = if let Some(analyzer) = options
            .include_function_analysis
            .then(|| analyzer_for_extension(ext))
            .flatten()
        {
            let base_content = match file_change.change_type {
                ChangeType::Added => None,
                _ => reader.read_file_at_ref(base_ref, &file_change.path).ok(),
            };

            let head_content = match file_change.change_type {
                ChangeType::Deleted => None,
                _ => reader.read_file_at_ref(head_ref, &file_change.path).ok(),
            };

            let base_fns = base_content
                .as_ref()
                .and_then(|c| analyzer.extract_functions(c.as_bytes()).ok())
                .unwrap_or_default();

            let head_fns = head_content
                .as_ref()
                .and_then(|c| analyzer.extract_functions(c.as_bytes()).ok())
                .unwrap_or_default();

            let fn_changes = diff_functions(&base_fns, &head_fns);

            let count = fn_changes.len();
            *total_functions_changed.get_or_insert(0) += count;

            let base_imports = base_content
                .as_ref()
                .and_then(|c| analyzer.extract_imports(c.as_bytes()).ok())
                .unwrap_or_default();

            let head_imports = head_content
                .as_ref()
                .and_then(|c| analyzer.extract_imports(c.as_bytes()).ok())
                .unwrap_or_default();

            let import_change = diff_imports(&base_imports, &head_imports);

            (Some(fn_changes), Some(import_change))
        } else {
            (None, None)
        };

        // Dependency analysis
        if is_dependency_file(&file_change.path) {
            let base_content = match file_change.change_type {
                ChangeType::Added => String::new(),
                _ => reader
                    .read_file_at_ref(base_ref, &file_change.path)
                    .unwrap_or_default(),
            };

            let head_content = match file_change.change_type {
                ChangeType::Deleted => String::new(),
                _ => reader
                    .read_file_at_ref(head_ref, &file_change.path)
                    .unwrap_or_default(),
            };

            if let Some(dep_diff) =
                diff_dependencies(&file_change.path, &base_content, &head_content)
            {
                dependency_changes.push(dep_diff);
            }
        }

        manifest_files.push(ManifestFileEntry {
            path: file_change.path.clone(),
            old_path: file_change.old_path.clone(),
            change_type: file_change.change_type,
            language: language.to_string(),
            is_binary: file_change.is_binary,
            is_generated,
            lines_added: file_change.lines_added,
            lines_removed: file_change.lines_removed,
            size_before: file_change.size_before,
            size_after: file_change.size_after,
            functions_changed,
            imports_changed,
        });
    }

    let mut languages_affected: Vec<String> = languages_set.into_iter().collect();
    languages_affected.sort();

    let summary = ManifestSummary {
        total_files_changed: manifest_files.len(),
        files_added: manifest_files
            .iter()
            .filter(|f| f.change_type == ChangeType::Added)
            .count(),
        files_modified: manifest_files
            .iter()
            .filter(|f| f.change_type == ChangeType::Modified)
            .count(),
        files_deleted: manifest_files
            .iter()
            .filter(|f| f.change_type == ChangeType::Deleted)
            .count(),
        files_renamed: manifest_files
            .iter()
            .filter(|f| f.change_type == ChangeType::Renamed)
            .count(),
        total_lines_added: manifest_files.iter().map(|f| f.lines_added).sum(),
        total_lines_removed: manifest_files.iter().map(|f| f.lines_removed).sum(),
        total_functions_changed,
        languages_affected,
    };

    Ok(ManifestResponse {
        metadata: ManifestMetadata {
            repo_path: repo_path.display().to_string(),
            base_ref: base_ref.to_string(),
            head_ref: head_ref.to_string(),
            base_sha: base_commit.sha,
            head_sha: head_commit.sha,
            generated_at: Utc::now(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        summary,
        files: manifest_files,
        dependency_changes,
        truncated,
        truncation_info,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_detects_added_function() {
        let base = vec![];
        let head = vec![Function {
            name: "foo".into(),
            signature: "fn foo()".into(),
            start_line: 1,
            end_line: 3,
        }];
        let changes = diff_functions(&base, &head);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].name, "foo");
        assert_eq!(changes[0].change_type, FunctionChangeType::Added);
    }

    #[test]
    fn it_detects_deleted_function() {
        let base = vec![Function {
            name: "bar".into(),
            signature: "fn bar()".into(),
            start_line: 1,
            end_line: 3,
        }];
        let head = vec![];
        let changes = diff_functions(&base, &head);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].name, "bar");
        assert_eq!(changes[0].change_type, FunctionChangeType::Deleted);
    }

    #[test]
    fn it_detects_signature_changed_function() {
        let base = vec![Function {
            name: "baz".into(),
            signature: "fn baz()".into(),
            start_line: 1,
            end_line: 3,
        }];
        let head = vec![Function {
            name: "baz".into(),
            signature: "fn baz(x: i32)".into(),
            start_line: 1,
            end_line: 5,
        }];
        let changes = diff_functions(&base, &head);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change_type, FunctionChangeType::SignatureChanged);
    }

    #[test]
    fn it_detects_modified_function_by_line_range() {
        let base = vec![Function {
            name: "qux".into(),
            signature: "fn qux()".into(),
            start_line: 1,
            end_line: 3,
        }];
        let head = vec![Function {
            name: "qux".into(),
            signature: "fn qux()".into(),
            start_line: 1,
            end_line: 10,
        }];
        let changes = diff_functions(&base, &head);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change_type, FunctionChangeType::Modified);
    }

    #[test]
    fn it_returns_empty_for_identical_functions() {
        let fns = vec![Function {
            name: "same".into(),
            signature: "fn same()".into(),
            start_line: 1,
            end_line: 3,
        }];
        let changes = diff_functions(&fns, &fns);
        assert!(changes.is_empty());
    }

    #[test]
    fn it_diffs_imports_correctly() {
        let base = vec!["fmt".to_string(), "os".to_string()];
        let head = vec!["fmt".to_string(), "io".to_string()];
        let change = diff_imports(&base, &head);
        assert_eq!(change.added, vec!["io"]);
        assert_eq!(change.removed, vec!["os"]);
    }

    #[test]
    fn it_returns_empty_import_diff_for_identical_imports() {
        let imports = vec!["fmt".to_string()];
        let change = diff_imports(&imports, &imports);
        assert!(change.added.is_empty());
        assert!(change.removed.is_empty());
    }

    #[test]
    fn it_handles_mixed_function_changes() {
        let base = vec![
            Function {
                name: "kept".into(),
                signature: "fn kept()".into(),
                start_line: 1,
                end_line: 3,
            },
            Function {
                name: "removed".into(),
                signature: "fn removed()".into(),
                start_line: 5,
                end_line: 7,
            },
            Function {
                name: "changed_sig".into(),
                signature: "fn changed_sig()".into(),
                start_line: 9,
                end_line: 11,
            },
        ];
        let head = vec![
            Function {
                name: "kept".into(),
                signature: "fn kept()".into(),
                start_line: 1,
                end_line: 3,
            },
            Function {
                name: "added".into(),
                signature: "fn added()".into(),
                start_line: 5,
                end_line: 7,
            },
            Function {
                name: "changed_sig".into(),
                signature: "fn changed_sig(x: i32)".into(),
                start_line: 9,
                end_line: 13,
            },
        ];
        let changes = diff_functions(&base, &head);
        assert_eq!(changes.len(), 3);

        let added = changes.iter().find(|c| c.name == "added").unwrap();
        assert_eq!(added.change_type, FunctionChangeType::Added);

        let removed = changes.iter().find(|c| c.name == "removed").unwrap();
        assert_eq!(removed.change_type, FunctionChangeType::Deleted);

        let sig = changes.iter().find(|c| c.name == "changed_sig").unwrap();
        assert_eq!(sig.change_type, FunctionChangeType::SignatureChanged);
    }

    fn create_repo_with_go_file() -> (tempfile::TempDir, std::path::PathBuf) {
        use std::process::Command;
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

        // Base commit: a Go file with one function
        std::fs::write(
            path.join("main.go"),
            "package main\n\nimport \"fmt\"\n\nfunc hello() {\n\tfmt.Println(\"hello\")\n}\n",
        )
        .unwrap();
        std::fs::write(path.join("README.md"), "# Test\n").unwrap();

        Command::new("git")
            .args(["add", "main.go", "README.md"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Head commit: add a function, modify README
        std::fs::write(
            path.join("main.go"),
            "package main\n\nimport (\n\t\"fmt\"\n\t\"os\"\n)\n\nfunc hello() {\n\tfmt.Println(\"hello\")\n}\n\nfunc goodbye() {\n\tfmt.Println(\"bye\")\n\tos.Exit(0)\n}\n",
        )
        .unwrap();
        std::fs::write(path.join("README.md"), "# Test Project\n\nUpdated.\n").unwrap();

        Command::new("git")
            .args(["add", "main.go", "README.md"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add goodbye function"])
            .current_dir(&path)
            .output()
            .unwrap();

        (dir, path)
    }

    #[test]
    fn it_builds_manifest_with_function_analysis() {
        let (_dir, path) = create_repo_with_go_file();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: true,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options).unwrap();

        assert_eq!(manifest.summary.total_files_changed, 2);
        assert!(!manifest.truncated);

        let go_file = manifest.files.iter().find(|f| f.path == "main.go").unwrap();
        assert_eq!(go_file.language, "go");
        assert!(!go_file.is_generated);

        // Function analysis: goodbye was added
        let fns = go_file.functions_changed.as_ref().unwrap();
        let added_fn = fns.iter().find(|f| f.name == "goodbye").unwrap();
        assert_eq!(added_fn.change_type, FunctionChangeType::Added);

        // Import analysis: os was added
        let imports = go_file.imports_changed.as_ref().unwrap();
        assert!(imports.added.iter().any(|i| i.contains("os")));

        // README has no function analysis (unknown language)
        let readme = manifest
            .files
            .iter()
            .find(|f| f.path == "README.md")
            .unwrap();
        assert!(readme.functions_changed.is_none());
    }

    #[test]
    fn it_applies_exclude_patterns() {
        let (_dir, path) = create_repo_with_go_file();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec!["*.md".to_string()],
            include_function_analysis: false,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options).unwrap();

        assert_eq!(manifest.summary.total_files_changed, 1);
        assert_eq!(manifest.files[0].path, "main.go");
    }

    #[test]
    fn it_applies_include_patterns() {
        let (_dir, path) = create_repo_with_go_file();
        let options = ManifestOptions {
            include_patterns: vec!["*.go".to_string()],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options).unwrap();

        assert_eq!(manifest.summary.total_files_changed, 1);
        assert_eq!(manifest.files[0].path, "main.go");
    }

    #[test]
    fn it_sets_functions_changed_to_none_without_analysis() {
        let (_dir, path) = create_repo_with_go_file();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options).unwrap();

        for file in &manifest.files {
            assert!(file.functions_changed.is_none());
            assert!(file.imports_changed.is_none());
        }
        assert!(manifest.summary.total_functions_changed.is_none());
    }

    #[test]
    fn it_sorts_function_changes_by_name() {
        let base = vec![];
        let head = vec![
            Function {
                name: "zebra".into(),
                signature: "fn zebra()".into(),
                start_line: 1,
                end_line: 2,
            },
            Function {
                name: "alpha".into(),
                signature: "fn alpha()".into(),
                start_line: 3,
                end_line: 4,
            },
        ];
        let changes = diff_functions(&base, &head);
        assert_eq!(changes[0].name, "alpha");
        assert_eq!(changes[1].name, "zebra");
    }

    // --- extension_from_path tests ---

    #[test]
    fn it_extracts_extension_from_normal_path() {
        assert_eq!(extension_from_path("src/main.rs"), "rs");
    }

    #[test]
    fn it_returns_empty_for_dotfile() {
        assert_eq!(extension_from_path(".gitignore"), "");
    }

    #[test]
    fn it_returns_empty_for_no_extension() {
        assert_eq!(extension_from_path("Makefile"), "");
    }

    #[test]
    fn it_extracts_last_extension_from_multiple_dots() {
        assert_eq!(extension_from_path("archive.tar.gz"), "gz");
    }

    #[test]
    fn it_returns_empty_for_empty_string() {
        assert_eq!(extension_from_path(""), "");
    }

    // --- matches_glob_pattern tests ---

    #[test]
    fn it_matches_glob_across_directory_separators() {
        assert!(matches_glob_pattern("src/lib.rs", "*.rs"));
    }

    #[test]
    fn it_matches_glob_at_root_level() {
        assert!(matches_glob_pattern("main.rs", "*.rs"));
    }

    #[test]
    fn it_matches_exact_filename_pattern() {
        assert!(matches_glob_pattern("Cargo.toml", "Cargo.toml"));
    }

    #[test]
    fn it_returns_false_for_invalid_glob() {
        assert!(!matches_glob_pattern("main.rs", "[invalid"));
    }

    #[test]
    fn it_is_case_sensitive() {
        assert!(!matches_glob_pattern("main.rs", "*.RS"));
    }

    // --- diff_imports edge cases ---

    #[test]
    fn it_returns_empty_diff_when_both_import_lists_are_empty() {
        let base: Vec<String> = vec![];
        let head: Vec<String> = vec![];
        let change = diff_imports(&base, &head);
        assert!(change.added.is_empty());
        assert!(change.removed.is_empty());
    }

    #[test]
    fn it_reports_all_added_when_base_imports_are_empty() {
        let base: Vec<String> = vec![];
        let head = vec!["fmt".to_string(), "os".to_string()];
        let change = diff_imports(&base, &head);
        assert_eq!(change.added, vec!["fmt", "os"]);
        assert!(change.removed.is_empty());
    }

    #[test]
    fn it_reports_all_removed_when_head_imports_are_empty() {
        let base = vec!["fmt".to_string(), "os".to_string()];
        let head: Vec<String> = vec![];
        let change = diff_imports(&base, &head);
        assert!(change.added.is_empty());
        assert_eq!(change.removed, vec!["fmt", "os"]);
    }
}
