use std::path::PathBuf;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};

use crate::git::diff::ChangeScope;
use crate::tools::{
    ContextArgs, FunctionContextResponse, HistoryArgs, HistoryResponse, ManifestArgs,
    ManifestOptions, ManifestResponse, SnapshotArgs, SnapshotOptions, SnapshotResponse,
    build_function_context, build_history, build_manifest, build_snapshots,
    build_worktree_manifest,
};

/// Convert a `ChangeScope` variant to a static metric label string.
fn change_scope_label(scope: ChangeScope) -> &'static str {
    match scope {
        ChangeScope::Committed => "committed",
        ChangeScope::Staged => "staged",
        ChangeScope::Unstaged => "unstaged",
    }
}

/// Group changed-function context entries by the language of their containing
/// file, returning one count per known language. Entries whose language cannot
/// be detected from the extension are excluded to keep metric label cardinality
/// bounded and consistent with the manifest tool's `functions_changed` signal.
fn functions_per_language_counts(
    entries: &[crate::tools::types::FunctionContextEntry],
) -> std::collections::HashMap<&'static str, u64> {
    let mut counts: std::collections::HashMap<&'static str, u64> = std::collections::HashMap::new();
    for entry in entries {
        let language = crate::tools::types::detect_language(&entry.file);
        if language != "unknown" {
            *counts.entry(language).or_insert(0) += 1;
        }
    }
    counts
}

#[derive(Debug, Clone)]
pub struct GitPrismServer {
    tool_router: ToolRouter<Self>,
}

impl GitPrismServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

impl Default for GitPrismServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl GitPrismServer {
    /// Returns structured metadata about what changed between two git refs,
    /// including file changes, function-level diffs, import changes, and
    /// dependency updates.
    #[tool(
        name = "get_change_manifest",
        description = "Returns structured metadata about what changed between two git refs"
    )]
    async fn get_change_manifest(
        &self,
        Parameters(args): Parameters<ManifestArgs>,
    ) -> Result<Json<ManifestResponse>, String> {
        let start = std::time::Instant::now();
        let tool_name = "get_change_manifest";

        // Extract ref info before moving args into spawn_blocking.
        let base_ref_clone = args.base_ref.clone();
        let head_ref_clone = args.head_ref.clone();

        let result = tokio::task::spawn_blocking(move || {
            let repo_path = match args.repo_path {
                Some(p) => PathBuf::from(p),
                None => std::env::current_dir()
                    .map_err(|e| format!("cannot determine working directory: {e}"))?,
            };

            let root_span = tracing::info_span!(
                "mcp.tool.get_change_manifest",
                tool_name = "get_change_manifest",
                repo_path_hash = crate::privacy::hash_repo_path(&repo_path).as_str(),
                ref_base = crate::privacy::normalize_ref_pattern(&args.base_ref).as_str(),
                ref_head = tracing::field::Empty,
                page_number = tracing::field::Empty,
                page_size = tracing::field::Empty,
                response_files_count = tracing::field::Empty,
                response_bytes = tracing::field::Empty,
                response_truncated = tracing::field::Empty,
            );
            let _enter = root_span.enter();

            if let Some(ref head) = args.head_ref {
                root_span.record(
                    "ref_head",
                    crate::privacy::normalize_ref_pattern(head).as_str(),
                );
            } else {
                root_span.record("ref_head", "worktree");
            }

            let page_size = crate::pagination::clamp_page_size(args.page_size);
            let offset = if let Some(ref cursor_str) = args.cursor {
                let cursor =
                    crate::pagination::decode_cursor(cursor_str).map_err(|e| e.to_string())?;
                // Resolve refs and validate cursor SHAs match
                let reader =
                    crate::git::reader::RepoReader::open(&repo_path).map_err(|e| e.to_string())?;
                let base_sha = reader
                    .resolve_commit(&args.base_ref)
                    .map_err(|e| e.to_string())?
                    .sha;
                let head_sha = match &args.head_ref {
                    Some(h) => reader.resolve_commit(h).map_err(|e| e.to_string())?.sha,
                    None => "WORKTREE".to_string(),
                };
                crate::pagination::validate_cursor(&cursor, &base_sha, &head_sha)
                    .map_err(|e| e.to_string())?;
                cursor.offset
            } else {
                0
            };

            root_span.record("page_number", (offset / page_size) as i64);
            root_span.record("page_size", page_size as i64);
            if args.cursor.is_some() {
                crate::metrics::get().record_pagination_page(tool_name);
            }

            let options = ManifestOptions {
                include_patterns: args.include_patterns,
                exclude_patterns: args.exclude_patterns,
                include_function_analysis: args.include_function_analysis,
            };
            let result = match args.head_ref {
                Some(head) => build_manifest(
                    &repo_path,
                    &args.base_ref,
                    &head,
                    &options,
                    offset,
                    page_size,
                ),
                None => {
                    build_worktree_manifest(&repo_path, &args.base_ref, &options, offset, page_size)
                }
            };

            match &result {
                Ok(manifest) => {
                    root_span.record("response_files_count", manifest.files.len() as i64);
                    root_span.record(
                        "response_truncated",
                        manifest.pagination.next_cursor.is_some(),
                    );
                    // Serialize once for byte counting — also used for metrics outside spawn_blocking.
                    let bytes = serde_json::to_vec(manifest).map(|v| v.len()).unwrap_or(0);
                    root_span.record("response_bytes", bytes as i64);
                }
                Err(e) => {
                    tracing::error!(error = %e, "tool invocation failed");
                }
            }

            result.map(Json).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?;

        let metrics = crate::metrics::get();
        let duration_ms = start.elapsed().as_millis() as f64;
        metrics.record_duration(tool_name, duration_ms);

        match &result {
            Ok(Json(response)) => {
                metrics.record_request(tool_name, "success");

                // TODO(#43): response is serialized inside spawn_blocking for span attributes
                // and again by rmcp for transport — consider caching.
                let json_bytes = serde_json::to_vec(response).map(|v| v.len()).unwrap_or(0);
                metrics.record_response_bytes(tool_name, json_bytes as f64);
                metrics.record_tokens_estimated(tool_name, (json_bytes / 4) as f64);

                // Manifest-specific metrics
                metrics.record_files_returned(response.files.len() as f64);

                for file in &response.files {
                    if file.language != "unknown" {
                        metrics.record_language(&file.language);
                    }
                    metrics.record_change_scope(change_scope_label(file.change_scope));
                    if let Some(fns) = &file.functions_changed {
                        metrics.record_functions_changed(&file.language, fns.len() as f64);
                    }
                }

                if response.pagination.next_cursor.is_some() {
                    metrics.record_truncated(tool_name, "paginated");
                }

                // Ref pattern classification
                metrics.record_ref_pattern(crate::privacy::classify_ref_mode(
                    &base_ref_clone,
                    head_ref_clone.as_deref(),
                ));
            }
            Err(e) => {
                metrics.record_request(tool_name, "error");
                metrics.record_error(tool_name, crate::privacy::classify_error_kind(e));
            }
        }

        result
    }

    /// Returns one manifest per commit in a range, so agents can see
    /// what changed in each commit separately.
    #[tool(
        name = "get_commit_history",
        description = "Returns per-commit manifests for a range of commits"
    )]
    async fn get_commit_history(
        &self,
        Parameters(args): Parameters<HistoryArgs>,
    ) -> Result<Json<HistoryResponse>, String> {
        let start = std::time::Instant::now();
        let tool_name = "get_commit_history";

        // Extract ref info before moving args into spawn_blocking.
        let base_ref_clone = args.base_ref.clone();
        let head_ref_clone = args.head_ref.clone();

        let result = tokio::task::spawn_blocking(move || {
            let repo_path = match args.repo_path {
                Some(p) => PathBuf::from(p),
                None => std::env::current_dir()
                    .map_err(|e| format!("cannot determine working directory: {e}"))?,
            };

            let root_span = tracing::info_span!(
                "mcp.tool.get_commit_history",
                tool_name = "get_commit_history",
                repo_path_hash = crate::privacy::hash_repo_path(&repo_path).as_str(),
                ref_base = crate::privacy::normalize_ref_pattern(&args.base_ref).as_str(),
                ref_head = crate::privacy::normalize_ref_pattern(&args.head_ref).as_str(),
                page_number = tracing::field::Empty,
                page_size = tracing::field::Empty,
                response_files_count = tracing::field::Empty,
                response_bytes = tracing::field::Empty,
                response_truncated = tracing::field::Empty,
            );
            let _enter = root_span.enter();

            let page_size = crate::pagination::clamp_page_size(args.page_size);
            let offset = if let Some(ref cursor_str) = args.cursor {
                let cursor =
                    crate::pagination::decode_cursor(cursor_str).map_err(|e| e.to_string())?;
                let reader =
                    crate::git::reader::RepoReader::open(&repo_path).map_err(|e| e.to_string())?;
                let base_sha = reader
                    .resolve_commit(&args.base_ref)
                    .map_err(|e| e.to_string())?
                    .sha;
                let head_sha = reader
                    .resolve_commit(&args.head_ref)
                    .map_err(|e| e.to_string())?
                    .sha;
                crate::pagination::validate_cursor(&cursor, &base_sha, &head_sha)
                    .map_err(|e| e.to_string())?;
                cursor.offset
            } else {
                0
            };

            root_span.record("page_number", (offset / page_size) as i64);
            root_span.record("page_size", page_size as i64);
            if args.cursor.is_some() {
                crate::metrics::get().record_pagination_page(tool_name);
            }

            let options = ManifestOptions {
                include_patterns: vec![],
                exclude_patterns: vec![],
                include_function_analysis: true,
            };
            let result = build_history(
                &repo_path,
                &args.base_ref,
                &args.head_ref,
                &options,
                offset,
                page_size,
            );

            match &result {
                Ok(response) => {
                    let total_files: usize = response.commits.iter().map(|c| c.files.len()).sum();
                    root_span.record("response_files_count", total_files as i64);
                    root_span.record(
                        "response_truncated",
                        response.pagination.next_cursor.is_some(),
                    );
                    let bytes = serde_json::to_vec(response).map(|v| v.len()).unwrap_or(0);
                    root_span.record("response_bytes", bytes as i64);
                }
                Err(e) => {
                    tracing::error!(error = %e, "tool invocation failed");
                }
            }

            result.map(Json).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?;

        let metrics = crate::metrics::get();
        let duration_ms = start.elapsed().as_millis() as f64;
        metrics.record_duration(tool_name, duration_ms);

        match &result {
            Ok(Json(response)) => {
                metrics.record_request(tool_name, "success");

                // TODO(#43): response is serialized inside spawn_blocking for span attributes
                // and again by rmcp for transport — consider caching.
                let json_bytes = serde_json::to_vec(response).map(|v| v.len()).unwrap_or(0);
                metrics.record_response_bytes(tool_name, json_bytes as f64);
                metrics.record_tokens_estimated(tool_name, (json_bytes / 4) as f64);

                metrics.record_ref_pattern(crate::privacy::classify_ref_mode(
                    &base_ref_clone,
                    Some(&head_ref_clone),
                ));

                for commit in &response.commits {
                    for file in &commit.files {
                        if file.language != "unknown" {
                            metrics.record_language(&file.language);
                        }
                        metrics.record_change_scope(change_scope_label(file.change_scope));
                    }
                }
            }
            Err(e) => {
                metrics.record_request(tool_name, "error");
                metrics.record_error(tool_name, crate::privacy::classify_error_kind(e));
            }
        }

        result
    }

    /// Returns complete before/after file content at two git refs for
    /// specified file paths.
    #[tool(
        name = "get_file_snapshots",
        description = "Returns complete before/after file content at two git refs"
    )]
    async fn get_file_snapshots(
        &self,
        Parameters(args): Parameters<SnapshotArgs>,
    ) -> Result<Json<SnapshotResponse>, String> {
        let start = std::time::Instant::now();
        let tool_name = "get_file_snapshots";

        // Extract ref info before moving args into spawn_blocking.
        let base_ref_clone = args.base_ref.clone();
        let head_ref_clone = args.head_ref.clone();

        let result = tokio::task::spawn_blocking(move || {
            let repo_path = match args.repo_path {
                Some(p) => PathBuf::from(p),
                None => std::env::current_dir()
                    .map_err(|e| format!("cannot determine working directory: {e}"))?,
            };

            let head_ref = args.head_ref.as_deref().unwrap_or("HEAD");

            let root_span = tracing::info_span!(
                "mcp.tool.get_file_snapshots",
                tool_name = "get_file_snapshots",
                repo_path_hash = crate::privacy::hash_repo_path(&repo_path).as_str(),
                ref_base = crate::privacy::normalize_ref_pattern(&args.base_ref).as_str(),
                ref_head = tracing::field::Empty,
                response_files_count = tracing::field::Empty,
                response_bytes = tracing::field::Empty,
                response_truncated = tracing::field::Empty,
            );
            let _enter = root_span.enter();

            if args.head_ref.is_some() {
                root_span.record(
                    "ref_head",
                    crate::privacy::normalize_ref_pattern(head_ref).as_str(),
                );
            } else {
                root_span.record("ref_head", "worktree");
            }

            let options = SnapshotOptions {
                include_before: args.include_before,
                include_after: args.include_after,
                max_file_size_bytes: args.max_file_size_bytes,
                line_range: args.line_range,
            };
            let result =
                build_snapshots(&repo_path, &args.base_ref, head_ref, &args.paths, &options);

            match &result {
                Ok(response) => {
                    root_span.record("response_files_count", response.files.len() as i64);
                    let any_truncated = response.files.iter().any(|f| {
                        f.before.as_ref().is_some_and(|c| c.truncated)
                            || f.after.as_ref().is_some_and(|c| c.truncated)
                    });
                    root_span.record("response_truncated", any_truncated);
                    let bytes = serde_json::to_vec(response).map(|v| v.len()).unwrap_or(0);
                    root_span.record("response_bytes", bytes as i64);
                }
                Err(e) => {
                    tracing::error!(error = %e, "tool invocation failed");
                }
            }

            result.map(Json).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?;

        let metrics = crate::metrics::get();
        let duration_ms = start.elapsed().as_millis() as f64;
        metrics.record_duration(tool_name, duration_ms);

        match &result {
            Ok(Json(response)) => {
                metrics.record_request(tool_name, "success");

                // TODO(#43): response is serialized again by rmcp — consider caching or estimating size
                let json_bytes = serde_json::to_vec(response).map(|v| v.len()).unwrap_or(0);
                metrics.record_response_bytes(tool_name, json_bytes as f64);
                metrics.record_tokens_estimated(tool_name, (json_bytes / 4) as f64);

                metrics.record_ref_pattern(crate::privacy::classify_ref_mode(
                    &base_ref_clone,
                    head_ref_clone.as_deref(),
                ));

                // Check for per-file truncation in snapshots
                for file in &response.files {
                    let truncated = file.before.as_ref().is_some_and(|c| c.truncated)
                        || file.after.as_ref().is_some_and(|c| c.truncated);
                    if truncated {
                        metrics.record_truncated(tool_name, "max_file_size");
                    }
                }
            }
            Err(e) => {
                metrics.record_request(tool_name, "error");
                metrics.record_error(tool_name, crate::privacy::classify_error_kind(e));
            }
        }

        result
    }

    /// Returns callers, callees, and test references for each function that
    /// changed between two git refs. Answers "what calls this function?" and
    /// "what does this function call?" without the agent having to grep.
    #[tool(
        name = "get_function_context",
        description = "Returns callers, callees, and test references for each function that changed between two git refs"
    )]
    async fn get_function_context(
        &self,
        Parameters(args): Parameters<ContextArgs>,
    ) -> Result<Json<FunctionContextResponse>, String> {
        let start = std::time::Instant::now();
        let tool_name = "get_function_context";

        // Extract ref info before moving args into spawn_blocking.
        let base_ref_clone = args.base_ref.clone();
        let head_ref_clone = args.head_ref.clone();

        let result = tokio::task::spawn_blocking(move || {
            let repo_path = match args.repo_path {
                Some(p) => PathBuf::from(p),
                None => std::env::current_dir()
                    .map_err(|e| format!("cannot determine working directory: {e}"))?,
            };

            let root_span = tracing::info_span!(
                "mcp.tool.get_function_context",
                tool_name = "get_function_context",
                repo_path_hash = crate::privacy::hash_repo_path(&repo_path).as_str(),
                ref_base = crate::privacy::normalize_ref_pattern(&args.base_ref).as_str(),
                ref_head = crate::privacy::normalize_ref_pattern(&args.head_ref).as_str(),
                response_files_count = tracing::field::Empty,
                response_bytes = tracing::field::Empty,
                response_truncated = tracing::field::Empty,
            );
            let _enter = root_span.enter();

            let result = build_function_context(&repo_path, &args.base_ref, &args.head_ref);

            match &result {
                Ok(response) => {
                    root_span.record("response_files_count", response.functions.len() as i64);
                    root_span.record("response_truncated", false);
                    let bytes = serde_json::to_vec(response).map(|v| v.len()).unwrap_or(0);
                    root_span.record("response_bytes", bytes as i64);
                }
                Err(e) => {
                    tracing::error!(error = %e, "tool invocation failed");
                }
            }

            result.map(Json).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?;

        let metrics = crate::metrics::get();
        let duration_ms = start.elapsed().as_millis() as f64;
        metrics.record_duration(tool_name, duration_ms);

        match &result {
            Ok(Json(response)) => {
                metrics.record_request(tool_name, "success");

                // TODO(#43): response is serialized inside spawn_blocking for span attributes
                // and again by rmcp for transport — consider caching.
                let json_bytes = serde_json::to_vec(response).map(|v| v.len()).unwrap_or(0);
                metrics.record_response_bytes(tool_name, json_bytes as f64);
                metrics.record_tokens_estimated(tool_name, (json_bytes / 4) as f64);

                // Context-specific metrics: count the unique files touched by changed
                // functions so "files returned" stays meaningful across tools.
                let mut seen_files: std::collections::HashSet<&str> =
                    std::collections::HashSet::new();
                for func in &response.functions {
                    seen_files.insert(func.file.as_str());
                }
                metrics.record_files_returned(seen_files.len() as f64);

                // Languages analyzed and per-language function counts — derived
                // from file extensions of the changed functions, mirroring the
                // manifest tool's `languages.analyzed` and `functions_changed`
                // signals so all four tools emit the same language-keyed metrics.
                let functions_per_language = functions_per_language_counts(&response.functions);
                for (language, count) in &functions_per_language {
                    metrics.record_language(language);
                    metrics.record_functions_changed(language, *count as f64);
                }

                metrics.record_ref_pattern(crate::privacy::classify_ref_mode(
                    &base_ref_clone,
                    Some(&head_ref_clone),
                ));
            }
            Err(e) => {
                metrics.record_request(tool_name, "error");
                metrics.record_error(tool_name, crate::privacy::classify_error_kind(e));
            }
        }

        result
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for GitPrismServer {}

pub async fn run_server() -> anyhow::Result<()> {
    let _telemetry = crate::telemetry::init();
    crate::metrics::get().record_session_started();
    let server = GitPrismServer::new();
    let transport = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
    server.serve(transport).await?.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_registers_get_change_manifest_tool() {
        let router = GitPrismServer::tool_router();
        assert!(
            router.has_route("get_change_manifest"),
            "get_change_manifest must be registered"
        );
    }

    #[test]
    fn it_registers_get_commit_history_tool() {
        let router = GitPrismServer::tool_router();
        assert!(
            router.has_route("get_commit_history"),
            "get_commit_history must be registered"
        );
    }

    #[test]
    fn it_registers_get_file_snapshots_tool() {
        let router = GitPrismServer::tool_router();
        assert!(
            router.has_route("get_file_snapshots"),
            "get_file_snapshots must be registered"
        );
    }

    #[test]
    fn it_registers_get_function_context_tool() {
        let router = GitPrismServer::tool_router();
        assert!(
            router.has_route("get_function_context"),
            "get_function_context must be registered as an MCP tool"
        );
    }

    #[test]
    fn it_counts_function_context_entries_per_language() {
        use crate::tools::types::{
            BlastRadius, CalleeEntry, CallerEntry, FunctionChangeType, FunctionContextEntry,
            ScopingMode,
        };

        let entries = vec![
            FunctionContextEntry {
                name: "calculate".to_string(),
                file: "src/lib.rs".to_string(),
                change_type: FunctionChangeType::Modified,
                blast_radius: BlastRadius::compute(0, 0),
                scoping_mode: ScopingMode::Scoped,
                callers: Vec::<CallerEntry>::new(),
                callees: Vec::<CalleeEntry>::new(),
                test_references: Vec::<CallerEntry>::new(),
                caller_count: 0,
            },
            FunctionContextEntry {
                name: "helper".to_string(),
                file: "src/main.rs".to_string(),
                change_type: FunctionChangeType::Added,
                blast_radius: BlastRadius::compute(0, 0),
                scoping_mode: ScopingMode::Scoped,
                callers: Vec::<CallerEntry>::new(),
                callees: Vec::<CalleeEntry>::new(),
                test_references: Vec::<CallerEntry>::new(),
                caller_count: 0,
            },
            FunctionContextEntry {
                name: "process_data".to_string(),
                file: "scripts/tool.py".to_string(),
                change_type: FunctionChangeType::Added,
                blast_radius: BlastRadius::compute(0, 0),
                scoping_mode: ScopingMode::Scoped,
                callers: Vec::<CallerEntry>::new(),
                callees: Vec::<CalleeEntry>::new(),
                test_references: Vec::<CallerEntry>::new(),
                caller_count: 0,
            },
            FunctionContextEntry {
                name: "Binary".to_string(),
                file: "blob.bin".to_string(),
                change_type: FunctionChangeType::Added,
                blast_radius: BlastRadius::compute(0, 0),
                scoping_mode: ScopingMode::Scoped,
                callers: Vec::<CallerEntry>::new(),
                callees: Vec::<CalleeEntry>::new(),
                test_references: Vec::<CallerEntry>::new(),
                caller_count: 0,
            },
        ];

        let counts = functions_per_language_counts(&entries);

        assert_eq!(counts.get("rust").copied(), Some(2));
        assert_eq!(counts.get("python").copied(), Some(1));
        assert!(
            !counts.contains_key("unknown"),
            "unknown language must be excluded from metric labels"
        );
    }

    #[test]
    fn it_registers_exactly_four_tools() {
        let router = GitPrismServer::tool_router();
        let tools = router.list_all();
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(
            tools.len(),
            4,
            "expected exactly four MCP tools, found {}: {:?}",
            tools.len(),
            names
        );
    }
}
