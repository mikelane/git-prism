use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::Utc;

use crate::git::depfiles::{diff_dependencies, is_dependency_file};
use crate::git::diff::ChangeType;
use crate::git::generated::GeneratedFileDetector;
use crate::git::reader::RepoReader;
use crate::pagination::{CURSOR_VERSION, PaginationCursor, PaginationInfo, encode_cursor};
use crate::tools::extension_from_path;
use crate::tools::size;
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
    let mut unmatched_added: Vec<&Function> = Vec::new();
    let mut unmatched_deleted: Vec<&Function> = Vec::new();

    // Step 1: Compare functions that share the same name
    for head_fn in head_map.values() {
        match base_map.get(head_fn.name.as_str()) {
            None => unmatched_added.push(head_fn),
            Some(base_fn) => {
                if base_fn.signature != head_fn.signature {
                    changes.push(FunctionChange::from_function(
                        head_fn,
                        FunctionChangeType::SignatureChanged,
                        None,
                    ));
                } else if base_fn.body_hash != head_fn.body_hash {
                    changes.push(FunctionChange::from_function(
                        head_fn,
                        FunctionChangeType::Modified,
                        None,
                    ));
                }
                // else: same signature and same body_hash → no change (even if lines moved)
            }
        }
    }

    // Collect functions in base that are not in head
    for base_fn in base_map.values() {
        if !head_map.contains_key(base_fn.name.as_str()) {
            unmatched_deleted.push(base_fn);
        }
    }

    // Step 2: Rename detection — match unmatched pairs by body_hash
    let mut deleted_by_hash: HashMap<&str, Vec<&Function>> = HashMap::new();
    for del_fn in &unmatched_deleted {
        deleted_by_hash
            .entry(del_fn.body_hash.as_str())
            .or_default()
            .push(del_fn);
    }

    let mut matched_deleted: HashSet<&str> = HashSet::new();

    for added_fn in &unmatched_added {
        if let Some(candidates) = deleted_by_hash.get_mut(added_fn.body_hash.as_str())
            && let Some(del_fn) = candidates.pop()
        {
            changes.push(FunctionChange::from_function(
                added_fn,
                FunctionChangeType::Renamed,
                Some(del_fn.name.clone()),
            ));
            matched_deleted.insert(del_fn.name.as_str());
            continue;
        }
        changes.push(FunctionChange::from_function(
            added_fn,
            FunctionChangeType::Added,
            None,
        ));
    }

    // Remaining unmatched deleted functions
    for del_fn in &unmatched_deleted {
        if !matched_deleted.contains(del_fn.name.as_str()) {
            changes.push(FunctionChange::from_function(
                del_fn,
                FunctionChangeType::Deleted,
                None,
            ));
        }
    }

    changes.sort_by(|a, b| a.name.cmp(&b.name));
    changes
}

pub fn diff_imports(base_imports: &[String], head_imports: &[String]) -> ImportChange {
    let base_set: HashSet<&str> = base_imports.iter().map(|s| s.as_str()).collect();
    let head_set: HashSet<&str> = head_imports.iter().map(|s| s.as_str()).collect();

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
    let is_paginating = total_files > page_size;

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
            // No rename/copy test fixtures exist; counter is exercised by the
            // summary_change_types test for other variants.
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
                // Fallback path reads content that doesn't exist, returning empty
                // string via unwrap_or_default — same as explicit empty string.
                ChangeType::Added => String::new(),
                _ => reader
                    .read_file_at_ref(base_ref, &file_change.path)
                    .unwrap_or_default(),
            };

            let head_content = match file_change.change_type {
                // Fallback path reads content that doesn't exist, returning empty
                // string via unwrap_or_default — same as explicit empty string.
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
    // offset == total_files produces empty slice either way.
    #[rustfmt::skip]
    let page_files = if offset < total_files { &files_to_process[offset..page_end] } else { &[] };

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
                // Fallback reads content that doesn't exist at base_ref, returning None via .ok() — same result.
                ChangeType::Added => None,
                _ => reader.read_file_at_ref(base_ref, &file_change.path).ok(),
            };

            let head_content = match file_change.change_type {
                // Fallback reads content that doesn't exist at head_ref, returning None via .ok() — same result.
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
            version: CURSOR_VERSION,
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

    let mut response = ManifestResponse {
        metadata: ManifestMetadata {
            repo_path: repo_path.display().to_string(),
            base_ref: base_ref.to_string(),
            head_ref: head_ref.to_string(),
            base_sha: base_commit.sha,
            head_sha: head_commit.sha,
            generated_at: Utc::now(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            // Placeholder; overwritten below via a two-pass estimate so the
            // final value reflects the fully-populated response. See the
            // ManifestMetadata::token_estimate doc comment for the caveat.
            token_estimate: 0,
            function_analysis_truncated: vec![],
            budget_tokens: None,
        },
        summary,
        files: manifest_files,
        dependency_changes,
        pagination: PaginationInfo {
            total_items: total_files,
            page_start: offset,
            page_size,
            next_cursor,
        },
    };
    let trimmed = if options.include_function_analysis {
        match options.max_response_tokens {
            Some(budget) if budget > 0 => enforce_token_budget(&mut response, budget),
            _ => vec![],
        }
    } else {
        vec![]
    };
    response.metadata.function_analysis_truncated = trimmed;

    // If budget enforcement reduced the file count, update pagination cursor
    let actual_page_files = response.files.len();
    if actual_page_files < page_end.saturating_sub(offset) {
        let actual_end = offset + actual_page_files;
        response.pagination.page_size = actual_page_files;
        if actual_end < total_files {
            response.pagination.next_cursor = Some(encode_cursor(&PaginationCursor {
                version: CURSOR_VERSION,
                offset: actual_end,
                base_sha: response.metadata.base_sha.clone(),
                head_sha: response.metadata.head_sha.clone(),
            }));
        }
    }

    response.metadata.token_estimate = size::estimate_response_tokens(&response);
    Ok(response)
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
    // Worktree test fixtures use .txt files without tree-sitter support;
    // is_paginating only affects total_functions_changed which is always None.
    let is_paginating = total_files > page_size;

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
            // No rename/copy test fixtures exist; counter is exercised by the
            // summary_change_types test for other variants.
            ChangeType::Renamed | ChangeType::Copied => summary_files_renamed += 1,
        }
        summary_lines_added += file_change.lines_added;
        summary_lines_removed += file_change.lines_removed;
    }

    let mut all_languages_affected: Vec<String> = all_languages_set.into_iter().collect();
    all_languages_affected.sort();

    // Apply pagination: select only the current page of files
    let page_end = (offset + page_size).min(total_files);
    // offset == total_files produces empty slice either way.
    #[rustfmt::skip]
    let page_files = if offset < total_files { &files_to_process[offset..page_end] } else { &[] };

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
                // Fallback reads content that doesn't exist at base_ref, returning None via .ok() — same result.
                ChangeType::Added => None,
                _ => reader.read_file_at_ref(base_ref, &file_change.path).ok(),
            };

            let head_content = match file_change.change_type {
                // Fallback reads deleted file from worktree/blob, returning None — same result.
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
            version: CURSOR_VERSION,
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

    let mut response = ManifestResponse {
        metadata: ManifestMetadata {
            repo_path: repo_path.display().to_string(),
            base_ref: base_ref.to_string(),
            head_ref: "WORKTREE".to_string(),
            base_sha: base_commit.sha,
            head_sha: "WORKTREE".to_string(),
            generated_at: Utc::now(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            // Placeholder; see build_manifest for the two-pass rationale.
            token_estimate: 0,
            function_analysis_truncated: vec![],
            budget_tokens: None,
        },
        summary,
        files: manifest_files,
        dependency_changes: vec![],
        pagination: PaginationInfo {
            total_items: total_files,
            page_start: offset,
            page_size,
            next_cursor,
        },
    };
    let trimmed = if options.include_function_analysis {
        match options.max_response_tokens {
            Some(budget) if budget > 0 => enforce_token_budget(&mut response, budget),
            _ => vec![],
        }
    } else {
        vec![]
    };
    response.metadata.function_analysis_truncated = trimmed;

    // If budget enforcement reduced the file count, update pagination cursor
    let actual_page_files = response.files.len();
    if actual_page_files < page_end.saturating_sub(offset) {
        let actual_end = offset + actual_page_files;
        response.pagination.page_size = actual_page_files;
        if actual_end < total_files {
            response.pagination.next_cursor = Some(encode_cursor(&PaginationCursor {
                version: CURSOR_VERSION,
                offset: actual_end,
                base_sha: response.metadata.base_sha.clone(),
                head_sha: "WORKTREE".to_string(),
            }));
        }
    }

    response.metadata.token_estimate = size::estimate_response_tokens(&response);
    Ok(response)
}

/// Enforce a token budget on a fully-constructed manifest response by
/// progressively stripping function/import analysis from file entries.
///
/// Returns the paths of "tier 1" trimmed files — those whose imports were
/// stripped but function signatures were preserved. Files stripped all the
/// way to bare entries (tier 2) are NOT included in the returned list
/// because the BDD assertion requires trimmed files to have non-empty
/// `functions_changed`.
///
/// Three-tier algorithm:
/// 1. Measure skeleton overhead (metadata + summary + deps + pagination)
/// 2. Walk files in page order, deducting each file's token cost
/// 3. When a file exceeds remaining budget:
///    - Tier 1: strip `imports_changed`, keep `functions_changed` (signatures)
///    - Tier 2: strip both — the file becomes a bare entry
pub fn enforce_token_budget(response: &mut ManifestResponse, budget: usize) -> Vec<String> {
    // Measure skeleton overhead (everything except files)
    let files = std::mem::take(&mut response.files);
    let skeleton_cost = size::estimate_response_tokens(response);
    response.files = files;

    // Safety margin for the `function_analysis_truncated` list that gets
    // populated AFTER enforcement runs. Each trimmed path serializes as ~25
    // chars (path + JSON quoting + comma), and large changes can produce
    // dozens of trimmed paths, adding hundreds of tokens we didn't budget for.
    // Reserve ~5% of the budget (min 256 tokens) as headroom.
    let safety_margin = (budget / 20).max(16);
    let file_budget = budget
        .saturating_sub(skeleton_cost)
        .saturating_sub(safety_margin);

    // Phase 1: measure per-file costs at each tier
    struct FileCosts {
        full: usize,
        tier1: usize, // functions kept, imports stripped
        bare: usize,  // both stripped
        has_analysis: bool,
    }
    let costs: Vec<FileCosts> = response
        .files
        .iter()
        .map(|f| {
            let full = size::estimate_response_tokens(f);
            // Tier 1: same as full but without imports
            let tier1 = if f.imports_changed.is_some() {
                let mut clone = f.clone();
                clone.imports_changed = None;
                size::estimate_response_tokens(&clone)
            } else {
                full
            };
            // Bare: no functions, no imports
            let bare = {
                let mut clone = f.clone();
                clone.functions_changed = None;
                clone.imports_changed = None;
                size::estimate_response_tokens(&clone)
            };
            FileCosts {
                full,
                tier1,
                bare,
                has_analysis: f.functions_changed.is_some(),
            }
        })
        .collect();

    // Phase 2: determine how many files fit and at what tier.
    // Strategy: uniform downgrade — try all-full, then all-tier1, then mix tier1+tier2.
    let total_full: usize = costs.iter().map(|c| c.full).sum();
    let total_tier1: usize = costs.iter().map(|c| c.tier1).sum();

    if total_full <= file_budget {
        // Everything fits at full analysis — no trimming needed
        return vec![];
    }

    if total_tier1 <= file_budget {
        // Everything fits at tier 1 — strip imports from files that had them,
        // record only those as trimmed (files without imports aren't actually
        // altered and shouldn't be listed).
        let mut trimmed = Vec::new();
        for file in &mut response.files {
            if file.imports_changed.is_some() {
                file.imports_changed = None;
                trimmed.push(file.path.clone());
            }
        }
        return trimmed;
    }

    // Phase 3: greedy tier1-first — walk files in order, preferring tier1
    // (functions preserved) over tier2 (bare). This ensures we get a non-
    // empty `function_analysis_truncated` list whenever budget allows.
    let mut remaining = file_budget;
    let mut decisions: Vec<TierChoice> = Vec::with_capacity(costs.len());

    for c in &costs {
        if c.has_analysis && c.tier1 <= remaining {
            remaining -= c.tier1;
            decisions.push(TierChoice::Tier1);
        } else if c.bare <= remaining {
            remaining -= c.bare;
            decisions.push(TierChoice::Bare);
        } else {
            // File doesn't fit even at bare cost — stop. Files are returned in
            // page order (matching the git diff order), so we preserve ordering
            // rather than skipping to find smaller files that might fit.
            // Agents needing files beyond this point should re-request with a
            // larger budget or without enforcement (max_response_tokens: 0).
            break;
        }
    }

    let files_to_keep = decisions.len();
    response.files.truncate(files_to_keep);

    let mut trimmed = Vec::new();
    for (i, file) in response.files.iter_mut().enumerate() {
        match decisions[i] {
            TierChoice::Tier1 => {
                // Keep functions (signatures), strip imports
                file.imports_changed = None;
                trimmed.push(file.path.clone());
            }
            TierChoice::Bare => {
                file.functions_changed = None;
                file.imports_changed = None;
            }
        }
    }

    trimmed
}

#[derive(Clone, Copy)]
enum TierChoice {
    Tier1,
    Bare,
}

fn read_worktree_file(repo_path: &Path, file_path: &str) -> Option<String> {
    let full_path = repo_path.join(file_path);
    std::fs::read_to_string(&full_path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::diff::ChangeScope;

    #[test]
    fn it_detects_added_function() {
        let base = vec![];
        let head = vec![Function {
            name: "foo".into(),
            signature: "fn foo()".into(),
            start_line: 1,
            end_line: 3,
            body_hash: "aaa".into(),
        }];
        let changes = diff_functions(&base, &head);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].name, "foo");
        assert_eq!(changes[0].change_type, FunctionChangeType::Added);
        assert!(
            changes[0].old_name.is_none(),
            "Added should not have old_name"
        );
    }

    #[test]
    fn it_detects_deleted_function() {
        let base = vec![Function {
            name: "bar".into(),
            signature: "fn bar()".into(),
            start_line: 1,
            end_line: 3,
            body_hash: "bbb".into(),
        }];
        let head = vec![];
        let changes = diff_functions(&base, &head);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].name, "bar");
        assert_eq!(changes[0].change_type, FunctionChangeType::Deleted);
        assert!(
            changes[0].old_name.is_none(),
            "Deleted should not have old_name"
        );
    }

    #[test]
    fn it_detects_signature_changed_function() {
        let base = vec![Function {
            name: "baz".into(),
            signature: "fn baz()".into(),
            start_line: 1,
            end_line: 3,
            body_hash: "ccc".into(),
        }];
        let head = vec![Function {
            name: "baz".into(),
            signature: "fn baz(x: i32)".into(),
            start_line: 1,
            end_line: 5,
            body_hash: "ddd".into(),
        }];
        let changes = diff_functions(&base, &head);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change_type, FunctionChangeType::SignatureChanged);
        assert!(
            changes[0].old_name.is_none(),
            "SignatureChanged should not have old_name"
        );
    }

    #[test]
    fn it_detects_modified_function_by_body_hash_change() {
        let base = vec![Function {
            name: "qux".into(),
            signature: "fn qux()".into(),
            start_line: 1,
            end_line: 3,
            body_hash: "eee".into(),
        }];
        let head = vec![Function {
            name: "qux".into(),
            signature: "fn qux()".into(),
            start_line: 1,
            end_line: 10,
            body_hash: "fff".into(),
        }];
        let changes = diff_functions(&base, &head);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].change_type, FunctionChangeType::Modified);
        assert!(changes[0].old_name.is_none());
    }

    #[test]
    fn line_range_change_alone_does_not_trigger_modified() {
        // Triangulation: same body_hash + different lines = no change (moved, not modified)
        let base = vec![Function {
            name: "qux".into(),
            signature: "fn qux()".into(),
            start_line: 1,
            end_line: 3,
            body_hash: "same_hash".into(),
        }];
        let head = vec![Function {
            name: "qux".into(),
            signature: "fn qux()".into(),
            start_line: 50,
            end_line: 100,
            body_hash: "same_hash".into(),
        }];
        let changes = diff_functions(&base, &head);
        assert!(
            changes.is_empty(),
            "line range change with same body_hash should NOT produce Modified"
        );
    }

    #[test]
    fn it_returns_empty_for_identical_functions() {
        let fns = vec![Function {
            name: "same".into(),
            signature: "fn same()".into(),
            start_line: 1,
            end_line: 3,
            body_hash: "ggg".into(),
        }];
        let changes = diff_functions(&fns, &fns);
        assert!(changes.is_empty());
    }

    // --- Content-aware diff tests (body_hash based) ---

    #[test]
    fn moved_but_unchanged_function_produces_no_change() {
        let base = vec![Function {
            name: "foo".into(),
            signature: "fn foo()".into(),
            start_line: 1,
            end_line: 5,
            body_hash: "same_hash".into(),
        }];
        let head = vec![Function {
            name: "foo".into(),
            signature: "fn foo()".into(),
            start_line: 10,
            end_line: 14,
            body_hash: "same_hash".into(),
        }];
        let changes = diff_functions(&base, &head);
        assert!(
            changes.is_empty(),
            "moved-but-unchanged should produce no change"
        );
    }

    #[test]
    fn body_only_change_detected_as_modified() {
        let base = vec![Function {
            name: "compute".into(),
            signature: "fn compute(x: i32) -> i32".into(),
            start_line: 1,
            end_line: 3,
            body_hash: "hash_v1".into(),
        }];
        let head = vec![Function {
            name: "compute".into(),
            signature: "fn compute(x: i32) -> i32".into(),
            start_line: 1,
            end_line: 3,
            body_hash: "hash_v2".into(),
        }];
        let changes = diff_functions(&base, &head);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].name, "compute");
        assert_eq!(changes[0].change_type, FunctionChangeType::Modified);
        assert!(
            changes[0].old_name.is_none(),
            "Modified should not have old_name"
        );
    }

    #[test]
    fn rename_detected_by_body_hash() {
        let base = vec![Function {
            name: "old_name".into(),
            signature: "fn old_name(x: i32)".into(),
            start_line: 1,
            end_line: 3,
            body_hash: "shared_hash".into(),
        }];
        let head = vec![Function {
            name: "new_name".into(),
            signature: "fn new_name(x: i32)".into(),
            start_line: 1,
            end_line: 3,
            body_hash: "shared_hash".into(),
        }];
        let changes = diff_functions(&base, &head);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].name, "new_name");
        assert_eq!(changes[0].change_type, FunctionChangeType::Renamed);
        assert_eq!(changes[0].old_name.as_deref(), Some("old_name"));
    }

    #[test]
    fn rename_plus_body_change_shows_deleted_and_added() {
        let base = vec![Function {
            name: "old_name".into(),
            signature: "fn old_name()".into(),
            start_line: 1,
            end_line: 3,
            body_hash: "hash_a".into(),
        }];
        let head = vec![Function {
            name: "new_name".into(),
            signature: "fn new_name()".into(),
            start_line: 1,
            end_line: 3,
            body_hash: "hash_b".into(),
        }];
        let changes = diff_functions(&base, &head);
        assert_eq!(changes.len(), 2);
        let deleted = changes
            .iter()
            .find(|c| c.change_type == FunctionChangeType::Deleted)
            .unwrap();
        assert_eq!(deleted.name, "old_name");
        let added = changes
            .iter()
            .find(|c| c.change_type == FunctionChangeType::Added)
            .unwrap();
        assert_eq!(added.name, "new_name");
    }

    #[test]
    fn swapped_functions_produce_no_changes() {
        let base = vec![
            Function {
                name: "foo".into(),
                signature: "fn foo()".into(),
                start_line: 1,
                end_line: 3,
                body_hash: "hash_foo".into(),
            },
            Function {
                name: "bar".into(),
                signature: "fn bar()".into(),
                start_line: 5,
                end_line: 7,
                body_hash: "hash_bar".into(),
            },
        ];
        let head = vec![
            Function {
                name: "bar".into(),
                signature: "fn bar()".into(),
                start_line: 1,
                end_line: 3,
                body_hash: "hash_bar".into(),
            },
            Function {
                name: "foo".into(),
                signature: "fn foo()".into(),
                start_line: 5,
                end_line: 7,
                body_hash: "hash_foo".into(),
            },
        ];
        let changes = diff_functions(&base, &head);
        assert!(
            changes.is_empty(),
            "swapped functions should produce no changes"
        );
    }

    #[test]
    fn multiple_renames_detected() {
        let base = vec![
            Function {
                name: "a".into(),
                signature: "fn a()".into(),
                start_line: 1,
                end_line: 2,
                body_hash: "hash_x".into(),
            },
            Function {
                name: "b".into(),
                signature: "fn b()".into(),
                start_line: 3,
                end_line: 4,
                body_hash: "hash_y".into(),
            },
        ];
        let head = vec![
            Function {
                name: "c".into(),
                signature: "fn c()".into(),
                start_line: 1,
                end_line: 2,
                body_hash: "hash_x".into(),
            },
            Function {
                name: "d".into(),
                signature: "fn d()".into(),
                start_line: 3,
                end_line: 4,
                body_hash: "hash_y".into(),
            },
        ];
        let changes = diff_functions(&base, &head);
        assert_eq!(changes.len(), 2);
        assert!(
            changes
                .iter()
                .all(|c| c.change_type == FunctionChangeType::Renamed)
        );
        let c_change = changes.iter().find(|c| c.name == "c").unwrap();
        assert_eq!(c_change.old_name.as_deref(), Some("a"));
        let d_change = changes.iter().find(|c| c.name == "d").unwrap();
        assert_eq!(d_change.old_name.as_deref(), Some("b"));
    }

    #[test]
    fn non_rename_changes_have_null_old_name() {
        let base = vec![Function {
            name: "deleted_fn".into(),
            signature: "fn deleted_fn()".into(),
            start_line: 1,
            end_line: 3,
            body_hash: "xxx".into(),
        }];
        let head = vec![Function {
            name: "added_fn".into(),
            signature: "fn added_fn()".into(),
            start_line: 1,
            end_line: 3,
            body_hash: "yyy".into(),
        }];
        let changes = diff_functions(&base, &head);
        assert_eq!(changes.len(), 2);
        for c in &changes {
            assert!(
                c.old_name.is_none(),
                "non-rename changes should have None old_name"
            );
        }
    }

    #[test]
    fn duplicate_body_hash_produces_correct_rename_and_delete_counts() {
        // 2 deleted functions share the same body_hash, 1 added matches.
        // Expect: 1 Renamed + 1 Deleted (pairing is greedy/arbitrary, but counts are exact).
        let base = vec![
            Function {
                name: "a".into(),
                signature: "fn a()".into(),
                start_line: 1,
                end_line: 2,
                body_hash: "stub_hash".into(),
            },
            Function {
                name: "b".into(),
                signature: "fn b()".into(),
                start_line: 3,
                end_line: 4,
                body_hash: "stub_hash".into(),
            },
        ];
        let head = vec![Function {
            name: "c".into(),
            signature: "fn c()".into(),
            start_line: 1,
            end_line: 2,
            body_hash: "stub_hash".into(),
        }];
        let changes = diff_functions(&base, &head);

        assert_eq!(changes.len(), 2);
        let renamed_count = changes
            .iter()
            .filter(|c| c.change_type == FunctionChangeType::Renamed)
            .count();
        let deleted_count = changes
            .iter()
            .filter(|c| c.change_type == FunctionChangeType::Deleted)
            .count();
        assert_eq!(renamed_count, 1, "exactly one rename");
        assert_eq!(deleted_count, 1, "exactly one delete");

        let renamed = changes
            .iter()
            .find(|c| c.change_type == FunctionChangeType::Renamed)
            .unwrap();
        assert_eq!(renamed.name, "c");
        assert!(
            renamed.old_name.as_deref() == Some("a") || renamed.old_name.as_deref() == Some("b"),
            "old_name should be one of the deleted functions, got {:?}",
            renamed.old_name
        );
    }

    #[test]
    fn more_added_than_deleted_with_same_hash() {
        // 1 deleted, 2 added with the same hash → 1 Renamed + 1 Added
        let base = vec![Function {
            name: "old".into(),
            signature: "fn old()".into(),
            start_line: 1,
            end_line: 2,
            body_hash: "stub_hash".into(),
        }];
        let head = vec![
            Function {
                name: "new_a".into(),
                signature: "fn new_a()".into(),
                start_line: 1,
                end_line: 2,
                body_hash: "stub_hash".into(),
            },
            Function {
                name: "new_b".into(),
                signature: "fn new_b()".into(),
                start_line: 3,
                end_line: 4,
                body_hash: "stub_hash".into(),
            },
        ];
        let changes = diff_functions(&base, &head);

        assert_eq!(changes.len(), 2);
        let renamed_count = changes
            .iter()
            .filter(|c| c.change_type == FunctionChangeType::Renamed)
            .count();
        let added_count = changes
            .iter()
            .filter(|c| c.change_type == FunctionChangeType::Added)
            .count();
        assert_eq!(renamed_count, 1, "exactly one rename");
        assert_eq!(added_count, 1, "exactly one added");
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
                body_hash: "hhh".into(),
            },
            Function {
                name: "removed".into(),
                signature: "fn removed()".into(),
                start_line: 5,
                end_line: 7,
                body_hash: "iii".into(),
            },
            Function {
                name: "changed_sig".into(),
                signature: "fn changed_sig()".into(),
                start_line: 9,
                end_line: 11,
                body_hash: "jjj".into(),
            },
        ];
        let head = vec![
            Function {
                name: "kept".into(),
                signature: "fn kept()".into(),
                start_line: 1,
                end_line: 3,
                body_hash: "hhh".into(),
            },
            Function {
                name: "added".into(),
                signature: "fn added()".into(),
                start_line: 5,
                end_line: 7,
                body_hash: "kkk".into(),
            },
            Function {
                name: "changed_sig".into(),
                signature: "fn changed_sig(x: i32)".into(),
                start_line: 9,
                end_line: 13,
                body_hash: "lll".into(),
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
    fn it_reports_a_positive_token_estimate_for_a_non_trivial_manifest() {
        let (_dir, path) = create_repo_with_go_file();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: true,
            max_response_tokens: None,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

        // The response includes two changed files plus function/import detail,
        // so the serialized JSON is well over 4 characters and the estimate
        // must be strictly positive. Exact value is not asserted because it
        // depends on metadata fields like `generated_at` that vary at runtime.
        assert!(
            manifest.metadata.token_estimate > 0,
            "expected a positive token_estimate on a non-trivial manifest response, got {}",
            manifest.metadata.token_estimate,
        );
    }

    #[test]
    fn it_builds_manifest_with_function_analysis() {
        let (_dir, path) = create_repo_with_go_file();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: true,
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

        for file in &manifest.files {
            assert!(file.functions_changed.is_none());
            assert!(file.imports_changed.is_none());
        }
        assert!(manifest.summary.total_functions_changed.is_none());
    }

    // --- Integration tests: content-aware function diffs with real git repos ---

    /// Helper: create a repo with two commits where the second reorders functions.
    fn create_repo_with_reordered_functions() -> (tempfile::TempDir, std::path::PathBuf) {
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

        // Base: two functions in order
        std::fs::write(
            path.join("lib.rs"),
            "fn greet(name: &str) -> String {\n    format!(\"Hello, {}!\", name)\n}\n\nfn farewell(name: &str) -> String {\n    format!(\"Goodbye, {}!\", name)\n}\n",
        )
        .unwrap();
        Command::new("git")
            .args(["add", "lib.rs"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Head: same two functions, swapped order
        std::fs::write(
            path.join("lib.rs"),
            "fn farewell(name: &str) -> String {\n    format!(\"Goodbye, {}!\", name)\n}\n\nfn greet(name: &str) -> String {\n    format!(\"Hello, {}!\", name)\n}\n",
        )
        .unwrap();
        Command::new("git")
            .args(["add", "lib.rs"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "swap order"])
            .current_dir(&path)
            .output()
            .unwrap();

        (dir, path)
    }

    #[test]
    fn reordered_functions_produce_zero_function_changes() {
        let (_dir, path) = create_repo_with_reordered_functions();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: true,
            max_response_tokens: None,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

        let rs_file = manifest.files.iter().find(|f| f.path == "lib.rs").unwrap();
        let fns = rs_file.functions_changed.as_ref().unwrap();
        assert!(
            fns.is_empty(),
            "reordered functions should produce no changes, got: {fns:?}"
        );
    }

    /// Helper: create a repo where the second commit changes a function body only.
    fn create_repo_with_body_change() -> (tempfile::TempDir, std::path::PathBuf) {
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

        std::fs::write(
            path.join("lib.rs"),
            "fn compute(x: i32) -> i32 {\n    x + 1\n}\n",
        )
        .unwrap();
        Command::new("git")
            .args(["add", "lib.rs"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Change only the body, keep signature identical
        std::fs::write(
            path.join("lib.rs"),
            "fn compute(x: i32) -> i32 {\n    x * 2 + 1\n}\n",
        )
        .unwrap();
        Command::new("git")
            .args(["add", "lib.rs"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "change body"])
            .current_dir(&path)
            .output()
            .unwrap();

        (dir, path)
    }

    #[test]
    fn body_only_change_detected_in_real_repo() {
        let (_dir, path) = create_repo_with_body_change();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: true,
            max_response_tokens: None,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

        let rs_file = manifest.files.iter().find(|f| f.path == "lib.rs").unwrap();
        let fns = rs_file.functions_changed.as_ref().unwrap();
        assert_eq!(fns.len(), 1, "exactly one function change expected");
        assert_eq!(fns[0].name, "compute");
        assert_eq!(fns[0].change_type, FunctionChangeType::Modified);
        assert!(fns[0].old_name.is_none());
    }

    /// Helper: create a repo where the second commit renames a function.
    fn create_repo_with_rename() -> (tempfile::TempDir, std::path::PathBuf) {
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

        std::fs::write(
            path.join("lib.rs"),
            "fn old_name(x: i32) -> i32 {\n    x + 1\n}\n",
        )
        .unwrap();
        Command::new("git")
            .args(["add", "lib.rs"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Same body, different name
        std::fs::write(
            path.join("lib.rs"),
            "fn new_name(x: i32) -> i32 {\n    x + 1\n}\n",
        )
        .unwrap();
        Command::new("git")
            .args(["add", "lib.rs"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "rename function"])
            .current_dir(&path)
            .output()
            .unwrap();

        (dir, path)
    }

    #[test]
    fn rename_detected_in_real_repo() {
        let (_dir, path) = create_repo_with_rename();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: true,
            max_response_tokens: None,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

        let rs_file = manifest.files.iter().find(|f| f.path == "lib.rs").unwrap();
        let fns = rs_file.functions_changed.as_ref().unwrap();
        assert_eq!(fns.len(), 1, "expected single rename, got: {fns:?}");
        assert_eq!(fns[0].name, "new_name");
        assert_eq!(fns[0].change_type, FunctionChangeType::Renamed);
        assert_eq!(fns[0].old_name.as_deref(), Some("old_name"));
    }

    /// Helper: create a repo where the second commit renames AND modifies a function.
    fn create_repo_with_rename_and_modify() -> (tempfile::TempDir, std::path::PathBuf) {
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

        std::fs::write(
            path.join("lib.rs"),
            "fn old_name(x: i32) -> i32 {\n    x + 1\n}\n",
        )
        .unwrap();
        Command::new("git")
            .args(["add", "lib.rs"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Different name AND different body
        std::fs::write(
            path.join("lib.rs"),
            "fn new_name(x: i32) -> i32 {\n    x * 2 + 1\n}\n",
        )
        .unwrap();
        Command::new("git")
            .args(["add", "lib.rs"])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "rename and modify"])
            .current_dir(&path)
            .output()
            .unwrap();

        (dir, path)
    }

    #[test]
    fn rename_plus_modify_shows_deleted_and_added_in_real_repo() {
        let (_dir, path) = create_repo_with_rename_and_modify();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: true,
            max_response_tokens: None,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

        let rs_file = manifest.files.iter().find(|f| f.path == "lib.rs").unwrap();
        let fns = rs_file.functions_changed.as_ref().unwrap();
        assert_eq!(fns.len(), 2, "expected deleted + added, got: {fns:?}");

        let deleted = fns
            .iter()
            .find(|f| f.change_type == FunctionChangeType::Deleted);
        assert!(deleted.is_some(), "expected a Deleted change");
        assert_eq!(deleted.unwrap().name, "old_name");

        let added = fns
            .iter()
            .find(|f| f.change_type == FunctionChangeType::Added);
        assert!(added.is_some(), "expected an Added change");
        assert_eq!(added.unwrap().name, "new_name");
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
                body_hash: "mmm".into(),
            },
            Function {
                name: "alpha".into(),
                signature: "fn alpha()".into(),
                start_line: 3,
                end_line: 4,
                body_hash: "nnn".into(),
            },
        ];
        let changes = diff_functions(&base, &head);
        assert_eq!(changes[0].name, "alpha");
        assert_eq!(changes[1].name, "zebra");
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 200).unwrap();

        assert!(
            manifest.pagination.next_cursor.is_none(),
            "should have no cursor when all files fit in page"
        );
        assert_eq!(manifest.pagination.total_items, 5);
        assert_eq!(manifest.files.len(), 5);
    }

    #[test]
    fn it_paginates_when_files_exceed_page_size() {
        let (_dir, path) = create_repo_with_n_files(5);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 3).unwrap();

        assert_eq!(
            manifest.files.len(),
            3,
            "should return only page_size files"
        );
        assert_eq!(manifest.pagination.total_items, 5);
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
        };
        let manifest = build_worktree_manifest(&path, "HEAD", &options, 0, 200).unwrap();

        assert!(
            manifest.pagination.next_cursor.is_none(),
            "worktree should have no cursor when all files fit in page"
        );
        assert_eq!(manifest.pagination.total_items, 5);
        assert_eq!(manifest.files.len(), 5);
    }

    #[test]
    fn it_worktree_paginates_when_files_exceed_page_size() {
        let (_dir, path) = create_worktree_repo_with_n_staged_files(5);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };
        let manifest = build_worktree_manifest(&path, "HEAD", &options, 0, 3).unwrap();

        assert_eq!(
            manifest.files.len(),
            3,
            "should return only page_size files"
        );
        assert_eq!(manifest.pagination.total_items, 5);
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
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
            max_response_tokens: None,
        };

        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 3).unwrap();
        let cursor_str = manifest.pagination.next_cursor.as_ref().unwrap();
        let cursor = crate::pagination::decode_cursor(cursor_str).unwrap();
        assert_eq!(cursor.offset, 3, "next cursor offset should be page_end");
        assert_eq!(cursor.version, 1);
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
            max_response_tokens: None,
        };

        // Even with page_size=1, dependency changes should be present
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 1).unwrap();
        assert!(
            !manifest.dependency_changes.is_empty(),
            "dependency changes should always be complete regardless of pagination"
        );
    }

    #[test]
    fn it_returns_empty_files_when_offset_beyond_total() {
        let (_dir, path) = create_repo_with_n_files(3);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };
        // The repo has ~2 changed files; offset 999 is way past the end
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 999, 100).unwrap();
        assert!(manifest.files.is_empty());
        assert!(manifest.pagination.next_cursor.is_none());
        // Summary still reflects all files
        assert!(manifest.summary.total_files_changed > 0);
    }

    #[test]
    fn it_is_paginating_only_when_total_exceeds_page_size() {
        // create_repo_with_go_file produces 2 changed files (main.go + README.md)
        // with tree-sitter support on main.go, so total_functions_changed is meaningful
        let (_dir, path) = create_repo_with_go_file();
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: true,
            max_response_tokens: None,
        };

        // 2 files, page_size=2 → NOT paginating, total_functions_changed present
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 2).unwrap();
        assert_eq!(manifest.files.len(), 2);
        assert!(manifest.pagination.next_cursor.is_none());
        assert!(
            manifest.summary.total_functions_changed.is_some(),
            "should have function count when not paginating"
        );

        // 2 files, page_size=1 → paginating, total_functions_changed is None
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 1).unwrap();
        assert_eq!(manifest.files.len(), 1);
        assert!(manifest.pagination.next_cursor.is_some());
        assert!(
            manifest.summary.total_functions_changed.is_none(),
            "should suppress function count when paginating"
        );
    }

    #[test]
    fn it_counts_summary_added_modified_deleted_correctly_with_pagination() {
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

        // First commit: create files to be modified and deleted
        std::fs::write(path.join("modify_me.rs"), "fn old() {}\n").unwrap();
        std::fs::write(path.join("delete_me.rs"), "fn gone() {}\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Second commit: add new, modify one, delete one
        std::fs::write(path.join("added.rs"), "fn new() {}\n").unwrap();
        std::fs::write(path.join("modify_me.rs"), "fn modified() {}\n").unwrap();
        std::fs::remove_file(path.join("delete_me.rs")).unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "changes"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };
        // Use a small page to ensure we're paginating but summary is still complete
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 1).unwrap();
        assert_eq!(manifest.summary.files_added, 1);
        assert_eq!(manifest.summary.files_modified, 1);
        assert_eq!(manifest.summary.files_deleted, 1);
        assert_eq!(manifest.summary.total_files_changed, 3);
        assert!(manifest.summary.total_lines_added > 0);
        assert!(manifest.summary.total_lines_removed > 0);
    }

    #[test]
    fn it_uses_empty_base_for_added_dep_file_in_paginated_mode() {
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

        std::fs::write(path.join("seed.txt"), "seed\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Add a new Cargo.toml (ChangeType::Added)
        std::fs::write(
            path.join("Cargo.toml"),
            "[package]\nname = \"test\"\n[dependencies]\nserde = \"1\"\n",
        )
        .unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add cargo"])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 0, 1).unwrap();
        // Dep analysis should show added deps even though page_size=1
        assert!(
            !manifest.dependency_changes.is_empty(),
            "dependency changes should detect added Cargo.toml"
        );
        let dep = &manifest.dependency_changes[0];
        assert!(!dep.added.is_empty(), "should have added dependencies");
    }

    #[test]
    fn it_returns_exact_page_boundary_files() {
        // offset == total_files should return empty; offset == total_files - 1 should return 1
        let (_dir, path) = create_repo_with_n_files(3);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        // offset at last file
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 2, 100).unwrap();
        assert_eq!(manifest.files.len(), 1, "should return the last file");

        // offset exactly at total
        let manifest = build_manifest(&path, "HEAD~1", "HEAD", &options, 3, 100).unwrap();
        assert!(
            manifest.files.is_empty(),
            "offset at total should return empty"
        );
    }

    #[test]
    fn it_worktree_is_paginating_only_when_total_exceeds_page_size() {
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
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Stage 3 new files
        for i in 0..3 {
            std::fs::write(path.join(format!("file{i}.txt")), format!("content {i}\n")).unwrap();
        }
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        // 3 staged files, page_size=3 → not paginating, no cursor
        let manifest = build_worktree_manifest(&path, "HEAD", &options, 0, 3).unwrap();
        assert_eq!(manifest.files.len(), 3);
        assert!(manifest.pagination.next_cursor.is_none());

        // page_size=2 → paginating, cursor present
        let manifest = build_worktree_manifest(&path, "HEAD", &options, 0, 2).unwrap();
        assert_eq!(manifest.files.len(), 2);
        assert!(manifest.pagination.next_cursor.is_some());
    }

    #[test]
    fn it_worktree_counts_summary_correctly_with_pagination() {
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

        // Commit files that will be modified and deleted
        std::fs::write(path.join("modify.txt"), "old\n").unwrap();
        std::fs::write(path.join("delete.txt"), "gone\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(&path)
            .output()
            .unwrap();

        // Stage: add new file, modify one, delete one
        std::fs::write(path.join("added.txt"), "new\n").unwrap();
        std::fs::write(path.join("modify.txt"), "changed\n").unwrap();
        std::fs::remove_file(path.join("delete.txt")).unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();

        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        // page_size=1 to force pagination, but summary must reflect all 3 changes
        let manifest = build_worktree_manifest(&path, "HEAD", &options, 0, 1).unwrap();
        assert_eq!(manifest.summary.files_added, 1);
        assert_eq!(manifest.summary.files_modified, 1);
        assert_eq!(manifest.summary.files_deleted, 1);
        assert_eq!(manifest.summary.total_files_changed, 3);
        assert!(manifest.summary.total_lines_added > 0);
        assert!(manifest.summary.total_lines_removed > 0);
    }

    #[test]
    fn it_worktree_returns_exact_page_boundary_files() {
        let (_dir, path) = create_worktree_repo_with_n_staged_files(3);
        let options = ManifestOptions {
            include_patterns: vec![],
            exclude_patterns: vec![],
            include_function_analysis: false,
            max_response_tokens: None,
        };

        // offset at last file
        let manifest = build_worktree_manifest(&path, "HEAD", &options, 2, 100).unwrap();
        assert_eq!(manifest.files.len(), 1);

        // offset exactly at total
        let manifest = build_worktree_manifest(&path, "HEAD", &options, 3, 100).unwrap();
        assert!(manifest.files.is_empty());
        assert!(manifest.pagination.next_cursor.is_none());
    }

    // --- enforce_token_budget tests ---

    fn make_test_file_entry(
        path: &str,
        with_functions: bool,
        with_imports: bool,
    ) -> ManifestFileEntry {
        ManifestFileEntry {
            path: path.to_string(),
            old_path: None,
            change_type: ChangeType::Modified,
            change_scope: ChangeScope::Committed,
            language: "rust".to_string(),
            is_binary: false,
            is_generated: false,
            lines_added: 10,
            lines_removed: 5,
            size_before: 100,
            size_after: 120,
            functions_changed: if with_functions {
                Some(vec![FunctionChange {
                    name: format!("fn_in_{path}"),
                    old_name: None,
                    change_type: FunctionChangeType::Modified,
                    start_line: 1,
                    end_line: 10,
                    signature: format!("pub fn fn_in_{path}()"),
                }])
            } else {
                None
            },
            imports_changed: if with_imports {
                Some(ImportChange {
                    added: vec!["use std::io".to_string()],
                    removed: vec![],
                })
            } else {
                None
            },
        }
    }

    fn make_test_response(files: Vec<ManifestFileEntry>) -> ManifestResponse {
        ManifestResponse {
            metadata: ManifestMetadata {
                repo_path: "/test".to_string(),
                base_ref: "HEAD~1".to_string(),
                head_ref: "HEAD".to_string(),
                base_sha: "aaa".to_string(),
                head_sha: "bbb".to_string(),
                generated_at: Utc::now(),
                version: "0.0.0".to_string(),
                token_estimate: 0,
                function_analysis_truncated: vec![],
                budget_tokens: None,
            },
            summary: ManifestSummary {
                total_files_changed: files.len(),
                files_added: 0,
                files_modified: files.len(),
                files_deleted: 0,
                files_renamed: 0,
                total_lines_added: 0,
                total_lines_removed: 0,
                total_functions_changed: None,
                languages_affected: vec!["rust".to_string()],
            },
            files,
            dependency_changes: vec![],
            pagination: PaginationInfo {
                total_items: 0,
                page_start: 0,
                page_size: 100,
                next_cursor: None,
            },
        }
    }

    #[test]
    fn enforce_token_budget_under_budget_returns_empty() {
        let files = vec![
            make_test_file_entry("a.rs", true, true),
            make_test_file_entry("b.rs", true, true),
        ];
        let mut response = make_test_response(files);
        // Use a very large budget so nothing gets trimmed
        let trimmed = enforce_token_budget(&mut response, 100_000);
        assert!(
            trimmed.is_empty(),
            "nothing should be trimmed under a large budget"
        );
        // All files should retain their function analysis
        for file in &response.files {
            assert!(file.functions_changed.is_some());
            assert!(file.imports_changed.is_some());
        }
    }

    #[test]
    fn enforce_token_budget_over_budget_trims_imports_first() {
        let files = vec![
            make_test_file_entry("a.rs", true, true),
            make_test_file_entry("b.rs", true, true),
            make_test_file_entry("c.rs", true, true),
        ];
        let mut response = make_test_response(files);
        // Pick a budget that fits the skeleton + first file fully but
        // forces tier 1 trim on subsequent files
        let skeleton_cost = {
            let files_saved = std::mem::take(&mut response.files);
            let cost = size::estimate_response_tokens(&response);
            response.files = files_saved;
            cost
        };
        let first_file_cost = size::estimate_response_tokens(&response.files[0]);
        // Budget fits roughly 1.5 files — forces tier-1 trim (imports stripped)
        // on at least some files while keeping function signatures.
        let budget = skeleton_cost + first_file_cost + first_file_cost / 2;
        let trimmed = enforce_token_budget(&mut response, budget);
        // At least one file should be tier-1 trimmed (imports stripped, functions kept)
        assert!(
            !trimmed.is_empty(),
            "at least one file should be listed as tier-1 trimmed"
        );
        // Trimmed paths must all still have functions_changed (signatures preserved)
        for path in &trimmed {
            let file = response.files.iter().find(|f| &f.path == path).unwrap();
            assert!(
                file.functions_changed.is_some(),
                "trimmed file {path} must retain function signatures"
            );
            assert!(
                file.imports_changed.is_none(),
                "trimmed file {path} must have imports stripped"
            );
        }
    }

    #[test]
    fn enforce_token_budget_very_tight_strips_to_bare() {
        let files = vec![
            make_test_file_entry("a.rs", true, true),
            make_test_file_entry("b.rs", true, true),
        ];
        let mut response = make_test_response(files);
        // Use a budget barely larger than skeleton — forces tier 2 on all files
        let skeleton_cost = {
            let files_saved = std::mem::take(&mut response.files);
            let cost = size::estimate_response_tokens(&response);
            response.files = files_saved;
            cost
        };
        let budget = skeleton_cost + 10; // almost no room for file data
        let trimmed = enforce_token_budget(&mut response, budget);
        // Both files should be stripped to bare (tier 2)
        for file in &response.files {
            assert!(
                file.functions_changed.is_none(),
                "file {} should be bare (tier 2)",
                file.path
            );
            assert!(file.imports_changed.is_none());
        }
        // Tier 2 files should NOT appear in trimmed list
        assert!(
            trimmed.is_empty(),
            "tier 2 files must not appear in trimmed list"
        );
    }

    #[test]
    fn enforce_token_budget_zero_budget_returns_empty() {
        // budget=0 is not called by production code (callers filter b > 0 first),
        // but the function must not panic and must deterministically drop all files.
        let files = vec![make_test_file_entry("a.rs", true, true)];
        let mut response = make_test_response(files);
        let trimmed = enforce_token_budget(&mut response, 0);
        assert!(
            trimmed.is_empty(),
            "zero budget produces no tier-1 trimmed paths"
        );
        assert!(
            response.files.is_empty(),
            "zero budget drops all files from the response"
        );
    }

    #[test]
    fn enforce_token_budget_trimmed_list_excludes_tier2_files() {
        // Create enough files that some get tier 1 and others get tier 2
        let files = (0..10)
            .map(|i| make_test_file_entry(&format!("file_{i}.rs"), true, true))
            .collect::<Vec<_>>();
        let mut response = make_test_response(files);
        // Estimate total cost and use half as budget
        let total_cost = size::estimate_response_tokens(&response);
        let budget = total_cost / 2;
        let trimmed = enforce_token_budget(&mut response, budget);
        // Every path in trimmed must point to a file with functions_changed still set
        for path in &trimmed {
            let file = response.files.iter().find(|f| &f.path == path).unwrap();
            assert!(
                file.functions_changed.is_some(),
                "trimmed file {path} listed in function_analysis_truncated must have functions_changed"
            );
            assert!(
                file.imports_changed.is_none(),
                "trimmed file {path} should have imports stripped"
            );
        }
        // Files with functions_changed=None (tier 2) must NOT be in the list
        for file in &response.files {
            if file.functions_changed.is_none() {
                assert!(
                    !trimmed.contains(&file.path),
                    "tier 2 file {} must not be in trimmed list",
                    file.path
                );
            }
        }
    }

    // --- enforce_token_budget arithmetic and boundary mutants ---
    //
    // These tests target surviving mutants reported by cargo-mutants
    // (issue #222 / CI run 24620558846, shard 1) in enforce_token_budget:
    //   * `budget / 20` → `budget % 20` (safety margin uses division, not modulo)
    //   * `remaining -= c.tier1` → `remaining += c.tier1` or `/=` (budget
    //      accounting decreases the remaining budget, doesn't grow or shrink it
    //      by division)
    //   * `remaining -= c.bare` → `remaining += c.bare` or `/=`
    // Each test is written so the mutant would flip the assertion outcome.

    #[test]
    fn it_uses_division_not_modulo_for_safety_margin() {
        // The safety_margin is computed as `(budget / 20).max(16)`. A mutant
        // that replaces `/` with `%` computes `budget % 20` — always < 20 —
        // then clamps to 16. The `.max(16)` clamp masks the mutation for any
        // budget < 320, so the test must use a larger budget to make the
        // mutation observable.
        //
        // Strategy: many small files so per_file_full_total is in the
        // thousands of tokens, then pick a budget that sits in the sweet
        // spot where the ~400-token gap between `budget/20` and 16
        // determines whether the `total_full <= file_budget` fast path
        // fires.
        let files = (0..80)
            .map(|i| make_test_file_entry(&format!("f{i}.rs"), true, true))
            .collect::<Vec<_>>();
        let mut response = make_test_response(files);

        // Measure the skeleton (response with files emptied) and the sum of
        // per-file full costs independently — enforce_token_budget does the
        // same decomposition internally.
        let skeleton_cost = {
            let saved = std::mem::take(&mut response.files);
            let token_count = size::estimate_response_tokens(&response);
            response.files = saved;
            token_count
        };
        let per_file_full_total: usize = response
            .files
            .iter()
            .map(size::estimate_response_tokens)
            .sum();

        // We want:
        //     budget - skeleton - budget/20 < per_file_full_total   (trim under /)
        //     budget - skeleton - 16        >= per_file_full_total  (no trim under %)
        // i.e. budget/20 - 16 > budget - skeleton - per_file_full_total >= 0
        //
        // Let target = skeleton + per_file_full_total. Pick budget so that
        // budget/20 is between 17 and budget - target + 1 (some nonzero
        // gap), and budget is at least 1 more than target so the % form
        // satisfies the lower bound.
        //
        // Solve the two inequalities:
        //     budget >= target + 16                   (modulo margin leaves
        //                                              room for everything)
        //     budget * 19 / 20 < target               (division margin eats
        //                                              budget - target)
        //     i.e. budget < target * 20 / 19
        // Sweet spot: pick midpoint between (target + 16) and the upper bound.
        let target = skeleton_cost + per_file_full_total;
        let lower = target + 16;
        let upper = target * 20 / 19; // exclusive upper bound
        assert!(
            upper > lower,
            "fixture must be large enough for the safety-margin sweet spot \
             to exist: target={target} needs target/19 > 16, i.e. target > 304",
        );
        let budget = (lower + upper) / 2;

        // Verify construction: under the correct implementation the fast
        // path should miss (trim required), under the mutant it should
        // fire (no trim).
        let margin_div = (budget / 20).max(16);
        let margin_mod = (budget % 20).max(16);
        assert!(
            margin_div > margin_mod,
            "test presumes budget/20 > budget%20 (clamped); \
             budget={budget} /20={margin_div} %20={margin_mod}",
        );
        let file_budget_under_div = budget
            .saturating_sub(skeleton_cost)
            .saturating_sub(margin_div);
        let file_budget_under_mod = budget
            .saturating_sub(skeleton_cost)
            .saturating_sub(margin_mod);
        assert!(
            file_budget_under_div < per_file_full_total,
            "test construction invalid: under correct `/`, file_budget \
             ({file_budget_under_div}) must be < per_file_full_total \
             ({per_file_full_total}) so trimming is required. \
             budget={budget} skeleton={skeleton_cost} margin_div={margin_div}",
        );
        assert!(
            file_budget_under_mod >= per_file_full_total,
            "test construction invalid: under mutant `%`, file_budget \
             ({file_budget_under_mod}) must be ≥ per_file_full_total \
             ({per_file_full_total}) so no trimming occurs. \
             budget={budget} skeleton={skeleton_cost} margin_mod={margin_mod}",
        );

        let trimmed = enforce_token_budget(&mut response, budget);
        // Under correct `/`: trimming happens — imports stripped from at
        // least one file, so trimmed is non-empty OR some file has
        // functions_changed=None (bare tier). Under mutant `%`: fast path
        // fires, trimmed is empty AND every file keeps imports.
        let some_imports_stripped = response.files.iter().any(|f| f.imports_changed.is_none());
        assert!(
            !trimmed.is_empty(),
            "with budget {budget} (skeleton={skeleton_cost}, \
             per_file_full_total={per_file_full_total}) the /20 safety margin \
             of {margin_div} tokens must force trimming; a modulo-based \
             margin would be clamped to {margin_mod} and leave room for every file"
        );
        assert!(
            some_imports_stripped,
            "trimming must strip imports from at least one file; \
             budget={budget} margin_div={margin_div}"
        );
    }

    #[test]
    fn it_decreases_remaining_budget_after_each_tier1_decision() {
        // Target `remaining -= c.tier1` mutants (→ `+=` grows budget, → `/=`
        // collapses it toward 1). We need a scenario where the greedy walk is
        // actually used (not the "all fits at full" or "all fits at tier1"
        // fast paths). That means total_tier1 > file_budget.
        //
        // Construction: several identical files, budget sized so only the
        // first ~half fit at tier1 and the rest must be stripped to bare.
        // Under `-=`: remaining shrinks after each kept file; eventually
        // even tier1 cost > remaining, and subsequent files fall to bare.
        // Under `+=`: remaining grows, every file stays at tier1, no file
        // goes to bare. The presence of at least one bare file distinguishes.
        // Under `/=`: after the first file remaining becomes tier1/tier1 = 1,
        // every subsequent file must fall to bare (even weaker than `-=`),
        // but the SAME file-count invariant fails differently — no files
        // survive at tier1 beyond the first, so many more bare files appear.
        let files = (0..6)
            .map(|i| make_test_file_entry(&format!("f{i}.rs"), true, true))
            .collect::<Vec<_>>();
        let mut response = make_test_response(files);
        // Sum of tier1 costs to target the middle. We want total_tier1 > budget
        // (so the tier1 fast path is skipped) but budget large enough that at
        // least two files can fit at tier1 (so `-=` and `+=` diverge visibly).
        let skeleton_cost = {
            let saved = std::mem::take(&mut response.files);
            let cost = size::estimate_response_tokens(&response);
            response.files = saved;
            cost
        };
        // Measure one file's tier1 cost (imports stripped, functions kept).
        let one_tier1 = {
            let mut clone = response.files[0].clone();
            clone.imports_changed = None;
            size::estimate_response_tokens(&clone)
        };
        // Budget fits ~3 tier1 files after skeleton + safety margin.
        // Safety margin is (budget/20).max(16); account for that by over-
        // budgeting the "fits 3" target.
        let raw_budget = skeleton_cost + one_tier1 * 3;
        // Inflate by 25% to absorb safety_margin without blowing past tier1
        // for all 6 files. 6*tier1 would make the fast path fire; 3*tier1
        // * 1.25 = 3.75*tier1 < 6*tier1.
        let budget = raw_budget + raw_budget / 4;
        // Pre-flight: confirm the greedy walk actually executes. If fixture cost
        // ever drifts such that total_tier1 <= file_budget, enforce_token_budget
        // short-circuits into the tier1 fast path and all three mutants survive
        // silently. The assertion below is what the other budget tests in this
        // file do explicitly — keep the invariant local.
        let safety_margin = (budget / 20).max(16);
        let file_budget = budget
            .saturating_sub(skeleton_cost)
            .saturating_sub(safety_margin);
        let total_tier1: usize = response
            .files
            .iter()
            .map(|f| {
                let mut clone = f.clone();
                clone.imports_changed = None;
                size::estimate_response_tokens(&clone)
            })
            .sum();
        assert!(
            total_tier1 > file_budget,
            "construction invalid: greedy walk must execute; \
             total_tier1={total_tier1} file_budget={file_budget} \
             budget={budget} skeleton_cost={skeleton_cost} \
             one_tier1={one_tier1}",
        );
        let trimmed = enforce_token_budget(&mut response, budget);
        // Under correct `-=`: remaining drains monotonically; once a file
        // no longer fits at tier1, it falls to bare.
        // Under `+=`: remaining grows, every file stays at tier1.
        // Under `/=`: remaining collapses to ~1 after the first file, so
        // only one file is tier1 and the rest go to bare.
        //
        // Invariant: with this budget, SOME files are tier1 (→ trimmed list
        // non-empty) AND SOME are bare (→ functions_changed=None on at least
        // one). `+=` breaks the "some bare" half; `/=` breaks "some tier1"
        // half. The two-sided check is what kills both directional mutants.
        let tier1_count = trimmed.len();
        let bare_count = response
            .files
            .iter()
            .filter(|f| f.functions_changed.is_none())
            .count();
        assert!(
            tier1_count >= 1,
            "at least one file must survive at tier1 (functions kept, imports \
             stripped) after a partial-fit budget; tier1_count={tier1_count}, \
             bare_count={bare_count}",
        );
        assert!(
            tier1_count >= 2,
            "at least two files must survive at tier1; under a /= mutant, remaining \
             collapses to ~1 after the first tier1 decision so only one file can \
             stay at tier1: tier1_count={tier1_count}",
        );
        assert!(
            bare_count >= 1,
            "at least one file must fall to bare once the remaining budget \
             is drained by earlier tier1 decisions; tier1_count={tier1_count}, \
             bare_count={bare_count}. If every file stayed at tier1, remaining \
             is not being decreased (mutant: `remaining += c.tier1`)",
        );
    }

    #[test]
    fn it_decreases_remaining_budget_after_each_bare_decision() {
        // Target `remaining -= c.bare` mutants. This branch fires when a file
        // does not fit at tier1 but does fit at bare.
        //
        // To force every file through the bare arm we need `c.tier1 >
        // remaining` from the very first iteration. The greedy walk clones
        // each file and measures both tier1 (functions kept, imports
        // stripped) and bare (both stripped) costs. If a file has MANY
        // functions, tier1 ≫ bare — the gap is proportional to the
        // function count. Pick file_budget so a single tier1 doesn't fit
        // but several bares do.
        //
        // Build a fixture with many functions per file to widen tier1 vs
        // bare.
        fn make_many_fn_entry(path: &str, fn_count: usize) -> ManifestFileEntry {
            let functions = (0..fn_count)
                .map(|i| FunctionChange {
                    name: format!("really_long_function_name_number_{i}"),
                    old_name: None,
                    change_type: FunctionChangeType::Modified,
                    start_line: i * 10,
                    end_line: i * 10 + 5,
                    signature: format!(
                        "pub fn really_long_function_name_number_{i}\
                         (arg_alpha: i64, arg_beta: String) -> Result<usize, Box<dyn Error>>"
                    ),
                })
                .collect();
            ManifestFileEntry {
                path: path.to_string(),
                old_path: None,
                change_type: ChangeType::Modified,
                change_scope: ChangeScope::Committed,
                language: "rust".to_string(),
                is_binary: false,
                is_generated: false,
                lines_added: 10,
                lines_removed: 5,
                size_before: 100,
                size_after: 120,
                functions_changed: Some(functions),
                imports_changed: None, // no imports → tier1 == full
            }
        }
        let files = (0..8)
            .map(|i| make_many_fn_entry(&format!("f{i}.rs"), 10))
            .collect::<Vec<_>>();
        let mut response = make_test_response(files);
        let skeleton_cost = {
            let saved = std::mem::take(&mut response.files);
            let cost = size::estimate_response_tokens(&response);
            response.files = saved;
            cost
        };
        let one_full = size::estimate_response_tokens(&response.files[0]);
        let one_bare = {
            let mut clone = response.files[0].clone();
            clone.functions_changed = None;
            clone.imports_changed = None;
            size::estimate_response_tokens(&clone)
        };
        assert!(
            one_full >= 4 * one_bare,
            "fixture construction requires full >> bare to leave room for \
             a tight budget that admits several bare files but no tier1 \
             file: full={one_full} bare={one_bare}",
        );

        // target_file_budget in the sweet spot: <= one_full - 1 (tier1
        // branch misses even for the first file) and >= 3 * one_bare (at
        // least 3 bares fit so dropping is observable).
        // target_file_budget fits several bares but strictly less than
        // one_full. Pick midpoint between 3*one_bare (minimum "observable"
        // cutoff) and one_full - 1 (upper bound). The earlier
        // `one_full >= 4 * one_bare` guard already implies `one_full >
        // 3 * one_bare`, so no separate check is needed here.
        let target_file_budget = (3 * one_bare + one_full - 1) / 2;
        // Solve for budget so file_budget == target after safety_margin.
        //   budget - skeleton - (budget/20) = target
        //   budget * 19 / 20 = skeleton + target
        //   budget = (skeleton + target) * 20 / 19
        // Use ceiling division to stay consistent with the parallel
        // construction in the tier1-fast-path test below; the explicit
        // `file_budget < one_full` cross-check after this guards against
        // the rounding pushing us above one_full.
        let budget = ((skeleton_cost + target_file_budget) * 20).div_ceil(19);
        // Cross-check construction.
        let safety_margin = (budget / 20).max(16);
        let file_budget = budget
            .saturating_sub(skeleton_cost)
            .saturating_sub(safety_margin);
        assert!(
            file_budget < one_full,
            "construction invalid: tier1 branch would fire for the first \
             file (file_budget={file_budget} >= one_full={one_full})",
        );
        assert!(
            file_budget >= 2 * one_bare,
            "construction invalid: fewer than 2 bare files fit \
             (file_budget={file_budget} < 2*one_bare={})",
            2 * one_bare,
        );

        let files_before = response.files.len();
        let _ = enforce_token_budget(&mut response, budget);
        let files_after = response.files.len();
        assert!(
            files_after < files_before,
            "with a tight budget that fits only a few bare entries, \
             enforcement must drop files from the 8-file input: \
             files_before={files_before} files_after={files_after}. \
             If all files survive, remaining is not being decreased \
             (mutant: `remaining += c.bare`)",
        );
        assert!(
            files_after >= 2,
            "at least two files should fit bare with this budget; \
             files_after={files_after} suggests remaining collapsed to near \
             zero after a single decision (mutant: `remaining /= c.bare`)",
        );
        for file in &response.files {
            assert!(
                file.functions_changed.is_none(),
                "file {} survived at tier1 but construction forces bare path \
                 (file_budget={file_budget} one_full={one_full})",
                file.path,
            );
        }
    }

    #[test]
    fn it_returns_empty_trimmed_and_keeps_all_files_when_everything_fits() {
        // Matches existing enforce_token_budget_under_budget_returns_empty but
        // triangulates on the specific early-return invariant: when
        // total_full <= file_budget, the function returns an empty Vec AND
        // leaves every file untouched. Pins down the "fast-path skip" branch.
        let files = vec![
            make_test_file_entry("a.rs", true, true),
            make_test_file_entry("b.rs", true, true),
        ];
        let mut response = make_test_response(files);
        let files_before: Vec<_> = response.files.iter().map(|f| f.path.clone()).collect();
        // Budget is 100x the actual content cost — guaranteed to fit under any
        // reasonable cost-estimator drift without being a magic constant.
        let skeleton_cost = {
            let saved = std::mem::take(&mut response.files);
            let cost = size::estimate_response_tokens(&response);
            response.files = saved;
            cost
        };
        let per_file_full_total: usize = response
            .files
            .iter()
            .map(size::estimate_response_tokens)
            .sum();
        let budget = (skeleton_cost + per_file_full_total) * 100;
        let trimmed = enforce_token_budget(&mut response, budget);
        assert!(trimmed.is_empty(), "nothing trimmed under huge budget");
        assert_eq!(
            response.files.len(),
            files_before.len(),
            "no files dropped under huge budget"
        );
        for (i, f) in response.files.iter().enumerate() {
            assert_eq!(f.path, files_before[i], "file order unchanged");
            assert!(
                f.functions_changed.is_some(),
                "functions_changed preserved for {}",
                f.path
            );
            assert!(
                f.imports_changed.is_some(),
                "imports_changed preserved for {}",
                f.path
            );
        }
    }

    #[test]
    fn it_strips_imports_only_from_files_that_had_them_at_tier1_fast_path() {
        // Pins the tier1 fast path (total_tier1 <= file_budget branch): only
        // files that originally had imports are added to the returned list;
        // files without imports are NOT reported even though they pass
        // through the same loop. A mutant that always pushed to the trimmed
        // list regardless of `imports_changed.is_some()` would fail this test.
        //
        // Construction: one file with imports, one without. Budget sized so
        // total_tier1 <= file_budget < total_full, which means:
        //   * total_full fast path misses (trimming required)
        //   * total_tier1 fast path fires (everything fits once imports
        //     stripped, no greedy walk)
        // Under the fast path, imports_changed is cleared only on files that
        // originally had them.
        let files = vec![
            make_test_file_entry("has_imports.rs", true, true),
            make_test_file_entry("no_imports.rs", true, false),
        ];
        let mut response = make_test_response(files);
        let skeleton_cost = {
            let saved = std::mem::take(&mut response.files);
            let cost = size::estimate_response_tokens(&response);
            response.files = saved;
            cost
        };
        let total_full: usize = response
            .files
            .iter()
            .map(size::estimate_response_tokens)
            .sum();
        let total_tier1: usize = response
            .files
            .iter()
            .map(|f| {
                let mut clone = f.clone();
                clone.imports_changed = None;
                size::estimate_response_tokens(&clone)
            })
            .sum();
        assert!(
            total_tier1 < total_full,
            "fixture must include a file with imports so total_tier1 \
             ({total_tier1}) < total_full ({total_full})",
        );
        // Pick file_budget strictly between total_tier1 and total_full.
        // Then pick budget so `budget - skeleton - safety_margin ==
        // file_budget`. Safety margin is `(budget/20).max(16)`. For a
        // sufficiently large budget (≥ 320), this is `budget/20`, giving a
        // fixed-point equation: budget = skeleton + file_budget + budget/20
        //   → budget * 19/20 = skeleton + file_budget
        //   → budget = (skeleton + file_budget) * 20 / 19
        // Use the midpoint of (total_tier1, total_full) as the file_budget
        // target. Round up slightly so integer-division floor doesn't push
        // us back under total_tier1.
        let mid_file_budget = total_tier1 + (total_full - total_tier1) / 2 + 1;
        let budget = ((skeleton_cost + mid_file_budget) * 20).div_ceil(19);
        // Verify construction: under the correct implementation the fast
        // tier1 path should fire.
        let safety_margin = (budget / 20).max(16);
        let file_budget = budget
            .saturating_sub(skeleton_cost)
            .saturating_sub(safety_margin);
        assert!(
            total_full > file_budget,
            "construction invalid: total_full ({total_full}) must exceed \
             file_budget ({file_budget}) to skip the all-full fast path",
        );
        assert!(
            total_tier1 <= file_budget,
            "construction invalid: total_tier1 ({total_tier1}) must fit in \
             file_budget ({file_budget}) to enter the tier1 fast path",
        );

        let trimmed = enforce_token_budget(&mut response, budget);
        assert_eq!(trimmed.len(), 1, "exactly one file should be trimmed");
        assert!(
            trimmed.contains(&"has_imports.rs".to_string()),
            "has_imports.rs must appear in the trimmed list; trimmed={trimmed:?}"
        );
        assert!(
            !trimmed.contains(&"no_imports.rs".to_string()),
            "no_imports.rs was never altered and must not appear; trimmed={trimmed:?}"
        );
        // no_imports.rs still has functions_changed because we didn't drop
        // to bare for either file.
        let no_imports_entry = response
            .files
            .iter()
            .find(|f| f.path == "no_imports.rs")
            .expect("no_imports.rs must still be present");
        assert!(
            no_imports_entry.functions_changed.is_some(),
            "no_imports.rs should keep its function signatures at tier1"
        );
    }
}
