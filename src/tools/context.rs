use std::path::Path;

use chrono::Utc;

use crate::git::reader::RepoReader;
use crate::pagination::{
    FUNCTION_CURSOR_VERSION, FunctionPaginationCursor, PaginationInfo, clamp_page_size,
    decode_function_cursor, encode_function_cursor, validate_function_cursor,
};
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

fn extension_from_path(path: &str) -> &str {
    path.rsplit('.')
        .next()
        .filter(|ext| path.len() > ext.len() + 1)
        .unwrap_or("")
}

fn is_test_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("/__tests__/")
        || lower.contains("/spec/")
        || lower.ends_with("_test.go")
        || lower.ends_with("_test.rs")
        || lower.contains("test_")
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
            Err(_) => continue,
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
                    .unwrap_or_default();
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
                .unwrap_or_default();
            let functions = analyzer
                .extract_functions(content.as_bytes())
                .unwrap_or_default();
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
    let next_cursor_offset = match (budget_cutoff, page_cutoff) {
        (Some(b), Some(p)) => Some(b.min(p)),
        (Some(b), None) => Some(b),
        (None, Some(p)) => Some(p),
        (None, None) => None,
    };
    if let Some(offset) = next_cursor_offset {
        let cursor = FunctionPaginationCursor {
            v: FUNCTION_CURSOR_VERSION,
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
    if entry.callers.len() > max_callers {
        entry.callers.truncate(max_callers);
    }
    if entry.callees.len() > max_callees {
        entry.callees.truncate(max_callees);
    }
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
        Err(_) => return vec![],
    };
    let functions = analyzer
        .extract_functions(content.as_bytes())
        .unwrap_or_default();
    let calls = analyzer
        .extract_calls(content.as_bytes())
        .unwrap_or_default();

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
        assert!(!result.functions.is_empty());
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

        let process_ctx = result.functions.iter().find(|f| f.name == "process");

        if let Some(ctx) = process_ctx {
            let callee_names: Vec<&str> = ctx.callees.iter().map(|c| c.callee.as_str()).collect();
            assert!(
                callee_names.contains(&"calculate"),
                "process should call calculate, got: {:?}",
                callee_names
            );
        }
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
    fn is_test_path_rejects_production_paths() {
        assert!(!is_test_path("src/main.rs"));
        assert!(!is_test_path("src/server.py"));
        assert!(!is_test_path("pkg/handler.go"));
    }
}
