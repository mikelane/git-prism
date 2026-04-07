use std::collections::HashMap;
use std::path::Path;

use chrono::Utc;

use crate::git::depfiles::{diff_dependencies, is_dependency_file};
use crate::git::diff::ChangeType;
use crate::git::generated::GeneratedFileDetector;
use crate::git::reader::RepoReader;
use crate::pagination::{PaginationCursor, PaginationInfo, encode_cursor};
use crate::tools::types::{
    FunctionChange, FunctionChangeType, ImportChange, ManifestFileEntry, ManifestMetadata,
    ManifestOptions, ManifestResponse, ManifestSummary, ToolError, detect_language,
};
use crate::treesitter::{Function, analyzer_for_extension};

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
    offset: usize,
    page_size: usize,
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

    let total_files = files_to_process.len();
    let is_paginating = offset > 0 || total_files > page_size;

    // Build summary from ALL files (before pagination)
    let mut all_languages_set = std::collections::HashSet::new();
    let mut summary_files_added = 0usize;
    let mut summary_files_modified = 0usize;
    let mut summary_files_deleted = 0usize;
    let mut summary_files_renamed = 0usize;
    let mut summary_lines_added = 0usize;
    let mut summary_lines_removed = 0usize;

    for file_change in &files_to_process {
        let language = detect_language(&file_change.path);
        if language != "unknown" {
            all_languages_set.insert(language.to_string());
        }
        match file_change.change_type {
            ChangeType::Added => summary_files_added += 1,
            ChangeType::Modified => summary_files_modified += 1,
            ChangeType::Deleted => summary_files_deleted += 1,
            ChangeType::Renamed | ChangeType::Copied => summary_files_renamed += 1,
        }
        summary_lines_added += file_change.lines_added;
        summary_lines_removed += file_change.lines_removed;
    }

    let mut all_languages_affected: Vec<String> = all_languages_set.into_iter().collect();
    all_languages_affected.sort();

    // Dependency analysis runs on ALL files (not paginated)
    let mut dependency_changes = Vec::new();
    for file_change in &files_to_process {
        if is_dependency_file(&file_change.path) {
            let _dep_span = tracing::info_span!("manifest.diff_dependencies").entered();
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
    }

    // Apply pagination: select only the current page of files
    let page_end = (offset + page_size).min(total_files);
    let page_files = if offset < total_files {
        &files_to_process[offset..page_end]
    } else {
        &[]
    };

    // Tree-sitter analysis ONLY on paginated files
    let mut manifest_files = Vec::new();
    let mut total_functions_changed: Option<usize> = None;

    for file_change in page_files {
        let language = detect_language(&file_change.path);
        let ext = extension_from_path(&file_change.path);
        let is_generated = {
            let _span = tracing::info_span!("manifest.detect_generated").entered();
            GeneratedFileDetector::is_generated(&file_change.path, None)
        };

        let _file_span =
            tracing::info_span!("manifest.analyze_file", file.language = language).entered();

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

            // Single treesitter.parse span wrapping all parsing (base+head functions+imports)
            let (base_fns, head_fns, base_imports, head_imports) = {
                let _parse_span =
                    tracing::info_span!("treesitter.parse", language = language).entered();

                let base_fns = base_content
                    .as_ref()
                    .and_then(|c| analyzer.extract_functions(c.as_bytes()).ok())
                    .unwrap_or_default();
                let head_fns = head_content
                    .as_ref()
                    .and_then(|c| analyzer.extract_functions(c.as_bytes()).ok())
                    .unwrap_or_default();
                let base_imports = base_content
                    .as_ref()
                    .and_then(|c| analyzer.extract_imports(c.as_bytes()).ok())
                    .unwrap_or_default();
                let head_imports = head_content
                    .as_ref()
                    .and_then(|c| analyzer.extract_imports(c.as_bytes()).ok())
                    .unwrap_or_default();

                (base_fns, head_fns, base_imports, head_imports)
            };

            let fn_changes = {
                let _span = tracing::info_span!("treesitter.extract_functions").entered();
                diff_functions(&base_fns, &head_fns)
            };

            if !is_paginating {
                let count = fn_changes.len();
                *total_functions_changed.get_or_insert(0) += count;
            }

            let import_change = {
                let _span = tracing::info_span!("treesitter.extract_imports").entered();
                diff_imports(&base_imports, &head_imports)
            };

            (Some(fn_changes), Some(import_change))
        } else {
            (None, None)
        };

        manifest_files.push(ManifestFileEntry {
            path: file_change.path.clone(),
            old_path: file_change.old_path.clone(),
            change_type: file_change.change_type,
            change_scope: file_change.change_scope,
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

    let next_cursor = if page_end < total_files {
        Some(encode_cursor(&PaginationCursor {
            v: 1,
            offset: page_end,
            base_sha: base_commit.sha.clone(),
            head_sha: head_commit.sha.clone(),
        }))
    } else {
        None
    };

    let summary = ManifestSummary {
        total_files_changed: total_files,
        files_added: summary_files_added,
        files_modified: summary_files_modified,
        files_deleted: summary_files_deleted,
        files_renamed: summary_files_renamed,
        total_lines_added: summary_lines_added,
        total_lines_removed: summary_lines_removed,
        total_functions_changed: if is_paginating {
            None
        } else {
            total_functions_changed
        },
        languages_affected: all_languages_affected,
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
        pagination: PaginationInfo {
            total_files,
            page_start: offset,
            page_size,
            next_cursor,
        },
    })
}

/// Build a manifest comparing a committed ref against the current working tree.
///
/// Unlike [`build_manifest`] which compares two committed trees, this reads
/// staged and unstaged changes via `git status` semantics. Each file entry
/// carries a `change_scope` of `Staged` or `Unstaged`. Function analysis
/// reads file content from disk rather than the object database.
pub fn build_worktree_manifest(
    repo_path: &Path,
    base_ref: &str,
    options: &ManifestOptions,
    offset: usize,
    page_size: usize,
) -> Result<ManifestResponse, ToolError> {
    let reader = RepoReader::open(repo_path)?;
    let base_commit = reader.resolve_commit(base_ref)?;

    let diff_result = reader.diff_worktree()?;

    let mut files_to_process = diff_result.files;

    if !options.include_patterns.is_empty() {
        files_to_process.retain(|f| {
            options
                .include_patterns
                .iter()
                .any(|p| matches_glob_pattern(&f.path, p))
        });
    }

    if !options.exclude_patterns.is_empty() {
        files_to_process.retain(|f| {
            !options
                .exclude_patterns
                .iter()
                .any(|p| matches_glob_pattern(&f.path, p))
        });
    }

    let total_files = files_to_process.len();
    let is_paginating = offset > 0 || total_files > page_size;

    // Build summary from ALL files (before pagination)
    let mut all_languages_set = std::collections::HashSet::new();
    let mut summary_files_added = 0usize;
    let mut summary_files_modified = 0usize;
    let mut summary_files_deleted = 0usize;
    let mut summary_files_renamed = 0usize;
    let mut summary_lines_added = 0usize;
    let mut summary_lines_removed = 0usize;

    for file_change in &files_to_process {
        let language = detect_language(&file_change.path);
        if language != "unknown" {
            all_languages_set.insert(language.to_string());
        }
        match file_change.change_type {
            ChangeType::Added => summary_files_added += 1,
            ChangeType::Modified => summary_files_modified += 1,
            ChangeType::Deleted => summary_files_deleted += 1,
            ChangeType::Renamed | ChangeType::Copied => summary_files_renamed += 1,
        }
        summary_lines_added += file_change.lines_added;
        summary_lines_removed += file_change.lines_removed;
    }

    let mut all_languages_affected: Vec<String> = all_languages_set.into_iter().collect();
    all_languages_affected.sort();

    // Apply pagination: select only the current page of files
    let page_end = (offset + page_size).min(total_files);
    let page_files = if offset < total_files {
        &files_to_process[offset..page_end]
    } else {
        &[]
    };

    // Tree-sitter analysis ONLY on paginated files
    let mut manifest_files = Vec::new();
    let mut total_functions_changed: Option<usize> = None;

    for file_change in page_files {
        let language = detect_language(&file_change.path);
        let ext = extension_from_path(&file_change.path);
        let is_generated = {
            let _span = tracing::info_span!("manifest.detect_generated").entered();
            GeneratedFileDetector::is_generated(&file_change.path, None)
        };

        let _file_span =
            tracing::info_span!("manifest.analyze_file", file.language = language).entered();

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
                _ => match &file_change.staged_blob_id {
                    Some(blob_id) => reader.read_blob(blob_id).ok(),
                    None => read_worktree_file(repo_path, &file_change.path),
                },
            };

            let (base_fns, head_fns, base_imports, head_imports) = {
                let _parse_span =
                    tracing::info_span!("treesitter.parse", language = language).entered();

                let base_fns = base_content
                    .as_ref()
                    .and_then(|c| analyzer.extract_functions(c.as_bytes()).ok())
                    .unwrap_or_default();
                let head_fns = head_content
                    .as_ref()
                    .and_then(|c| analyzer.extract_functions(c.as_bytes()).ok())
                    .unwrap_or_default();
                let base_imports = base_content
                    .as_ref()
                    .and_then(|c| analyzer.extract_imports(c.as_bytes()).ok())
                    .unwrap_or_default();
                let head_imports = head_content
                    .as_ref()
                    .and_then(|c| analyzer.extract_imports(c.as_bytes()).ok())
                    .unwrap_or_default();

                (base_fns, head_fns, base_imports, head_imports)
            };

            let fn_changes = {
                let _span = tracing::info_span!("treesitter.extract_functions").entered();
                diff_functions(&base_fns, &head_fns)
            };

            if !is_paginating {
                let count = fn_changes.len();
                *total_functions_changed.get_or_insert(0) += count;
            }

            let import_change = {
                let _span = tracing::info_span!("treesitter.extract_imports").entered();
                diff_imports(&base_imports, &head_imports)
            };

            (Some(fn_changes), Some(import_change))
        } else {
            (None, None)
        };

        manifest_files.push(ManifestFileEntry {
            path: file_change.path.clone(),
            old_path: file_change.old_path.clone(),
            change_type: file_change.change_type,
            change_scope: file_change.change_scope,
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

    let next_cursor = if page_end < total_files {
        Some(encode_cursor(&PaginationCursor {
            v: 1,
            offset: page_end,
            base_sha: base_commit.sha.clone(),
            head_sha: "WORKTREE".to_string(),
        }))
    } else {
        None
    };

    let summary = ManifestSummary {
        total_files_changed: total_files,
        files_added: summary_files_added,
        files_modified: summary_files_modified,
        files_deleted: summary_files_deleted,
        files_renamed: summary_files_renamed,
        total_lines_added: summary_lines_added,
        total_lines_removed: summary_lines_removed,
        total_functions_changed: if is_paginating {
            None
        } else {
            total_functions_changed
        },
        languages_affected: all_languages_affected,
    };

    Ok(ManifestResponse {
        metadata: ManifestMetadata {
            repo_path: repo_path.display().to_string(),
            base_ref: base_ref.to_string(),
            head_ref: "WORKTREE".to_string(),
            base_sha: base_commit.sha,
            head_sha: "WORKTREE".to_string(),
            generated_at: Utc::now(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        summary,
        files: manifest_files,
        dependency_changes: vec![],
        pagination: PaginationInfo {
            total_files,
            page_start: offset,
            page_size,
            next_cursor,
        },
    })
}

fn read_worktree_file(repo_path: &Path, file_path: &str) -> Option<String> {
    let full_path = repo_path.join(file_path);
    std::fs::read_to_string(&full_path).ok()
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
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

        assert_eq!(manifest.summary.total_files_changed, 2);
        assert!(manifest.pagination.next_cursor.is_none());

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
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

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
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

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
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

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

    #[test]
    fn it_reads_staged_content_from_index_not_disk_for_function_analysis() {
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

        // Initial commit with a Python file containing one function
        std::fs::write(path.join("lib.py"), "def original():\n    return 1\n").unwrap();
        Command::new("git")
            .args(["add", "lib.py"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Stage a version with a new function "staged_fn"
        std::fs::write(
            path.join("lib.py"),
            "def original():\n    return 1\n\ndef staged_fn():\n    return 2\n",
        )
        .unwrap();
        Command::new("git")
            .args(["add", "lib.py"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Now modify disk to have a DIFFERENT function "disk_fn" instead of "staged_fn"
        std::fs::write(
            path.join("lib.py"),
            "def original():\n    return 1\n\ndef disk_fn():\n    return 3\n",
        )
        .unwrap();

        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: true,
        };
        let manifest = build_worktree_manifest(&path, "HEAD", &options, 0, 200).unwrap();

        // The staged entry should show "staged_fn" (from index), not "disk_fn" (from disk)
        let staged_entry = manifest
            .files
            .iter()
            .find(|f| f.path == "lib.py" && f.change_scope == crate::git::diff::ChangeScope::Staged)
            .expect("should have a staged entry for lib.py");

        let fns = staged_entry
            .functions_changed
            .as_ref()
            .expect("should have function analysis");

        assert!(
            fns.iter().any(|f| f.name == "staged_fn"),
            "staged entry should show 'staged_fn' from index, not 'disk_fn' from disk. Got: {:?}",
            fns.iter().map(|f| &f.name).collect::<Vec<_>>()
        );
        assert!(
            !fns.iter().any(|f| f.name == "disk_fn"),
            "staged entry should NOT show 'disk_fn' from disk"
        );
    }

    #[test]
    fn it_builds_worktree_manifest_with_staged_file() {
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

        std::fs::write(path.join("existing.txt"), "hello\n").unwrap();
        Command::new("git")
            .args(["add", "existing.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Stage a new file
        std::fs::write(path.join("new.py"), "def foo(): pass\n").unwrap();
        Command::new("git")
            .args(["add", "new.py"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };
        let manifest = build_worktree_manifest(&path, "HEAD", &options, 0, 200).unwrap();

        assert!(manifest.summary.total_files_changed > 0);
        let new_file = manifest.files.iter().find(|f| f.path == "new.py").unwrap();
        assert_eq!(new_file.change_type, ChangeType::Added);
    }

    // --- Gap-closing tests for mutation testing ---

    /// Helper: create a git repo with N files changed between two commits.
    fn create_repo_with_n_files(n: usize) -> (tempfile::TempDir, std::path::PathBuf) {
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

        // Initial commit with a placeholder
        std::fs::write(path.join("init.txt"), "init\n").unwrap();
        Command::new("git")
            .args(["add", "init.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Second commit: add N files
        for i in 0..n {
            std::fs::write(path.join(format!("file{i}.txt")), format!("content {i}\n")).unwrap();
        }
        let mut add_args = vec!["add".to_string()];
        for i in 0..n {
            add_args.push(format!("file{i}.txt"));
        }
        Command::new("git")
            .args(&add_args)
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add files"])
            .current_dir(&path)
            .output()
            .unwrap();

        (dir, path)
    }

    #[test]
    fn it_returns_no_cursor_when_files_fit_in_page() {
        let (_dir, path) = create_repo_with_n_files(5);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

        assert!(
            manifest.pagination.next_cursor.is_none(),
            "should have no cursor when all files fit in page"
        );
        assert_eq!(manifest.pagination.total_files, 5);
        assert_eq!(manifest.files.len(), 5);
    }

    #[test]
    fn it_paginates_when_files_exceed_page_size() {
        let (_dir, path) = create_repo_with_n_files(5);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 3).unwrap();

        assert_eq!(
            manifest.files.len(),
            3,
            "should return only page_size files"
        );
        assert_eq!(manifest.pagination.total_files, 5);
        assert_eq!(manifest.pagination.page_start, 0);
        assert_eq!(manifest.pagination.page_size, 3);
        assert!(
            manifest.pagination.next_cursor.is_some(),
            "should have cursor when more files remain"
        );
    }

    #[test]
    fn it_counts_known_language_in_languages_affected() {
        // Kills: replace != with == at line 180 (language detection)
        let (_dir, path) = create_repo_with_go_file();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

        // main.go => "go" should be in languages_affected
        assert!(
            manifest
                .summary
                .languages_affected
                .contains(&"go".to_string()),
            "go should be in languages_affected, got: {:?}",
            manifest.summary.languages_affected
        );
        // README.md => "unknown" should NOT be in languages_affected
        assert!(
            !manifest
                .summary
                .languages_affected
                .contains(&"unknown".to_string()),
            "unknown should not be in languages_affected"
        );
    }

    #[test]
    fn it_skips_base_content_for_added_files_in_function_analysis() {
        // Kills: delete match arm ChangeType::Added at line 194
        // For an added file, base_content should be None => no base functions => all functions show as Added
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

        std::fs::write(path.join("init.txt"), "init\n").unwrap();
        Command::new("git")
            .args(["add", "init.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Add a new Rust file with a function
        std::fs::write(
            path.join("new.rs"),
            "fn brand_new() {\n    println!(\"new\");\n}\n",
        )
        .unwrap();
        Command::new("git")
            .args(["add", "new.rs"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add new rust file"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: true,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

        let rs_file = manifest.files.iter().find(|f| f.path == "new.rs").unwrap();
        assert_eq!(rs_file.change_type, ChangeType::Added);
        let fns = rs_file.functions_changed.as_ref().unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].name, "brand_new");
        assert_eq!(fns[0].change_type, FunctionChangeType::Added);
    }

    #[test]
    fn it_skips_head_content_for_deleted_files_in_function_analysis() {
        // Kills: delete match arm ChangeType::Deleted at line 199
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

        // Create a Rust file with a function
        std::fs::write(
            path.join("doomed.rs"),
            "fn doomed_fn() {\n    println!(\"bye\");\n}\n",
        )
        .unwrap();
        Command::new("git")
            .args(["add", "doomed.rs"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial with function"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Delete the file
        std::fs::remove_file(path.join("doomed.rs")).unwrap();
        Command::new("git")
            .args(["add", "doomed.rs"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "delete rust file"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: true,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

        let rs_file = manifest
            .files
            .iter()
            .find(|f| f.path == "doomed.rs")
            .unwrap();
        assert_eq!(rs_file.change_type, ChangeType::Deleted);
        let fns = rs_file.functions_changed.as_ref().unwrap();
        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].name, "doomed_fn");
        assert_eq!(fns[0].change_type, FunctionChangeType::Deleted);
    }

    #[test]
    fn it_accumulates_total_functions_changed_across_files() {
        // Kills: replace += with *= at line 236
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

        // Initial: two Rust files, each with one function
        std::fs::write(path.join("a.rs"), "fn alpha() {}\n").unwrap();
        std::fs::write(path.join("b.rs"), "fn beta() {}\n").unwrap();
        Command::new("git")
            .args(["add", "a.rs", "b.rs"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Modify both files to add one function each
        std::fs::write(path.join("a.rs"), "fn alpha() {}\nfn alpha2() {}\n").unwrap();
        std::fs::write(path.join("b.rs"), "fn beta() {}\nfn beta2() {}\n").unwrap();
        Command::new("git")
            .args(["add", "a.rs", "b.rs"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add functions"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: true,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

        // Each file has 1 new function => total = 1 + 1 = 2
        // If += were replaced by *=, the first file would set it to 0*1=0 then 0*1=0
        assert_eq!(
            manifest.summary.total_functions_changed,
            Some(2),
            "total_functions_changed should be sum, not product"
        );
    }

    #[test]
    fn it_uses_empty_base_content_for_added_dep_file() {
        // Kills: delete match arm ChangeType::Added at line 252 (dep analysis)
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

        std::fs::write(path.join("init.txt"), "init\n").unwrap();
        Command::new("git")
            .args(["add", "init.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Add a new Cargo.toml (dependency file)
        std::fs::write(
            path.join("Cargo.toml"),
            "[package]\nname = \"test\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1.0\"\n",
        )
        .unwrap();
        Command::new("git")
            .args(["add", "Cargo.toml"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add cargo.toml"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

        // Should have dependency changes showing serde was added
        assert!(
            !manifest.dependency_changes.is_empty(),
            "should detect dependency changes for added Cargo.toml"
        );
        let dep = &manifest.dependency_changes[0];
        assert!(
            !dep.added.is_empty(),
            "should show added dependencies for new Cargo.toml"
        );
    }

    #[test]
    fn it_uses_empty_head_content_for_deleted_dep_file() {
        // Kills: delete match arm ChangeType::Deleted at line 259 (dep analysis)
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

        // Create a Cargo.toml with a dependency
        std::fs::write(
            path.join("Cargo.toml"),
            "[package]\nname = \"test\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1.0\"\n",
        )
        .unwrap();
        Command::new("git")
            .args(["add", "Cargo.toml"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial with cargo"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Delete the Cargo.toml
        std::fs::remove_file(path.join("Cargo.toml")).unwrap();
        Command::new("git")
            .args(["add", "Cargo.toml"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "delete cargo.toml"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

        // Should have dependency changes showing serde was removed
        assert!(
            !manifest.dependency_changes.is_empty(),
            "should detect dependency changes for deleted Cargo.toml"
        );
        let dep = &manifest.dependency_changes[0];
        assert!(
            !dep.removed.is_empty(),
            "should show removed dependencies for deleted Cargo.toml"
        );
    }

    #[test]
    fn it_counts_summary_change_types_correctly() {
        // Kills: replace == with != at lines 296, 300, 304, 308
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

        // Create files for various operations
        std::fs::write(path.join("modify.txt"), "original\n").unwrap();
        std::fs::write(path.join("delete.txt"), "to be deleted\n").unwrap();
        Command::new("git")
            .args(["add", "modify.txt", "delete.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Modify one, delete one, add one
        std::fs::write(path.join("modify.txt"), "changed\n").unwrap();
        std::fs::remove_file(path.join("delete.txt")).unwrap();
        std::fs::write(path.join("added.txt"), "new file\n").unwrap();
        Command::new("git")
            .args(["add", "modify.txt", "delete.txt", "added.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "mixed changes"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

        assert_eq!(manifest.summary.total_files_changed, 3);
        assert_eq!(
            manifest.summary.files_added, 1,
            "should have exactly 1 added file"
        );
        assert_eq!(
            manifest.summary.files_modified, 1,
            "should have exactly 1 modified file"
        );
        assert_eq!(
            manifest.summary.files_deleted, 1,
            "should have exactly 1 deleted file"
        );
        assert_eq!(
            manifest.summary.files_renamed, 0,
            "should have exactly 0 renamed files"
        );
    }

    #[test]
    fn it_worktree_excludes_patterns_correctly() {
        // Kills: delete ! at lines 361 and 363 in build_worktree_manifest
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

        std::fs::write(path.join("keep.txt"), "keep\n").unwrap();
        std::fs::write(path.join("drop.log"), "drop\n").unwrap();
        Command::new("git")
            .args(["add", "keep.txt", "drop.log"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Stage changes to both files
        std::fs::write(path.join("keep.txt"), "keep changed\n").unwrap();
        std::fs::write(path.join("drop.log"), "drop changed\n").unwrap();
        Command::new("git")
            .args(["add", "keep.txt", "drop.log"])
            .current_dir(&path)
            .output()
            .unwrap();

        // With exclude: *.log should remove drop.log but keep keep.txt
        let options_exclude = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec!["*.log".to_string()],
            include_function_analysis: false,
        };
        let manifest = build_worktree_manifest(&path, "HEAD", &options_exclude, 0, 200).unwrap();

        assert!(
            manifest.files.iter().any(|f| f.path == "keep.txt"),
            "keep.txt should be included"
        );
        assert!(
            !manifest.files.iter().any(|f| f.path == "drop.log"),
            "drop.log should be excluded by pattern"
        );

        // With include: *.txt should include only keep.txt
        let options_include = ManifestOptions {
            include_patterns: vec!["*.txt".to_string()],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };
        let manifest = build_worktree_manifest(&path, "HEAD", &options_include, 0, 200).unwrap();

        assert!(
            manifest.files.iter().any(|f| f.path == "keep.txt"),
            "keep.txt should be included by pattern"
        );
        assert!(
            !manifest.files.iter().any(|f| f.path == "drop.log"),
            "drop.log should not match *.txt include pattern"
        );
    }

    /// Helper: create a git repo with N staged new files for worktree testing.
    fn create_worktree_repo_with_n_staged_files(
        n: usize,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
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

        // Initial commit
        std::fs::write(path.join("init.txt"), "init\n").unwrap();
        Command::new("git")
            .args(["add", "init.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Stage N new files
        let mut add_args = vec!["add".to_string()];
        for i in 0..n {
            let filename = format!("wt_file{i}.txt");
            std::fs::write(path.join(&filename), format!("content {i}\n")).unwrap();
            add_args.push(filename);
        }
        Command::new("git")
            .args(&add_args)
            .current_dir(&path)
            .output()
            .unwrap();

        (dir, path)
    }

    #[test]
    fn it_worktree_returns_no_cursor_when_files_fit_in_page() {
        let (_dir, path) = create_worktree_repo_with_n_staged_files(5);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };
        let manifest = build_worktree_manifest(&path, "HEAD", &options, 0, 200).unwrap();

        assert!(
            manifest.pagination.next_cursor.is_none(),
            "worktree should have no cursor when all files fit in page"
        );
        assert_eq!(manifest.pagination.total_files, 5);
        assert_eq!(manifest.files.len(), 5);
    }

    #[test]
    fn it_worktree_paginates_when_files_exceed_page_size() {
        let (_dir, path) = create_worktree_repo_with_n_staged_files(5);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };
        let manifest = build_worktree_manifest(&path, "HEAD", &options, 0, 3).unwrap();

        assert_eq!(
            manifest.files.len(),
            3,
            "should return only page_size files"
        );
        assert_eq!(manifest.pagination.total_files, 5);
        assert_eq!(manifest.pagination.page_start, 0);
        assert_eq!(manifest.pagination.page_size, 3);
        assert!(
            manifest.pagination.next_cursor.is_some(),
            "worktree should have cursor when more files remain"
        );
    }

    #[test]
    fn it_worktree_counts_known_language() {
        // Kills: replace != with == at line 395
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

        std::fs::write(path.join("init.txt"), "init\n").unwrap();
        Command::new("git")
            .args(["add", "init.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Stage a Python file
        std::fs::write(path.join("hello.py"), "print('hi')\n").unwrap();
        Command::new("git")
            .args(["add", "hello.py"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };
        let manifest = build_worktree_manifest(&path, "HEAD", &options, 0, 200).unwrap();

        assert!(
            manifest
                .summary
                .languages_affected
                .contains(&"python".to_string()),
            "worktree: python should be in languages_affected"
        );
        assert!(
            !manifest
                .summary
                .languages_affected
                .contains(&"unknown".to_string()),
            "worktree: unknown should NOT be in languages_affected"
        );
    }

    #[test]
    fn it_worktree_accumulates_total_functions_changed() {
        // Kills: replace += with *= at line 452
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

        // Initial commit with two Python files, each with one function
        std::fs::write(path.join("a.py"), "def func_a():\n    pass\n").unwrap();
        std::fs::write(path.join("b.py"), "def func_b():\n    pass\n").unwrap();
        Command::new("git")
            .args(["add", "a.py", "b.py"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Stage: add one function to each file
        std::fs::write(
            path.join("a.py"),
            "def func_a():\n    pass\n\ndef func_a2():\n    pass\n",
        )
        .unwrap();
        std::fs::write(
            path.join("b.py"),
            "def func_b():\n    pass\n\ndef func_b2():\n    pass\n",
        )
        .unwrap();
        Command::new("git")
            .args(["add", "a.py", "b.py"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: true,
        };
        let manifest = build_worktree_manifest(&path, "HEAD", &options, 0, 200).unwrap();

        // Each file adds 1 function => total = 2
        assert_eq!(
            manifest.summary.total_functions_changed,
            Some(2),
            "worktree total_functions_changed should be sum (2), not product"
        );
    }

    #[test]
    fn it_read_worktree_file_returns_file_content() {
        // Kills: replace read_worktree_file -> Option<String> with None/Some(String::new())/Some("xyzzy".into())
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().to_path_buf();

        let content = "def real_content():\n    return 42\n";
        std::fs::write(path.join("test.py"), content).unwrap();

        let result = read_worktree_file(&path, "test.py");
        assert!(result.is_some(), "should return Some for existing file");
        let file_content = result.unwrap();
        assert_eq!(
            file_content, content,
            "should return actual file content, not empty or dummy"
        );
        assert!(
            file_content.contains("real_content"),
            "should contain actual function name"
        );
        assert_ne!(file_content, "", "should not be empty");
        assert_ne!(file_content, "xyzzy", "should not be dummy value");
    }

    #[test]
    fn it_read_worktree_file_returns_none_for_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().to_path_buf();

        let result = read_worktree_file(&path, "nonexistent.py");
        assert!(result.is_none(), "should return None for missing file");
    }

    // --- Pagination tests ---

    #[test]
    fn it_summary_counts_reflect_all_files_on_every_page() {
        // Summary should always reflect total files, not just the page
        let (_dir, path) = create_repo_with_n_files(10);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };

        // First page
        let page1 = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 3).unwrap();
        assert_eq!(page1.summary.total_files_changed, 10);
        assert_eq!(page1.files.len(), 3);

        // Second page
        let page2 = build_manifest(&path, "HEAD~1", "HEAD", &options, 3, 3).unwrap();
        assert_eq!(page2.summary.total_files_changed, 10);
        assert_eq!(page2.files.len(), 3);
    }

    #[test]
    fn it_second_page_returns_different_files_than_first() {
        let (_dir, path) = create_repo_with_n_files(6);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };

        let page1 = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 3).unwrap();
        let page2 = build_manifest(&path, "HEAD~1", "HEAD", &options, 3, 3).unwrap();

        let page1_paths: Vec<&str> = page1.files.iter().map(|f| f.path.as_str()).collect();
        let page2_paths: Vec<&str> = page2.files.iter().map(|f| f.path.as_str()).collect();

        // No overlap
        for p in &page2_paths {
            assert!(
                !page1_paths.contains(p),
                "page2 file {:?} should not appear in page1",
                p
            );
        }
    }

    #[test]
    fn it_last_page_has_no_cursor() {
        let (_dir, path) = create_repo_with_n_files(5);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };

        // Request page starting at offset 3 with page_size 3 => covers files 3,4 (indices)
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 3, 3).unwrap();
        assert_eq!(
            manifest.files.len(),
            2,
            "last page should have remaining files"
        );
        assert!(
            manifest.pagination.next_cursor.is_none(),
            "last page should have no cursor"
        );
    }

    #[test]
    fn it_sets_total_functions_changed_to_none_when_paginating() {
        let (_dir, path) = create_repo_with_go_file();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: true,
        };

        // With a page_size of 1 and 2 total files, we are paginating
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 1).unwrap();
        assert!(
            manifest.summary.total_functions_changed.is_none(),
            "total_functions_changed should be None when paginating"
        );
    }

    #[test]
    fn it_total_functions_changed_present_when_not_paginating() {
        let (_dir, path) = create_repo_with_go_file();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: true,
        };

        // page_size 200 with 2 files => no pagination
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();
        assert!(
            manifest.summary.total_functions_changed.is_some(),
            "total_functions_changed should be present when not paginating"
        );
    }

    #[test]
    fn it_treesitter_only_runs_on_current_page() {
        // Create repo with 2 Go files so tree-sitter would normally analyze both
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

        std::fs::write(path.join("init.txt"), "init\n").unwrap();
        Command::new("git")
            .args(["add", "init.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        std::fs::write(path.join("a.go"), "package main\n\nfunc funcA() {}\n").unwrap();
        std::fs::write(path.join("b.go"), "package main\n\nfunc funcB() {}\n").unwrap();
        Command::new("git")
            .args(["add", "a.go", "b.go"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add go files"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = ManifestOptions {
            include_patterns: vec!["*.go".to_string()],
            exclude_patterns: vec![],
            include_function_analysis: true,
        };

        // Page 1: only first Go file
        let page1 = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 1).unwrap();
        assert_eq!(page1.files.len(), 1);
        // The file on page 1 should have function analysis
        assert!(page1.files[0].functions_changed.is_some());

        // Page 2: only second Go file
        let page2 = build_manifest(&path, "HEAD~1", "HEAD", &options, 1, 1).unwrap();
        assert_eq!(page2.files.len(), 1);
        assert!(page2.files[0].functions_changed.is_some());

        // The two pages should have different files
        assert_ne!(page1.files[0].path, page2.files[0].path);
    }

    #[test]
    fn it_worktree_summary_reflects_all_files_when_paginated() {
        let (_dir, path) = create_worktree_repo_with_n_staged_files(8);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };

        let page1 = build_worktree_manifest(&path, "HEAD", &options, 0, 3).unwrap();
        assert_eq!(page1.summary.total_files_changed, 8);
        assert_eq!(page1.files.len(), 3);
        assert!(page1.pagination.next_cursor.is_some());

        let page2 = build_worktree_manifest(&path, "HEAD", &options, 3, 3).unwrap();
        assert_eq!(page2.summary.total_files_changed, 8);
        assert_eq!(page2.files.len(), 3);
    }

    #[test]
    fn it_worktree_last_page_has_no_cursor() {
        let (_dir, path) = create_worktree_repo_with_n_staged_files(5);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };

        let manifest = build_worktree_manifest(&path, "HEAD", &options, 3, 3).unwrap();
        assert_eq!(manifest.files.len(), 2);
        assert!(
            manifest.pagination.next_cursor.is_none(),
            "worktree last page should have no cursor"
        );
    }

    #[test]
    fn it_cursor_encodes_correct_next_offset() {
        let (_dir, path) = create_repo_with_n_files(10);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };

        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 3).unwrap();
        let cursor_str = manifest.pagination.next_cursor.as_ref().unwrap();
        let cursor = crate::pagination::decode_cursor(cursor_str).unwrap();
        assert_eq!(cursor.offset, 3, "next cursor offset should be page_end");
        assert_eq!(cursor.v, 1);
        // SHAs should match the metadata
        assert_eq!(cursor.base_sha, manifest.metadata.base_sha);
        assert_eq!(cursor.head_sha, manifest.metadata.head_sha);
    }

    #[test]
    fn it_dependency_changes_always_complete_regardless_of_pagination() {
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

        std::fs::write(path.join("init.txt"), "init\n").unwrap();
        Command::new("git")
            .args(["add", "init.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Add multiple files including Cargo.toml
        std::fs::write(
            path.join("Cargo.toml"),
            "[package]\nname = \"test\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1.0\"\n",
        )
        .unwrap();
        std::fs::write(path.join("a.txt"), "a\n").unwrap();
        std::fs::write(path.join("b.txt"), "b\n").unwrap();
        Command::new("git")
            .args(["add", "Cargo.toml", "a.txt", "b.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add files"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
        };

        // Even with page_size=1, dependency changes should be present
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 1).unwrap();
        assert!(
            !manifest.dependency_changes.is_empty(),
            "dependency changes should always be complete regardless of pagination"
        );
    }
}
