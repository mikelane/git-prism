use std::path::PathBuf;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};

use crate::tools::{
    ManifestArgs, ManifestOptions, ManifestResponse, SnapshotArgs, SnapshotOptions,
    SnapshotResponse, build_manifest, build_snapshots,
};

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
        tokio::task::spawn_blocking(move || {
            let repo_path = args
                .repo_path
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap());
            let head_ref = args.head_ref.as_deref().unwrap_or("HEAD");
            let options = ManifestOptions {
                include_patterns: args.include_patterns,
                exclude_patterns: args.exclude_patterns,
                include_function_analysis: args.include_function_analysis,
            };
            build_manifest(&repo_path, &args.base_ref, head_ref, &options)
                .map(Json)
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
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
        tokio::task::spawn_blocking(move || {
            let repo_path = args
                .repo_path
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap());
            let head_ref = args.head_ref.as_deref().unwrap_or("HEAD");
            let options = SnapshotOptions {
                include_before: args.include_before,
                include_after: args.include_after,
                max_file_size_bytes: args.max_file_size_bytes,
                line_range: args.line_range,
            };
            build_snapshots(&repo_path, &args.base_ref, head_ref, &args.paths, &options)
                .map(Json)
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for GitPrismServer {}

pub async fn run_server() -> anyhow::Result<()> {
    let server = GitPrismServer::new();
    let transport = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
    server.serve(transport).await?.waiting().await?;
    Ok(())
}
