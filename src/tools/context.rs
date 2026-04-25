use std::path::Path;

use chrono::Utc;

use crate::git::reader::RepoReader;
use crate::pagination::{
    FUNCTION_CURSOR_VERSION, FunctionPaginationCursor, PaginationInfo, clamp_page_size,
    decode_function_cursor, encode_function_cursor, validate_function_cursor,
};
use crate::tools::extension_from_path;
use crate::tools::import_scope::{self, RepoContext};
use crate::tools::manifest::build_manifest;
use crate::tools::size;
use crate::tools::types::{
    BlastRadius, CalleeEntry, CallerEntry, ContextMetadata, FunctionChangeType,
    FunctionContextEntry, FunctionContextResponse, ManifestOptions, ScopingMode, ToolError,
};
use crate::treesitter::analyzer_for_extension;

/// Options passed from the CLI / MCP handlers into
/// [`build_function_context`]. Keeping this struct internal to `tools` mirrors
/// [`ManifestOptions`]: CLI/MCP code builds one of these from the user-facing
/// args types, and the build function never depends on the args types
/// directly.
#[derive(Debug, Clone)]
pub struct ContextOptions {
    /// Incoming pagination cursor from a prior response, `None` for the first
    /// page.
    pub cursor: Option<String>,
    /// Maximum functions per page; clamped to `[1, 500]` by
    /// [`clamp_page_size`].
    pub page_size: usize,
    /// When set, restrict the response to functions whose names match. Must
    /// be identical across paginated calls (not validated by the cursor).
    pub function_names: Option<Vec<String>>,
    /// Response-size budget in estimated tokens. `None` or `Some(0)` disables
    /// enforcement; `Some(n > 0)` triggers per-entry clamping when the
    /// response exceeds `n` tokens.
    pub max_response_tokens: Option<usize>,
}

impl Default for ContextOptions {
    fn default() -> Self {
        Self {
            cursor: None,
            page_size: 25,
            function_names: None,
            max_response_tokens: None,
        }
    }
}

fn is_test_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("/__tests__/")
        || lower.contains("/spec/")
        || lower.ends_with("_test.go")
        || lower.ends_with("_test.rs")
        || lower
            .rsplit('/')
            .next()
            .is_some_and(|name| name.starts_with("test_"))
        || lower.ends_with(".test.ts")
        || lower.ends_with(".test.js")
        || lower.ends_with(".test.tsx")
        || lower.ends_with(".test.jsx")
        || lower.ends_with("_spec.rb")
        || lower.ends_with("test.java")
        || lower.ends_with("tests.cs")
}

/// Build function context for changed functions in a commit range.
///
/// For each changed function in the manifest:
/// 1. Extract its callees from the head version of its file
/// 2. Scan all files in the repo at head_ref for callers
/// 3. Classify callers as test references based on path conventions
///
/// This is a thin convenience wrapper that uses the default
/// [`ContextOptions`]: no cursor, default page size, no name filter, no
/// token budget. Production call sites (the CLI and the MCP handler) should
/// call [`build_function_context_with_options`] to pass real pagination and
/// budget knobs.
#[cfg(test)]
pub fn build_function_context(
    repo_path: &Path,
    base_ref: &str,
    head_ref: &str,
) -> Result<FunctionContextResponse, ToolError> {
    build_function_context_with_options(repo_path, base_ref, head_ref, &ContextOptions::default())
}

/// Build function context with explicit pagination, filter, and budget knobs.
///
/// Precedence of the knobs (must not be reordered):
///
/// 1. `function_names` filter — narrows the working set before pagination.
/// 2. Incoming `cursor` — if valid, its offset becomes the page start.
/// 3. `page_size` — clamped to `[1, 500]`; slice the filtered list to this
///    many entries.
/// 4. `max_response_tokens` — runs the per-entry budget clamp on the
///    already-paginated slice. A `Some(0)` or `None` disables the budget.
pub fn build_function_context_with_options(
    repo_path: &Path,
    base_ref: &str,
    head_ref: &str,
    options: &ContextOptions,
) -> Result<FunctionContextResponse, ToolError> {
    let _root_span = tracing::info_span!(
        "context.build",
        functions_changed = tracing::field::Empty,
        files_scanned = tracing::field::Empty,
        total_callers_found = tracing::field::Empty,
    )
    .entered();

    let reader = RepoReader::open(repo_path)?;
    let base_commit = reader.resolve_commit(base_ref)?;
    let head_commit = reader.resolve_commit(head_ref)?;

    // Step 1: Get the manifest to find changed functions. Always pass
    // `max_response_tokens: None` — the context tool reads the full manifest
    // internally and applies its own budget to the context response below.
    let _manifest_span = tracing::info_span!("context.get_manifest").entered();
    let manifest_options = ManifestOptions {
        include_patterns: vec![],
        exclude_patterns: vec![],
        include_function_analysis: true,
        max_response_tokens: None,
    };
    let manifest = build_manifest(repo_path, base_ref, head_ref, &manifest_options, 0, 10_000)?;

    drop(_manifest_span);

    // Collect changed functions with their file paths. Sort the list by name
    // so pagination is deterministic across repeated calls with the same
    // inputs; the Gherkin "first page ... in deterministic order" scenario
    // asserts this explicitly.
    let mut changed_functions: Vec<(String, String, FunctionChangeType)> = Vec::new();
    for file in &manifest.files {
        if let Some(ref fns) = file.functions_changed {
            for fc in fns {
                changed_functions.push((
                    fc.name.clone(),
                    file.path.clone(),
                    fc.change_type.clone(),
                ));
            }
        }
    }
    changed_functions.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    // Apply optional function-name filter BEFORE pagination so the cursor
    // offset is always into the filtered list. Absent filter or empty vec
    // leaves the list untouched.
    if let Some(ref names) = options.function_names
        && !names.is_empty()
    {
        let allow: std::collections::HashSet<&str> = names.iter().map(String::as_str).collect();
        changed_functions.retain(|(name, _, _)| allow.contains(name.as_str()));
    }

    // Validate and decode incoming cursor against the current range SHAs.
    // A stale cursor (base/head changed between calls) is surfaced as an
    // error via `ToolError::Git` rather than silently resetting to page 0.
    let start_offset = if let Some(ref cursor_str) = options.cursor {
        let cursor = decode_function_cursor(cursor_str)?;
        validate_function_cursor(&cursor, &base_commit.sha, &head_commit.sha)?;
        cursor.offset
    } else {
        0
    };

    let filtered_total = changed_functions.len();
    let page_size = clamp_page_size(options.page_size);
    let end_offset = (start_offset + page_size).min(filtered_total);
    let page_slice = if start_offset >= filtered_total {
        Vec::new()
    } else {
        changed_functions[start_offset..end_offset].to_vec()
    };

    // Step 2: List all files in the repo at head_ref for caller scanning
    let all_files = reader.list_files_at_ref(head_ref)?;

    // Load repo-level context (Cargo.toml crate name, go.mod module path) so
    // Rust integration tests and Go imports match correctly.
    let repo_ctx = RepoContext::load(repo_path);

    // Infer module paths for the files whose functions appear on THIS page.
    // Scoping on the page (not the full filtered set) keeps `function_names`
    // re-queries cheap: asking for one name shouldn't force the server to
    // scan every file that contained any changed function across the range.
    let changed_file_paths: std::collections::HashSet<&str> =
        page_slice.iter().map(|(_, p, _)| p.as_str()).collect();
    let changed_modules: Vec<(&str, Option<String>, bool)> = page_slice
        .iter()
        .map(|(_, p, _)| p.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|p| {
            let ext = extension_from_path(p);
            let module = import_scope::infer_module_path(p, ext, &repo_ctx);
            let supports = import_scope::supports_import_scoping(ext);
            (p, module, supports)
        })
        .collect();

    // Step 3: Import-scoped scan — only full-parse files that reference changed modules
    let _scan_span =
        tracing::info_span!("context.scan_files", file_count = all_files.len()).entered();
    let mut file_calls: Vec<(
        String,
        Vec<crate::treesitter::CallSite>,
        Vec<crate::treesitter::Function>,
    )> = Vec::new();
    for file_path in &all_files {
        let ext = extension_from_path(file_path);
        let analyzer = match analyzer_for_extension(ext) {
            Some(a) => a,
            None => continue,
        };
        let content = match reader.read_file_at_ref(head_ref, file_path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    file = %file_path,
                    error = %e,
                    "skipping file: read_file_at_ref failed"
                );
                continue;
            }
        };

        // Check if this file should be scanned via import scoping
        let is_changed_file = changed_file_paths.contains(file_path.as_str());
        // If ANY changed file is in a language that doesn't support scoping,
        // OR has no inferred module, we must fall back for that file — so this
        // file must be scanned to cover that case.
        let any_changed_needs_fallback = changed_modules
            .iter()
            .any(|(_, module, supports)| !*supports || module.is_none());

        let should_scan = if is_changed_file {
            // Always scan the changed file itself (for same-file callers)
            true
        } else if !import_scope::supports_import_scoping(ext) || any_changed_needs_fallback {
            // Unsupported scanning language, or fallback required for some
            // changed file: full scan
            true
        } else {
            // Go uses same-package semantics: same-directory files can
            // call each other without explicit imports
            let is_same_pkg = ext == "go"
                && changed_file_paths
                    .iter()
                    .any(|cf| import_scope::same_directory(cf, file_path));
            if is_same_pkg {
                true
            } else {
                // Lightweight import extraction to check relationship
                let imports = analyzer
                    .extract_imports(content.as_bytes())
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            file = %file_path,
                            language = ext,
                            error = %e,
                            "extract_imports failed; skipping import scope"
                        );
                        Vec::new()
                    });
                changed_modules.iter().any(|(_, module_path, _)| {
                    if let Some(mp) = module_path {
                        import_scope::imports_reference_module(
                            &imports, mp, file_path, ext, &repo_ctx,
                        )
                    } else {
                        false
                    }
                })
            }
        };

        if should_scan {
            let calls = analyzer
                .extract_calls(content.as_bytes())
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        file = %file_path,
                        language = ext,
                        error = %e,
                        "extract_calls failed; treating as no calls"
                    );
                    Vec::new()
                });
            let functions = analyzer
                .extract_functions(content.as_bytes())
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        file = %file_path,
                        language = ext,
                        error = %e,
                        "extract_functions failed; treating as no functions"
                    );
                    Vec::new()
                });
            if !calls.is_empty() || !functions.is_empty() {
                file_calls.push((file_path.clone(), calls, functions));
            }
        }
    }

    drop(_scan_span);

    // Step 4: Build context for each changed function
    let _match_span = tracing::info_span!("context.match_callers").entered();
    let mut function_entries: Vec<FunctionContextEntry> = Vec::new();

    for (func_name, func_file, change_type) in &page_slice {
        // Extract callees: calls made BY this function in the head version
        let callees = extract_callees_for_function(&reader, head_ref, func_file, func_name);

        // Find callers: other files (and same file) that call this function
        let mut callers = Vec::new();
        let mut test_references = Vec::new();

        let leaf_name = leaf_function_name(func_name);

        for (caller_file, calls, functions) in &file_calls {
            for call in calls {
                let call_leaf = leaf_function_name(&call.callee);
                if call_leaf == leaf_name {
                    // Find which function contains this call
                    let containing_fn = find_containing_function(functions, call.line);
                    let is_test = is_test_path(caller_file);

                    let entry = CallerEntry {
                        file: caller_file.clone(),
                        line: call.line,
                        caller: containing_fn.unwrap_or_default(),
                        is_test,
                    };

                    if is_test {
                        test_references.push(entry);
                    } else {
                        callers.push(entry);
                    }
                }
            }
        }

        let caller_count = callers.len() + test_references.len();
        let blast_radius = BlastRadius::compute(callers.len(), test_references.len());

        // The scoping mode for this function is determined by its file's
        // language: if the language supports scoping AND we could infer a
        // module path AND no other changed file forced a global fallback,
        // the scan was scoped.
        let func_ext = extension_from_path(func_file);
        let func_has_module =
            import_scope::infer_module_path(func_file, func_ext, &repo_ctx).is_some();
        let scoping_mode = if import_scope::supports_import_scoping(func_ext)
            && func_has_module
            && !changed_modules.iter().any(|(_, m, s)| !*s || m.is_none())
        {
            ScopingMode::Scoped
        } else {
            ScopingMode::Fallback
        };

        function_entries.push(FunctionContextEntry {
            name: func_name.clone(),
            file: func_file.clone(),
            change_type: change_type.clone(),
            blast_radius,
            scoping_mode,
            callers,
            callees,
            test_references,
            caller_count,
            truncated: false,
        });
    }

    drop(_match_span);

    let total_callers: usize = function_entries.iter().map(|f| f.caller_count).sum();
    _root_span.record("functions_changed", function_entries.len() as i64);
    _root_span.record("files_scanned", file_calls.len() as i64);
    _root_span.record("total_callers_found", total_callers as i64);

    let mut response = FunctionContextResponse {
        metadata: ContextMetadata {
            base_ref: base_ref.to_string(),
            head_ref: head_ref.to_string(),
            base_sha: base_commit.sha.clone(),
            head_sha: head_commit.sha.clone(),
            generated_at: Utc::now(),
            // Placeholder; see build_manifest for the two-pass estimate trick.
            token_estimate: 0,
            function_analysis_truncated: vec![],
            budget_tokens: None,
            next_cursor: None,
        },
        functions: function_entries,
        pagination: PaginationInfo {
            total_items: filtered_total,
            page_start: start_offset,
            page_size,
            next_cursor: None,
        },
    };

    // Run the per-entry token budget on the already-paginated slice. Returns
    // the names of clamped entries and (if any) the local index of the first
    // entry that was dropped because even the clamped form didn't fit.
    let (clamped_names, local_cutoff) = match options.max_response_tokens {
        Some(budget) if budget > 0 => enforce_context_token_budget(&mut response, budget),
        _ => (Vec::new(), None),
    };
    response.metadata.function_analysis_truncated = clamped_names;

    // Resolve the next cursor. Both limits can fire; the earlier (smaller
    // global offset) one wins. `Option::min` would be wrong here because
    // stdlib's `Option: Ord` treats `None < Some` — inverting the intent.
    let budget_cutoff = local_cutoff.map(|i| start_offset + i);
    let page_cutoff = if end_offset < filtered_total {
        Some(end_offset)
    } else {
        None
    };
    let next_cursor_offset = resolve_next_cursor_offset(budget_cutoff, page_cutoff);
    if let Some(offset) = next_cursor_offset {
        let cursor = FunctionPaginationCursor {
            version: FUNCTION_CURSOR_VERSION,
            offset,
            base_sha: base_commit.sha.clone(),
            head_sha: head_commit.sha.clone(),
        };
        let encoded = encode_function_cursor(&cursor);
        response.pagination.next_cursor = Some(encoded.clone());
        response.metadata.next_cursor = Some(encoded);

        // Signal page-level truncation on the last kept entry so
        // `metadata.function_analysis_truncated` is non-empty whenever the
        // response was cut short — whether the budget clamped the entry
        // lists OR the page simply rolled over at `page_size`. This keeps
        // the "any follow-up call needed" proxy uniform across truncation
        // causes and matches the BDD contract: a budget-cutoff response
        // for a low-caller-count fixture always has SOMETHING to signal.
        if let Some(last) = response.functions.last_mut()
            && !last.truncated
        {
            last.truncated = true;
            response
                .metadata
                .function_analysis_truncated
                .push(last.name.clone());
        }
    }

    // Update page_size in the response to reflect how many entries actually
    // landed after budget enforcement — the manifest tool does the same thing
    // for file dropping, and the BDD "first page is deterministic" scenario
    // expects `functions.len()` to match the reported page_size.
    response.pagination.page_size = response.functions.len();

    response.metadata.token_estimate = size::estimate_response_tokens(&response);
    Ok(response)
}

/// Resolve the next-page cursor offset from the two independent truncation
/// signals. `budget_cutoff` is the global offset of the first entry dropped
/// by token-budget enforcement; `page_cutoff` is the global offset just past
/// the page-size boundary. Both may fire on the same response, in which case
/// the earlier offset wins so a follow-up call picks up exactly where this
/// one stopped. `Option::min` cannot be used directly: stdlib's
/// `Option: Ord` orders `None < Some`, which inverts the intent here.
fn resolve_next_cursor_offset(
    budget_cutoff: Option<usize>,
    page_cutoff: Option<usize>,
) -> Option<usize> {
    match (budget_cutoff, page_cutoff) {
        (Some(b), Some(p)) => Some(b.min(p)),
        (Some(b), None) => Some(b),
        (None, Some(p)) => Some(p),
        (None, None) => None,
    }
}

/// Two-tier per-function token budget.
///
/// Mirrors the three-phase pattern in [`crate::tools::manifest::enforce_token_budget`]:
/// skeleton cost, uniform-downgrade check, greedy walk. Unlike the manifest
/// helper there is no "bare" entry tier because an entry with no callers,
/// callees, or test references defeats the tool's purpose — if even the
/// clamped form doesn't fit, the entry is dropped entirely and the next-page
/// cursor points at it.
///
/// Returns `(clamped_names, local_cutoff)` where:
/// - `clamped_names` is the list of entry names that had their caller /
///   callee / test-reference lists reduced in place. Their `truncated` flag
///   is also set to `true` as a wire-level signal.
/// - `local_cutoff` is the index into `response.functions` of the first entry
///   that did not fit (even clamped) and was dropped. `None` means every
///   entry on the page fit. The caller converts this local index into a
///   global cursor offset.
fn enforce_context_token_budget(
    response: &mut FunctionContextResponse,
    budget: usize,
) -> (Vec<String>, Option<usize>) {
    const MAX_CALLERS: usize = 5;
    const MAX_CALLEES: usize = 5;
    const MAX_TEST_REFS: usize = 3;

    // Measure skeleton overhead (everything except functions).
    let functions = std::mem::take(&mut response.functions);
    let skeleton_cost = size::estimate_response_tokens(response);
    response.functions = functions;

    // Safety margin — the list in `metadata.function_analysis_truncated`
    // (potentially one name per kept entry), the duplicated `next_cursor`
    // in metadata + pagination, and the post-enforcement bookkeeping entry
    // marker are all populated AFTER enforcement runs. Empirically a 10%
    // margin overshoots on small caps (512 → 561) once those trailing
    // fields land. Reserve ~25% or at least 128 tokens; budgets this tight
    // should not be common, and the overshoot is only two entries.
    let safety_margin = (budget / 4).max(128);
    let entry_budget = budget
        .saturating_sub(skeleton_cost)
        .saturating_sub(safety_margin);

    // Phase 1: per-entry cost at each tier.
    struct EntryCosts {
        full: usize,
        clamped: usize,
    }
    let costs: Vec<EntryCosts> = response
        .functions
        .iter()
        .map(|f| {
            let full = size::estimate_response_tokens(f);
            // Build a hypothetical clamped version to measure.
            let clamped = {
                let mut clone = f.clone();
                clamp_entry_lists(&mut clone, MAX_CALLERS, MAX_CALLEES, MAX_TEST_REFS);
                size::estimate_response_tokens(&clone)
            };
            EntryCosts { full, clamped }
        })
        .collect();

    // Phase 2: if everything fits full, no trimming.
    let total_full: usize = costs.iter().map(|c| c.full).sum();
    if total_full <= entry_budget {
        return (Vec::new(), None);
    }

    // Phase 3: greedy walk — prefer full, fall back to clamped, drop the
    // entry (and the remainder of the page) when even clamped won't fit.
    let mut remaining = entry_budget;
    let mut clamped_names = Vec::new();
    for (i, cost) in costs.iter().enumerate() {
        if cost.full <= remaining {
            remaining -= cost.full;
            continue;
        }
        if cost.clamped <= remaining {
            remaining -= cost.clamped;
            let entry = &mut response.functions[i];
            clamp_entry_lists(entry, MAX_CALLERS, MAX_CALLEES, MAX_TEST_REFS);
            entry.truncated = true;
            clamped_names.push(entry.name.clone());
            continue;
        }
        // Even the clamped form won't fit — drop this entry and the tail.
        // The caller attaches a "last entry is truncated" marker after the
        // cutoff is finalized against both the budget and page-size limits,
        // so this helper only reports the cutoff index.
        response.functions.truncate(i);
        return (clamped_names, Some(i));
    }

    (clamped_names, None)
}

/// Reduce an entry's caller / callee / test-reference lists to the given
/// caps. Production callers are preserved over test callers; callees and
/// test references are kept in original order (the agent can recover the
/// full lists via a `function_names` re-query).
fn clamp_entry_lists(
    entry: &mut FunctionContextEntry,
    max_callers: usize,
    max_callees: usize,
    max_test_refs: usize,
) {
    // cargo-mutants: skip -- equivalent mutant: Vec::truncate(N) on a Vec of len==N is a no-op,
    // so `> cap` and `>= cap` are observably identical here.
    if entry.callers.len() > max_callers {
        entry.callers.truncate(max_callers);
    }
    // cargo-mutants: skip -- equivalent mutant: Vec::truncate(N) on a Vec of len==N is a no-op,
    // so `> cap` and `>= cap` are observably identical here.
    if entry.callees.len() > max_callees {
        entry.callees.truncate(max_callees);
    }
    // cargo-mutants: skip -- equivalent mutant: Vec::truncate(N) on a Vec of len==N is a no-op,
    // so `> cap` and `>= cap` are observably identical here.
    if entry.test_references.len() > max_test_refs {
        entry.test_references.truncate(max_test_refs);
    }
}

/// Extract the callees (functions called) from a specific function's body.
fn extract_callees_for_function(
    reader: &RepoReader,
    head_ref: &str,
    file_path: &str,
    func_name: &str,
) -> Vec<CalleeEntry> {
    let _span = tracing::info_span!("context.extract_callees").entered();
    let ext = extension_from_path(file_path);
    let analyzer = match analyzer_for_extension(ext) {
        Some(a) => a,
        None => return vec![],
    };
    let content = match reader.read_file_at_ref(head_ref, file_path) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                file = %file_path,
                error = %e,
                "extract_callees: read_file_at_ref failed"
            );
            return vec![];
        }
    };
    let functions = analyzer
        .extract_functions(content.as_bytes())
        .unwrap_or_else(|e| {
            tracing::warn!(
                file = %file_path,
                error = %e,
                "extract_callees: extract_functions failed"
            );
            Vec::new()
        });
    let calls = analyzer
        .extract_calls(content.as_bytes())
        .unwrap_or_else(|e| {
            tracing::warn!(
                file = %file_path,
                error = %e,
                "extract_callees: extract_calls failed"
            );
            Vec::new()
        });

    // Find the function by name
    let func = match functions.iter().find(|f| f.name == func_name) {
        Some(f) => f,
        None => return vec![],
    };

    // Filter calls to those within this function's line range
    calls
        .iter()
        .filter(|c| c.line >= func.start_line && c.line <= func.end_line)
        .map(|c| CalleeEntry {
            callee: c.callee.clone(),
            line: c.line,
        })
        .collect()
}

/// Extract the "leaf" name from a potentially qualified function name.
/// "std::collections::HashMap::new" → "new"
/// "server.start" → "start"
/// "foo" → "foo"
fn leaf_function_name(name: &str) -> &str {
    name.rsplit_once("::")
        .or_else(|| name.rsplit_once('.'))
        .map(|(_, leaf)| leaf)
        .unwrap_or(name)
}

/// Find which function contains a given line number.
fn find_containing_function(
    functions: &[crate::treesitter::Function],
    line: usize,
) -> Option<String> {
    functions
        .iter()
        .find(|f| line >= f.start_line && line <= f.end_line)
        .map(|f| f.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn create_context_test_repo() -> (TempDir, std::path::PathBuf) {
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

        // Commit 1: lib.rs with calculate, main.rs calling it, test file
        std::fs::create_dir_all(path.join("src")).unwrap();
        std::fs::create_dir_all(path.join("tests")).unwrap();
        std::fs::write(
            path.join("src/lib.rs"),
            "pub fn calculate(x: i32) -> i32 {\n    x + 1\n}\n\npub fn helper(x: i32) -> i32 {\n    x * 2\n}\n",
        ).unwrap();
        std::fs::write(
            path.join("src/main.rs"),
            "use crate::lib::calculate;\n\nfn main() {\n    let result = calculate(42);\n}\n",
        )
        .unwrap();
        std::fs::write(
            path.join("tests/test_lib.rs"),
            "use crate::lib::calculate;\n\nfn test_calculate() {\n    let result = calculate(1);\n}\n",
        )
        .unwrap();

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

        // Commit 2: modify calculate body
        std::fs::write(
            path.join("src/lib.rs"),
            "pub fn calculate(x: i32) -> i32 {\n    x + 1 + helper(x)\n}\n\npub fn helper(x: i32) -> i32 {\n    x * 3\n}\n\npub fn process(data: i32) -> i32 {\n    calculate(data) + helper(data)\n}\n",
        ).unwrap();

        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "modify calculate, add process"])
            .current_dir(&path)
            .output()
            .unwrap();

        (dir, path)
    }

    #[test]
    fn it_returns_context_for_changed_functions() {
        let (_dir, path) = create_context_test_repo();
        let result = build_function_context(&path, "HEAD~1", "HEAD").unwrap();

        // The fixture's HEAD~1..HEAD range modifies `calculate` and `helper`
        // and adds `process`. All three must appear in the context response
        // — asserting on specific names guards against a mutation that
        // silently drops added or modified functions while still leaving
        // the vec non-empty.
        let names: std::collections::HashSet<&str> =
            result.functions.iter().map(|f| f.name.as_str()).collect();
        assert!(
            names.contains("calculate"),
            "calculate should be in function context, got: {:?}",
            names,
        );
        assert!(
            names.contains("helper"),
            "helper should be in function context, got: {:?}",
            names,
        );
        assert!(
            names.contains("process"),
            "process should be in function context, got: {:?}",
            names,
        );
    }

    #[test]
    fn it_finds_callers_in_other_files() {
        let (_dir, path) = create_context_test_repo();
        let result = build_function_context(&path, "HEAD~1", "HEAD").unwrap();

        let calculate_ctx = result
            .functions
            .iter()
            .find(|f| f.name == "calculate")
            .expect("calculate should have context");

        // main.rs calls calculate
        let has_main_caller = calculate_ctx
            .callers
            .iter()
            .any(|c| c.file.contains("main.rs"));
        assert!(
            has_main_caller,
            "calculate should have a caller in main.rs, got: {:?}",
            calculate_ctx.callers
        );
    }

    #[test]
    fn it_finds_test_references() {
        let (_dir, path) = create_context_test_repo();
        let result = build_function_context(&path, "HEAD~1", "HEAD").unwrap();

        let calculate_ctx = result
            .functions
            .iter()
            .find(|f| f.name == "calculate")
            .expect("calculate should have context");

        assert!(
            !calculate_ctx.test_references.is_empty(),
            "calculate should have test references, got: {:?}",
            calculate_ctx.test_references
        );
        assert!(
            calculate_ctx.test_references[0].file.contains("test"),
            "test reference should be in a test file"
        );
    }

    #[test]
    fn it_extracts_callees_for_changed_functions() {
        let (_dir, path) = create_context_test_repo();
        let result = build_function_context(&path, "HEAD~1", "HEAD").unwrap();

        let process_ctx = result
            .functions
            .iter()
            .find(|f| f.name == "process")
            .expect("process function context should be present");

        let callee_names: Vec<&str> = process_ctx
            .callees
            .iter()
            .map(|c| c.callee.as_str())
            .collect();
        assert!(
            callee_names.contains(&"calculate"),
            "process should call calculate, got: {:?}",
            callee_names
        );
    }

    #[test]
    fn it_includes_metadata() {
        let (_dir, path) = create_context_test_repo();
        let result = build_function_context(&path, "HEAD~1", "HEAD").unwrap();

        assert_eq!(result.metadata.base_ref, "HEAD~1");
        assert_eq!(result.metadata.head_ref, "HEAD");
        assert!(!result.metadata.base_sha.is_empty());
        assert!(!result.metadata.head_sha.is_empty());
    }

    #[test]
    fn it_reports_a_positive_token_estimate_for_a_non_trivial_context_response() {
        // Symmetric with manifest.rs::it_reports_a_positive_token_estimate_for_a_non_trivial_manifest.
        // The fixture repo produces several changed functions plus callers
        // and callees, so the serialized response is well over 4 characters
        // and the two-pass estimate must be strictly positive. Without this
        // test, a mutation that replaced the `estimate_response_tokens`
        // call in build_function_context with a hardcoded `0` would escape:
        // no other test reads metadata.token_estimate on the context path.
        // Exact value is not asserted because metadata.generated_at varies
        // at runtime; we only lock in the "wired and > 0" contract.
        let (_dir, path) = create_context_test_repo();
        let result = build_function_context(&path, "HEAD~1", "HEAD").unwrap();

        assert!(
            result.metadata.token_estimate > 0,
            "expected a positive token_estimate on a non-trivial function context response, got {}",
            result.metadata.token_estimate,
        );
    }

    #[test]
    fn leaf_function_name_extracts_simple() {
        assert_eq!(leaf_function_name("foo"), "foo");
    }

    #[test]
    fn leaf_function_name_extracts_from_scoped() {
        assert_eq!(leaf_function_name("std::collections::HashMap::new"), "new");
    }

    #[test]
    fn leaf_function_name_extracts_from_dotted() {
        assert_eq!(leaf_function_name("server.start"), "start");
    }

    #[test]
    fn is_test_path_detects_test_directories() {
        assert!(is_test_path("tests/test_lib.rs"));
        assert!(is_test_path("src/__tests__/foo.test.ts"));
        assert!(is_test_path("test/helper_test.go"));
    }

    #[test]
    fn is_test_path_detects_spec_directory() {
        assert!(is_test_path("spec/models/user_spec.rb"));
    }

    #[test]
    fn is_test_path_detects_go_test_suffix() {
        assert!(is_test_path("pkg/handler_test.go"));
    }

    #[test]
    fn is_test_path_detects_rust_test_suffix() {
        assert!(is_test_path("src/lib_test.rs"));
    }

    #[test]
    fn is_test_path_detects_dot_test_extensions() {
        assert!(is_test_path("src/components/Button.test.ts"));
        assert!(is_test_path("src/utils.test.js"));
        assert!(is_test_path("src/App.test.tsx"));
        assert!(is_test_path("src/App.test.jsx"));
    }

    #[test]
    fn is_test_path_detects_spec_rb_suffix() {
        assert!(is_test_path("models/user_spec.rb"));
    }

    #[test]
    fn is_test_path_detects_java_test_suffix() {
        assert!(is_test_path("src/UserTest.java"));
    }

    #[test]
    fn is_test_path_detects_csharp_tests_suffix() {
        assert!(is_test_path("src/UserTests.cs"));
    }

    #[test]
    fn is_test_path_detects_test_prefix_in_filename() {
        // The is_test_path function matches `test_` anywhere in the lowercased
        // path — including filenames like test_utils.py. This test documents
        // the current behaviour so a mutation that removes the `test_` arm is
        // caught immediately.
        assert!(is_test_path("src/test_utils.py"));
        assert!(is_test_path("test_helpers.go"));
    }

    #[test]
    fn is_test_path_rejects_production_paths() {
        assert!(!is_test_path("src/main.rs"));
        assert!(!is_test_path("src/server.py"));
        assert!(!is_test_path("pkg/handler.go"));
    }

    #[test]
    fn is_test_path_rejects_test_infix_in_filename() {
        // `test_` appearing mid-filename (e.g., `contest_utils`, `protest_utils`) must NOT
        // classify a path as a test — only a leading `test_` on the filename should count.
        assert!(!is_test_path("src/contest_utils.rs"));
        assert!(!is_test_path("src/protest_utils.py"));
    }

    #[test]
    fn is_test_path_rejects_path_with_test_only_in_component_name_not_matching_patterns() {
        // "latest" contains "test" as a substring but does not match any of
        // the is_test_path patterns (no leading `test_`, no `/test/` dir, etc.)
        assert!(!is_test_path("src/latest.rs"));
        assert!(!is_test_path("pkg/contest.go"));
    }

    // --- ContextOptions::default ---

    #[test]
    fn context_options_default_has_expected_values() {
        let opts = ContextOptions::default();
        assert!(opts.cursor.is_none());
        assert_eq!(opts.page_size, 25);
        assert!(opts.function_names.is_none());
        assert!(opts.max_response_tokens.is_none());
    }

    // --- resolve_next_cursor_offset ---

    #[test]
    fn next_cursor_resolution_both_none_returns_none() {
        assert_eq!(resolve_next_cursor_offset(None, None), None);
    }

    #[test]
    fn next_cursor_resolution_budget_only_returns_budget() {
        assert_eq!(resolve_next_cursor_offset(Some(7), None), Some(7));
    }

    #[test]
    fn next_cursor_resolution_page_only_returns_page() {
        assert_eq!(resolve_next_cursor_offset(None, Some(42)), Some(42));
    }

    #[test]
    fn next_cursor_resolution_budget_wins_when_smaller() {
        // Budget-induced cutoff lands earlier than the page-size rollover;
        // the follow-up call must resume at the budget cutoff so nothing is
        // skipped.
        assert_eq!(resolve_next_cursor_offset(Some(3), Some(10)), Some(3));
    }

    #[test]
    fn next_cursor_resolution_page_wins_when_smaller() {
        // Page-size rollover lands earlier than the budget cutoff; the
        // remaining entries are deferred to the next page rather than being
        // silently lost to the budget.
        assert_eq!(resolve_next_cursor_offset(Some(20), Some(5)), Some(5));
    }

    #[test]
    fn it_returns_some_when_both_cutoffs_are_equal() {
        // When the budget cutoff and the page cutoff land on the same index
        // the function must still emit a next_cursor — the two constraints
        // happen to agree rather than cancel.
        assert_eq!(resolve_next_cursor_offset(Some(5), Some(5)), Some(5));
    }

    // --- clamp_entry_lists ---

    fn make_clamp_fixture_entry(
        name: &str,
        caller_count: usize,
        callee_count: usize,
        test_ref_count: usize,
    ) -> FunctionContextEntry {
        let callers = (0..caller_count)
            .map(|i| CallerEntry {
                file: format!("src/caller_{i}.rs"),
                line: i + 1,
                caller: format!("caller_fn_{i}"),
                is_test: false,
            })
            .collect();
        let callees = (0..callee_count)
            .map(|i| CalleeEntry {
                callee: format!("callee_{i}"),
                line: i + 1,
            })
            .collect();
        let test_references = (0..test_ref_count)
            .map(|i| CallerEntry {
                file: format!("tests/test_{i}.rs"),
                line: i + 1,
                caller: format!("test_caller_{i}"),
                is_test: true,
            })
            .collect();
        FunctionContextEntry {
            name: name.to_string(),
            file: "src/lib.rs".to_string(),
            change_type: FunctionChangeType::Modified,
            blast_radius: BlastRadius::compute(caller_count, test_ref_count),
            scoping_mode: ScopingMode::Fallback,
            callers,
            callees,
            test_references,
            caller_count: caller_count + test_ref_count,
            truncated: false,
        }
    }

    #[test]
    fn clamp_entry_lists_truncates_callers_to_five() {
        let mut entry = make_clamp_fixture_entry("calc", 8, 2, 1);
        let original_caller_count = entry.caller_count;
        clamp_entry_lists(&mut entry, 5, 5, 3);
        assert_eq!(entry.callers.len(), 5);
        assert_eq!(entry.callees.len(), 2);
        assert_eq!(entry.test_references.len(), 1);
        assert_eq!(
            entry.caller_count, original_caller_count,
            "clamp_entry_lists must not modify caller_count"
        );
    }

    #[test]
    fn clamp_entry_lists_truncates_callees_to_five() {
        let mut entry = make_clamp_fixture_entry("calc", 2, 8, 1);
        let original_caller_count = entry.caller_count;
        clamp_entry_lists(&mut entry, 5, 5, 3);
        assert_eq!(entry.callers.len(), 2);
        assert_eq!(entry.callees.len(), 5);
        assert_eq!(entry.test_references.len(), 1);
        assert_eq!(
            entry.caller_count, original_caller_count,
            "clamp_entry_lists must not modify caller_count"
        );
    }

    #[test]
    fn clamp_entry_lists_truncates_test_refs_to_three() {
        let mut entry = make_clamp_fixture_entry("calc", 1, 1, 6);
        let original_caller_count = entry.caller_count;
        clamp_entry_lists(&mut entry, 5, 5, 3);
        assert_eq!(entry.callers.len(), 1);
        assert_eq!(entry.callees.len(), 1);
        assert_eq!(entry.test_references.len(), 3);
        assert_eq!(
            entry.caller_count, original_caller_count,
            "clamp_entry_lists must not modify caller_count"
        );
    }

    #[test]
    fn clamp_entry_lists_leaves_under_cap_entries_unchanged() {
        let mut entry = make_clamp_fixture_entry("calc", 5, 5, 3);
        clamp_entry_lists(&mut entry, 5, 5, 3);
        assert_eq!(entry.callers.len(), 5);
        assert_eq!(entry.callees.len(), 5);
        assert_eq!(entry.test_references.len(), 3);
    }

    // --- budget enforcement through the public API ---

    fn create_many_callers_repo() -> (TempDir, std::path::PathBuf) {
        // Build a fixture where `calculate` is called from many production
        // files, so the function_context response for HEAD~1..HEAD carries a
        // long callers list and exercises the budget clamp. A `Cargo.toml`
        // is included so RepoContext picks up the crate name and the
        // `use crate::lib::calculate` imports resolve correctly, letting
        // caller discovery find all 10 call sites rather than zero.
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

        std::fs::write(
            path.join("Cargo.toml"),
            "[package]\nname = \"budget_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(path.join("src")).unwrap();
        // Eight changed functions (calc_0..calc_7) so the page slice has
        // enough entries that a tight budget has to drop some of them,
        // triggering both the clamped-list path and the page-rollover
        // next_cursor marker.
        let initial_lib: String = (0..8)
            .map(|i| format!("pub fn calc_{i}(x: i32) -> i32 {{\n    x + {i}\n}}\n\n"))
            .collect();
        std::fs::write(path.join("src/lib.rs"), initial_lib).unwrap();
        // Several call sites per function so the per-entry cost is large
        // enough to trip the budget once more than one entry is on the page.
        for i in 0..8 {
            std::fs::write(
                path.join(format!("src/caller_{i}.rs")),
                format!(
                    "use crate::calc_{i};\n\n\
                     pub fn caller_a_{i}() {{ let _ = calc_{i}({i}); }}\n\
                     pub fn caller_b_{i}() {{ let _ = calc_{i}({i} + 1); }}\n\
                     pub fn caller_c_{i}() {{ let _ = calc_{i}({i} + 2); }}\n"
                ),
            )
            .unwrap();
        }

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

        // Modify every calc_N so they all appear in the manifest.
        let modified_lib: String = (0..8)
            .map(|i| format!("pub fn calc_{i}(x: i32) -> i32 {{\n    x + {i} + x\n}}\n\n"))
            .collect();
        std::fs::write(path.join("src/lib.rs"), modified_lib).unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "modify all calc_N"])
            .current_dir(&path)
            .output()
            .unwrap();

        (dir, path)
    }

    #[test]
    fn it_returns_full_entries_when_budget_is_large() {
        let (_dir, path) = create_many_callers_repo();
        let options = ContextOptions {
            cursor: None,
            page_size: 25,
            function_names: None,
            max_response_tokens: Some(100_000),
        };
        let result =
            build_function_context_with_options(&path, "HEAD~1", "HEAD", &options).unwrap();

        // Nothing should be marked truncated at a very loose budget.
        assert!(
            result.metadata.function_analysis_truncated.is_empty(),
            "expected no truncated entries at 100k-token budget, got: {:?}",
            result.metadata.function_analysis_truncated,
        );
        assert!(
            result.metadata.next_cursor.is_none(),
            "expected no next_cursor at 100k-token budget, got: {:?}",
            result.metadata.next_cursor,
        );
    }

    #[test]
    fn it_clamps_entries_when_budget_is_tight() {
        let (_dir, path) = create_many_callers_repo();
        // Pre-flight: compute the unbounded response size so the test can
        // pick a budget that's guaranteed to be tight without hardcoding a
        // fragile number. If the fixture ever grows / shrinks, this stays
        // correct. A budget well below the unbounded estimate forces the
        // response to drop functions on the page, which in turn populates
        // `function_analysis_truncated` via the page-rollover marker.
        let unbounded_opts = ContextOptions {
            cursor: None,
            page_size: 25,
            function_names: None,
            max_response_tokens: Some(100_000),
        };
        let unbounded =
            build_function_context_with_options(&path, "HEAD~1", "HEAD", &unbounded_opts).unwrap();
        assert!(
            !unbounded.functions.is_empty(),
            "fixture must produce at least one function entry for the budget test to be meaningful",
        );
        // Intentionally ask for a tiny slice of the unbounded response so the
        // enforce_context_token_budget greedy walk must drop entries and
        // produce a next_cursor. Aim at ~70% of the unbounded estimate: big
        // enough that the skeleton + safety margin doesn't eat it all (which
        // would zero out entry_budget and drop EVERY entry silently), small
        // enough that the walk hits a clamp/drop threshold on at least one
        // entry with enough survivors that the "last kept entry" marker
        // still fires.
        let tight_budget = (unbounded.metadata.token_estimate * 7 / 10).max(400);

        let options = ContextOptions {
            cursor: None,
            page_size: 25,
            function_names: None,
            max_response_tokens: Some(tight_budget),
        };
        let result =
            build_function_context_with_options(&path, "HEAD~1", "HEAD", &options).unwrap();

        assert!(
            !result.metadata.function_analysis_truncated.is_empty(),
            "expected at least one truncated entry at a {tight_budget}-token budget \
             (unbounded estimate: {}, unbounded function count: {}), got none",
            unbounded.metadata.token_estimate,
            unbounded.functions.len(),
        );
        // At least one entry must carry the truncated flag so agents see the
        // budget pressure at the wire level, not just in metadata.
        assert!(
            result.functions.iter().any(|f| f.truncated),
            "expected at least one FunctionContextEntry.truncated=true at tight budget",
        );
        // Budget enforcement must actually drop entries, not just set flags.
        // A mutation that set `truncated: true` without shrinking the entry
        // list would otherwise slip past the truncation assertions above.
        assert!(
            result.functions.len() < unbounded.functions.len(),
            "budget enforcement must reduce entry count: got {} entries, unbounded had {}",
            result.functions.len(),
            unbounded.functions.len(),
        );
    }

    #[test]
    fn it_skips_budget_when_max_response_tokens_is_zero() {
        // `Some(0)` is the "disabled" sentinel on the internal
        // ContextOptions — it must bypass enforcement entirely, just like
        // `None` does. Without this guard a zero budget would drop every
        // entry and produce a permanently empty response.
        let (_dir, path) = create_many_callers_repo();
        let options = ContextOptions {
            cursor: None,
            page_size: 25,
            function_names: None,
            max_response_tokens: Some(0),
        };
        let result =
            build_function_context_with_options(&path, "HEAD~1", "HEAD", &options).unwrap();

        assert!(
            result.metadata.function_analysis_truncated.is_empty(),
            "expected no truncated entries when max_response_tokens=Some(0), got: {:?}",
            result.metadata.function_analysis_truncated,
        );
        assert!(
            !result.functions.is_empty(),
            "expected the response to still carry entries when budget is disabled",
        );
    }

    // --- enforce_context_token_budget arithmetic and clamp boundary mutants ---
    //
    // These tests target surviving mutants reported by cargo-mutants
    // (issue #224 / CI run 24917104987) in `enforce_context_token_budget`
    // and `clamp_entry_lists`:
    //   * `(budget / 4).max(128)` → `(budget % 4).max(128)` — safety margin
    //     uses division, not modulo
    //   * `remaining -= cost.full`   → `/=`         (full-fit branch)
    //   * `remaining -= cost.clamped` → `+=` or `/=` (clamped-fit branch)
    //   * `entry.callers.len()         > max_callers`  → `>=`
    //   * `entry.callees.len()         > max_callees`  → `>=`
    //   * `entry.test_references.len() > max_test_refs` → `>=`
    //
    // Mirrors the manifest-side tests added in PR #223 for the analogous
    // `enforce_token_budget` mutants.
    //
    // Equivalent-mutant note: the three `> → >=` mutants on lines 600,
    // 603, 606 are mathematically equivalent — `Vec::truncate(N)` on a
    // Vec of length N is documented as a no-op, so for `len == cap` both
    // operators produce identical output (skip vs no-op-truncate). For
    // `len < cap` and `len > cap` both operators agree. Cargo-mutants
    // surfaces them because no test fails under the mutation, but no
    // black-box test of `clamp_entry_lists` can distinguish the two. The
    // boundary tests below pin the contract at exactly `len == cap`
    // (skip) and `len == cap + 1` (truncate to cap) so any future change
    // that makes the operation non-idempotent — or that shifts the
    // threshold by one — fails immediately.
    //
    // The arithmetic mutants use synthetic FunctionContextResponse fixtures so
    // the test exercises `enforce_context_token_budget` directly without
    // standing up a git repo — much faster to triangulate budget arithmetic.

    fn make_context_entry_with_lists(
        name: &str,
        caller_count: usize,
        callee_count: usize,
        test_ref_count: usize,
    ) -> FunctionContextEntry {
        // Same shape as `make_clamp_fixture_entry` but with longer file/caller
        // names so each entry's serialized cost is in the hundreds of tokens —
        // enough that a tight budget can clamp or drop it.
        let callers = (0..caller_count)
            .map(|i| CallerEntry {
                file: format!("src/very/deeply/nested/module/path/caller_{name}_{i}.rs"),
                line: i * 7 + 1,
                caller: format!("caller_function_named_{name}_index_{i}"),
                is_test: false,
            })
            .collect();
        let callees = (0..callee_count)
            .map(|i| CalleeEntry {
                callee: format!("descriptively_named_callee_{name}_{i}"),
                line: i * 11 + 1,
            })
            .collect();
        let test_references = (0..test_ref_count)
            .map(|i| CallerEntry {
                file: format!("tests/integration/test_module_{name}_{i}.rs"),
                line: i * 13 + 1,
                caller: format!("test_caller_{name}_{i}"),
                is_test: true,
            })
            .collect();
        FunctionContextEntry {
            name: name.to_string(),
            file: format!("src/lib_{name}.rs"),
            change_type: FunctionChangeType::Modified,
            blast_radius: BlastRadius::compute(caller_count, test_ref_count),
            scoping_mode: ScopingMode::Fallback,
            callers,
            callees,
            test_references,
            caller_count: caller_count + test_ref_count,
            truncated: false,
        }
    }

    fn make_context_response(entries: Vec<FunctionContextEntry>) -> FunctionContextResponse {
        let count = entries.len();
        FunctionContextResponse {
            metadata: ContextMetadata {
                base_ref: "HEAD~1".to_string(),
                head_ref: "HEAD".to_string(),
                base_sha: "0000000000000000000000000000000000000000".to_string(),
                head_sha: "1111111111111111111111111111111111111111".to_string(),
                generated_at: Utc::now(),
                token_estimate: 0,
                function_analysis_truncated: vec![],
                next_cursor: None,
            },
            functions: entries,
            pagination: PaginationInfo {
                total_items: count,
                page_start: 0,
                page_size: count,
                next_cursor: None,
            },
        }
    }

    #[test]
    fn it_uses_division_not_modulo_for_safety_margin() {
        // The safety_margin in enforce_context_token_budget is computed as
        // `(budget / 4).max(128)`. A mutant that replaces `/` with `%`
        // computes `budget % 4` — always 0..=3 — which then clamps to 128.
        // The `.max(128)` clamp masks the mutation for any budget < 512;
        // beyond that, the real margin grows with the budget while the
        // mutant margin stays at 128. The test must use a budget large
        // enough that `budget/4 > 128` to make the mutation observable.
        //
        // Strategy: build a response where total full cost lives in a
        // sweet spot such that `(budget/4).max(128)` forces a trim but
        // `(budget%4).max(128) == 128` leaves everything fitting full.
        let entries = (0..8)
            .map(|i| make_context_entry_with_lists(&format!("calc_{i}"), 3, 3, 2))
            .collect::<Vec<_>>();
        let mut response = make_context_response(entries);

        // Measure skeleton + per-entry full cost the same way
        // enforce_context_token_budget does internally.
        let skeleton_cost = {
            let saved = std::mem::take(&mut response.functions);
            let cost = size::estimate_response_tokens(&response);
            response.functions = saved;
            cost
        };
        let total_full: usize = response
            .functions
            .iter()
            .map(size::estimate_response_tokens)
            .sum();

        // Solve for budget so:
        //     budget - skeleton - (budget/4) < total_full   (real → trim)
        //     budget - skeleton - 128        >= total_full  (mutant → no trim)
        // i.e. budget >= total_full + skeleton + 128
        //  and budget * 3/4 < total_full + skeleton
        //         → budget < (total_full + skeleton) * 4 / 3
        //
        // Pick the midpoint of that range. Cap the lower bound at 513 so
        // `budget / 4 > 128` (mutation actually observable).
        let target = total_full + skeleton_cost;
        let lower = (target + 128).max(513);
        let upper = target * 4 / 3; // exclusive upper bound
        assert!(
            upper > lower,
            "fixture must be large enough for the safety-margin sweet spot to exist: \
             target={target} skeleton={skeleton_cost} total_full={total_full} \
             lower={lower} upper={upper}",
        );
        let budget = (lower + upper) / 2;

        // Pre-flight: under the correct `/`, file_budget < total_full
        // (trim required); under the mutant `%`, file_budget >= total_full
        // (no trim).
        let margin_div = (budget / 4).max(128);
        let margin_mod = (budget % 4).max(128);
        assert!(
            margin_div > margin_mod,
            "test presumes (budget/4).max(128) > (budget%4).max(128); \
             budget={budget} /4-margin={margin_div} %4-margin={margin_mod}",
        );
        let entry_budget_under_div = budget
            .saturating_sub(skeleton_cost)
            .saturating_sub(margin_div);
        let entry_budget_under_mod = budget
            .saturating_sub(skeleton_cost)
            .saturating_sub(margin_mod);
        assert!(
            entry_budget_under_div < total_full,
            "test construction invalid: under correct `/`, entry_budget \
             ({entry_budget_under_div}) must be < total_full ({total_full}) \
             so trimming is required. \
             budget={budget} skeleton={skeleton_cost} margin_div={margin_div}",
        );
        assert!(
            entry_budget_under_mod >= total_full,
            "test construction invalid: under mutant `%`, entry_budget \
             ({entry_budget_under_mod}) must be >= total_full ({total_full}) \
             so no trimming occurs. \
             budget={budget} skeleton={skeleton_cost} margin_mod={margin_mod}",
        );

        let original_entries = response.functions.len();
        let (clamped_names, local_cutoff) = enforce_context_token_budget(&mut response, budget);

        // Under correct `/`: at least one entry was clamped or dropped — i.e.
        // either clamped_names is non-empty or the cutoff fired (entries
        // truncated). Under mutant `%`: total_full <= entry_budget so the
        // function returns (Vec::new(), None) without touching anything.
        let some_trimming = !clamped_names.is_empty()
            || local_cutoff.is_some()
            || response.functions.iter().any(|f| f.truncated)
            || response.functions.len() < original_entries;
        assert!(
            some_trimming,
            "with budget {budget} (skeleton={skeleton_cost}, \
             total_full={total_full}) the (budget/4).max(128) safety margin \
             of {margin_div} tokens must force trimming; a modulo-based margin \
             clamps to {margin_mod} (=128) and leaves room for every entry. \
             clamped_names={clamped_names:?} local_cutoff={local_cutoff:?} \
             surviving_entries={}",
            response.functions.len(),
        );
    }

    #[test]
    fn it_decreases_remaining_budget_after_each_clamped_fit_decision() {
        // Targets `remaining -= cost.clamped` → `+=` (line 572). The full
        // branch (line 568) also uses `-=` so under any `+=` mutation
        // the SAME fixture must distinguish.
        //
        // Strategy: make `cost.full ≫ cost.clamped` by stuffing each entry
        // with very long caller / callee / test-ref lists. Pick budget so
        // that:
        //   * No entry fits at full ever (under correct `-=` OR `+=`):
        //     even after `+=` accumulates ALL clamped costs, remaining
        //     stays below one_full. This pins line 568 off — so the only
        //     active branch is line 572.
        //   * Several entries fit clamped; the rest must drop.
        //
        // With line 568 inert, the `+=` mutation on line 572 turns drops
        // into clamps (every entry survives clamped). Under correct `-=`,
        // drops happen. Assertion: at least one entry was dropped.
        let entries = (0..6)
            .map(|i| make_context_entry_with_lists(&format!("calc_{i}"), 100, 100, 50))
            .collect::<Vec<_>>();
        let mut response = make_context_response(entries);
        let skeleton_cost = {
            let saved = std::mem::take(&mut response.functions);
            let cost = size::estimate_response_tokens(&response);
            response.functions = saved;
            cost
        };
        let one_full = size::estimate_response_tokens(&response.functions[0]);
        let one_clamped = {
            let mut clone = response.functions[0].clone();
            clamp_entry_lists(&mut clone, 5, 5, 3);
            size::estimate_response_tokens(&clone)
        };
        let n = response.functions.len();

        // Target entry_budget ≈ 3 * one_clamped so 3 clamped entries fit,
        // 3 must drop. Critically, we also need `one_full > entry_budget +
        // n * one_clamped` so even after a `+=` mutant grows remaining by
        // the maximum possible amount (every entry clamps), remaining
        // never reaches one_full — line 568 stays dormant.
        let target_entry_budget = 3 * one_clamped;
        let budget = ((skeleton_cost + target_entry_budget) * 4).div_ceil(3);

        let safety_margin = (budget / 4).max(128);
        let entry_budget = budget
            .saturating_sub(skeleton_cost)
            .saturating_sub(safety_margin);
        // Pre-flight 1: one_full > entry_budget (line 568 inert at start).
        assert!(
            one_full > entry_budget,
            "construction invalid: one_full must exceed entry_budget so \
             line 568 doesn't fire on the first entry; \
             one_full={one_full} entry_budget={entry_budget}",
        );
        // Pre-flight 2: even under `+=`, remaining can never reach
        // one_full. Worst case `+=` accumulates `n * one_clamped` on top
        // of entry_budget. If `entry_budget + n * one_clamped < one_full`
        // line 568 stays inert under both `-=` and `+=`.
        let max_grown_remaining = entry_budget + n * one_clamped;
        assert!(
            max_grown_remaining < one_full,
            "construction invalid: under `+=` mutation, remaining can grow \
             to entry_budget + n*one_clamped = {max_grown_remaining}, which \
             must stay below one_full = {one_full} so line 568 never fires \
             (otherwise the walk's behavior under the mutation gets \
             complicated by full-fits taking over)",
        );
        // Pre-flight 3: budget must admit ≥2 clamped entries.
        assert!(
            entry_budget >= 2 * one_clamped,
            "construction invalid: budget must admit ≥2 clamped entries; \
             entry_budget={entry_budget} one_clamped={one_clamped}",
        );
        // Pre-flight 4: total clamped cost must exceed entry_budget so
        // drops happen under correct `-=`.
        let total_clamped = n * one_clamped;
        assert!(
            total_clamped > entry_budget,
            "construction invalid: total clamped cost must exceed budget; \
             total_clamped={total_clamped} entry_budget={entry_budget}",
        );

        let original_entries = response.functions.len();
        let (clamped_names, _) = enforce_context_token_budget(&mut response, budget);
        let dropped = original_entries - response.functions.len();

        // Under correct `-=`: budget drains, eventually a drop fires.
        // Under `+=` on line 572: remaining never reaches one_full (by
        // construction), so line 568 stays inert. But each clamp grows
        // remaining instead of shrinking it — so `cost.clamped <=
        // remaining` keeps passing for every entry. Result: all entries
        // clamp, none drop.
        assert!(
            !clamped_names.is_empty(),
            "fixture must trigger the clamped branch at least once; \
             clamped_names={clamped_names:?}",
        );
        assert!(
            dropped >= 1,
            "with a budget that admits only ~3 clamped entries out of {n}, \
             enforcement must DROP at least one entry once `remaining` is \
             drained. If every entry survives clamped, `remaining` is being \
             increased instead of decreased (mutant: \
             `remaining += cost.clamped`). \
             dropped={dropped} clamped_names={clamped_names:?} \
             entry_budget={entry_budget} one_clamped={one_clamped} \
             one_full={one_full}",
        );
    }

    #[test]
    fn it_does_not_collapse_remaining_after_first_clamped_decision() {
        // Targets `remaining -= cost.clamped` → `/=` (line 572). Under
        // `/=`, after the first clamp `remaining` becomes
        // `remaining / cost.clamped` (typically 1 or 0). The next entry's
        // `cost.clamped <= remaining` check fails → drop. Result: only
        // ONE entry is clamped, the rest drop. Under correct `-=`, several
        // entries clamp before the budget drains.
        //
        // Same construction strategy as the `+=` test: every entry hits
        // the clamped branch (one_full ≫ entry_budget + n*one_clamped so
        // line 568 stays inert). Assertion is on the OTHER direction:
        // under `/=`, far fewer entries are clamped.
        let entries = (0..6)
            .map(|i| make_context_entry_with_lists(&format!("calc_{i}"), 100, 100, 50))
            .collect::<Vec<_>>();
        let mut response = make_context_response(entries);
        let skeleton_cost = {
            let saved = std::mem::take(&mut response.functions);
            let cost = size::estimate_response_tokens(&response);
            response.functions = saved;
            cost
        };
        let one_full = size::estimate_response_tokens(&response.functions[0]);
        let one_clamped = {
            let mut clone = response.functions[0].clone();
            clamp_entry_lists(&mut clone, 5, 5, 3);
            size::estimate_response_tokens(&clone)
        };
        let n = response.functions.len();

        // Same budget shape as the `+=` test: at least 3 clamped entries
        // fit, no full entries fit. The two-or-more-clamped invariant is
        // what kills `/=` because it collapses to one clamped + drops.
        let target_entry_budget = 3 * one_clamped;
        let budget = ((skeleton_cost + target_entry_budget) * 4).div_ceil(3);

        let safety_margin = (budget / 4).max(128);
        let entry_budget = budget
            .saturating_sub(skeleton_cost)
            .saturating_sub(safety_margin);
        assert!(
            one_full > entry_budget,
            "construction invalid: line 568 must NOT fire on entry 0; \
             one_full={one_full} entry_budget={entry_budget}",
        );
        // Belt-and-suspenders: even under any monotonically-non-decreasing
        // mutation of remaining, the value can't reach one_full so line
        // 568 stays dormant for the whole walk.
        let max_grown_remaining = entry_budget + n * one_clamped;
        assert!(
            max_grown_remaining < one_full,
            "construction invalid: max_grown_remaining={max_grown_remaining} \
             must stay below one_full={one_full}",
        );
        assert!(
            entry_budget >= 3 * one_clamped,
            "construction invalid: at least 3 clamped entries must fit \
             under correct `-=` so a `/=` mutant (which clamps only 1) is \
             distinguishable; entry_budget={entry_budget} \
             one_clamped={one_clamped}",
        );

        let (clamped_names, _) = enforce_context_token_budget(&mut response, budget);

        // Under correct `-=`: at least 3 entries clamp.
        // Under `/=` on line 572: after the first clamp, remaining ≈ 1, so
        // every subsequent entry drops. Only 1 entry ends up clamped.
        // Under `/=` on line 568: line 568 never fires here, so this
        // mutant survives this fixture — the parallel "full path" test
        // (`it_does_not_collapse_remaining_to_one_after_first_decision`)
        // covers it.
        assert!(
            clamped_names.len() >= 2,
            "with a budget that admits 3+ clamped entries under correct \
             `-=`, at least two entries must clamp. If only one clamps, \
             `remaining` collapsed to ~1 after the first clamped decision \
             (mutant: `remaining /= cost.clamped`). \
             clamped_names={clamped_names:?} entry_budget={entry_budget} \
             one_clamped={one_clamped}",
        );
    }

    #[test]
    fn it_does_not_collapse_remaining_to_one_after_first_decision() {
        // Targets `remaining -= cost.full` → `/=` (line 568). Under `/=`,
        // after the first full-fit `remaining` becomes
        // `remaining / cost.full` (typically 1 or 0). The next entry's
        // `cost.full <= remaining` check fails, and `cost.clamped <=
        // remaining` also fails — so EVERY subsequent entry drops. Only
        // one entry survives.
        //
        // Construct a budget that admits at least 3 entries at full under
        // correct `-=`. Under `/=` only 1 survives.
        let entries = (0..5)
            .map(|i| make_context_entry_with_lists(&format!("calc_{i}"), 4, 4, 2))
            .collect::<Vec<_>>();
        let mut response = make_context_response(entries);
        let skeleton_cost = {
            let saved = std::mem::take(&mut response.functions);
            let cost = size::estimate_response_tokens(&response);
            response.functions = saved;
            cost
        };
        let one_full = size::estimate_response_tokens(&response.functions[0]);
        let total_full: usize = response
            .functions
            .iter()
            .map(size::estimate_response_tokens)
            .sum();

        // Aim for entry_budget == 4 * one_full so 4 of 5 entries fit at
        // full under correct `-=`. Under `/=`, after entry 0 (cost = one_full)
        // remaining becomes ~1, and entries 1..=4 are all dropped.
        let target_entry_budget = 4 * one_full;
        let budget = ((skeleton_cost + target_entry_budget) * 4).div_ceil(3);

        let safety_margin = (budget / 4).max(128);
        let entry_budget = budget
            .saturating_sub(skeleton_cost)
            .saturating_sub(safety_margin);
        // Pre-flight: greedy walk must run.
        assert!(
            total_full > entry_budget,
            "construction invalid: greedy walk must execute; \
             total_full={total_full} entry_budget={entry_budget} \
             budget={budget} skeleton={skeleton_cost} one_full={one_full}",
        );
        // Pre-flight: under correct `-=`, at least 3 entries fit at full.
        assert!(
            entry_budget >= 3 * one_full,
            "construction invalid: at least three entries must fit at full \
             under correct `-=` so a `/=` mutant (which leaves only one) is \
             distinguishable; entry_budget={entry_budget} one_full={one_full}",
        );

        let original_entries = response.functions.len();
        let _ = enforce_context_token_budget(&mut response, budget);

        // Under correct `-=`: at least 2 entries survive at full.
        // Under `/=` on line 568: only 1 entry survives.
        let surviving = response.functions.len();
        assert!(
            surviving >= 2,
            "with a budget that admits 3+ entries at full under correct `-=`, \
             at least two entries must survive. If only one survives, \
             `remaining` collapsed to ~1 after the first full-fit decision \
             (mutant: `remaining /= cost.full`). \
             surviving={surviving} original={original_entries} \
             entry_budget={entry_budget} one_full={one_full}",
        );
    }

    #[test]
    fn it_leaves_callers_at_exact_cap_unchanged() {
        // Targets the `entry.callers.len() > max_callers` → `>=` mutant on
        // line 600. At len == cap the `>` branch is false (no truncate);
        // documenting this boundary explicitly future-proofs the contract
        // against any change that makes truncation non-idempotent.
        //
        // Pair test: same fixture run with len = cap + 1 must truncate to
        // cap. Together they pin the threshold exactly at `> max_callers`.
        let cap = 5;

        // Boundary 1: len == cap. Under both `>` and `>=` the resulting
        // length is `cap` (truncate to cap is a no-op when len == cap),
        // but the caller list contents must still be the original list —
        // not a clone, not reversed, not partial.
        let mut at_cap = make_clamp_fixture_entry("at_cap", cap, 0, 0);
        let original_callers: Vec<String> =
            at_cap.callers.iter().map(|c| c.caller.clone()).collect();
        clamp_entry_lists(&mut at_cap, cap, cap, 3);
        assert_eq!(
            at_cap.callers.len(),
            cap,
            "callers.len() at exact cap must remain {cap}",
        );
        let after_callers: Vec<String> = at_cap.callers.iter().map(|c| c.caller.clone()).collect();
        assert_eq!(
            after_callers, original_callers,
            "callers list at exact cap must be unchanged in content and order",
        );

        // Boundary 2: len == cap + 1. Truncation MUST fire and reduce to cap.
        let mut over_cap = make_clamp_fixture_entry("over_cap", cap + 1, 0, 0);
        clamp_entry_lists(&mut over_cap, cap, cap, 3);
        assert_eq!(
            over_cap.callers.len(),
            cap,
            "callers.len() at cap+1 must be truncated to {cap}",
        );
    }

    #[test]
    fn it_leaves_callees_at_exact_cap_unchanged() {
        // Targets the `entry.callees.len() > max_callees` → `>=` mutant on
        // line 603. Symmetric to the callers boundary test above.
        let cap = 5;

        let mut at_cap = make_clamp_fixture_entry("at_cap", 0, cap, 0);
        let original_callees: Vec<String> =
            at_cap.callees.iter().map(|c| c.callee.clone()).collect();
        clamp_entry_lists(&mut at_cap, cap, cap, 3);
        assert_eq!(
            at_cap.callees.len(),
            cap,
            "callees.len() at exact cap must remain {cap}",
        );
        let after_callees: Vec<String> = at_cap.callees.iter().map(|c| c.callee.clone()).collect();
        assert_eq!(
            after_callees, original_callees,
            "callees list at exact cap must be unchanged in content and order",
        );

        let mut over_cap = make_clamp_fixture_entry("over_cap", 0, cap + 1, 0);
        clamp_entry_lists(&mut over_cap, cap, cap, 3);
        assert_eq!(
            over_cap.callees.len(),
            cap,
            "callees.len() at cap+1 must be truncated to {cap}",
        );
    }

    #[test]
    fn it_leaves_test_references_at_exact_cap_unchanged() {
        // Targets the `entry.test_references.len() > max_test_refs` → `>=`
        // mutant on line 606. Test-references cap is 3 (not 5 like the
        // other two), so this test uses cap=3 to land exactly on the
        // boundary.
        let cap = 3;

        let mut at_cap = make_clamp_fixture_entry("at_cap", 0, 0, cap);
        let original_test_refs: Vec<String> = at_cap
            .test_references
            .iter()
            .map(|c| c.caller.clone())
            .collect();
        clamp_entry_lists(&mut at_cap, 5, 5, cap);
        assert_eq!(
            at_cap.test_references.len(),
            cap,
            "test_references.len() at exact cap must remain {cap}",
        );
        let after_test_refs: Vec<String> = at_cap
            .test_references
            .iter()
            .map(|c| c.caller.clone())
            .collect();
        assert_eq!(
            after_test_refs, original_test_refs,
            "test_references list at exact cap must be unchanged in content and order",
        );

        let mut over_cap = make_clamp_fixture_entry("over_cap", 0, 0, cap + 1);
        clamp_entry_lists(&mut over_cap, 5, 5, cap);
        assert_eq!(
            over_cap.test_references.len(),
            cap,
            "test_references.len() at cap+1 must be truncated to {cap}",
        );
    }
}
