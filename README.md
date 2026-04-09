# git-prism

Agent-optimized git data for LLM agents. Three MCP tools that replace human-oriented
diffs with structured JSON -- function-level granularity, import tracking,
dependency changes, complete file snapshots, and per-commit history.

## The Problem

Git's porcelain output (`diff`, `log`, `--stat`) is designed for human eyes. When
an LLM agent parses a unified diff, it burns tokens on `@@` hunk headers, `+`/`-`
line prefixes, and whitespace context that carries no semantic meaning. Worse, it
has to reconstruct _what actually changed_ -- which functions were modified, which
imports were added, whether the file is generated -- from raw text.

git-prism gives agents structured data directly: a change manifest with per-file
metadata and function-level analysis, plus full before/after file content when
deeper inspection is needed.

## Installation

### From crates.io (recommended)

```bash
cargo install git-prism
```

### From source

```bash
cargo install --path .
```

### Binary download

Grab a prebuilt binary from the
[GitHub Releases](https://github.com/mikelane/git-prism/releases) page.

### Homebrew (macOS and Linux)

```bash
brew tap mikelane/tap
brew install git-prism
```

## MCP Registration

Register git-prism as an MCP server for Claude Code:

```bash
claude mcp add git-prism -- git-prism serve
```

That's it. The server uses stdio transport and is available in all Claude Code
sessions.

## Tools

### `get_change_manifest`

Returns structured metadata about what changed between two git refs.

**Parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `base_ref` | string | _(required)_ | Base git ref (commit SHA, branch, tag, `HEAD~1`) |
| `head_ref` | string | `"HEAD"` | Head git ref |
| `repo_path` | string | cwd | Path to the git repository |
| `include_patterns` | string[] | `[]` | Glob patterns to include (e.g. `["*.rs", "*.go"]`) |
| `exclude_patterns` | string[] | `[]` | Glob patterns to exclude (e.g. `["*.lock"]`) |
| `include_function_analysis` | bool | `true` | Enable tree-sitter function/import analysis |
| `cursor` | string | `null` | Opaque pagination cursor from a previous response |
| `page_size` | int | `100` | Max file entries per page (1-500) |

**Example output:**

```json
{
  "metadata": {
    "repo_path": "/home/user/myproject",
    "base_ref": "main",
    "head_ref": "HEAD",
    "base_sha": "a1b2c3d4e5f6",
    "head_sha": "f6e5d4c3b2a1",
    "generated_at": "2026-04-03T12:00:00Z",
    "version": "0.1.0"
  },
  "summary": {
    "total_files_changed": 3,
    "files_added": 1,
    "files_modified": 2,
    "files_deleted": 0,
    "files_renamed": 0,
    "total_lines_added": 47,
    "total_lines_removed": 12,
    "total_functions_changed": 4,
    "languages_affected": ["go", "rust"]
  },
  "files": [
    {
      "path": "src/handler.go",
      "old_path": null,
      "change_type": "modified",
      "language": "go",
      "is_binary": false,
      "is_generated": false,
      "lines_added": 25,
      "lines_removed": 8,
      "size_before": 1200,
      "size_after": 1450,
      "functions_changed": [
        {
          "name": "HandleRequest",
          "old_name": null,
          "change_type": "signature_changed",
          "start_line": 15,
          "end_line": 42,
          "signature": "func HandleRequest(ctx context.Context, req *Request) (*Response, error)"
        },
        {
          "name": "validateInput",
          "old_name": "checkInput",
          "change_type": "renamed",
          "start_line": 44,
          "end_line": 58,
          "signature": "func validateInput(req *Request) error"
        }
      ],
      "imports_changed": {
        "added": ["context", "errors"],
        "removed": ["log"]
      }
    }
  ],
  "dependency_changes": [
    {
      "file": "go.mod",
      "added": [{"name": "github.com/pkg/errors", "old_version": null, "new_version": "v0.9.1"}],
      "removed": [],
      "changed": []
    }
  ],
  "pagination": {
    "total_items": 3,
    "page_start": 0,
    "page_size": 100,
    "next_cursor": null
  }
}
```

### `get_file_snapshots`

Returns complete before/after file content at two git refs. No diffs to parse --
the agent gets the full file at each point in time.

**Parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `base_ref` | string | _(required)_ | Base git ref |
| `head_ref` | string | `"HEAD"` | Head git ref |
| `paths` | string[] | _(required)_ | File paths to snapshot (max 20) |
| `repo_path` | string | cwd | Path to the git repository |
| `include_before` | bool | `true` | Include file content at base ref |
| `include_after` | bool | `true` | Include file content at head ref |
| `max_file_size_bytes` | int | `100000` | Truncate files larger than this |
| `line_range` | [int, int] | `null` | Return only lines in this range (1-indexed) |

**Example output:**

```json
{
  "metadata": {
    "repo_path": "/home/user/myproject",
    "base_ref": "main",
    "head_ref": "HEAD",
    "generated_at": "2026-04-03T12:00:00Z"
  },
  "files": [
    {
      "path": "src/handler.go",
      "language": "go",
      "is_binary": false,
      "before": {
        "content": "package main\n\nfunc HandleRequest(req *Request) error {\n    // old implementation\n}\n",
        "line_count": 5,
        "size_bytes": 82,
        "truncated": false
      },
      "after": {
        "content": "package main\n\nimport \"context\"\n\nfunc HandleRequest(ctx context.Context, req *Request) (*Response, error) {\n    // new implementation\n}\n",
        "line_count": 7,
        "size_bytes": 130,
        "truncated": false
      },
      "error": null
    }
  ],
  "token_estimate": 53
}
```

### `get_commit_history`

Returns one manifest per commit in a range, so agents can see what changed in
each commit separately instead of a single collapsed diff.

**Parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `base_ref` | string | _(required)_ | Base git ref (exclusive -- commits after this) |
| `head_ref` | string | _(required)_ | Head git ref (inclusive) |
| `repo_path` | string | cwd | Path to the git repository |
| `cursor` | string | `null` | Opaque pagination cursor from a previous response |
| `page_size` | int | `100` | Max commits per page (1-500) |

**Example output:**

```json
{
  "commits": [
    {
      "metadata": {
        "sha": "a1b2c3d4",
        "message": "add validation helper",
        "author": "Jane Dev",
        "timestamp": "2026-04-03T10:30:00+00:00"
      },
      "files": [
        {
          "path": "src/validate.rs",
          "change_type": "added",
          "language": "rust",
          "lines_added": 25,
          "lines_removed": 0
        }
      ],
      "summary": {
        "total_files_changed": 1,
        "files_added": 1,
        "total_lines_added": 25,
        "total_lines_removed": 0
      }
    }
  ],
  "pagination": {
    "total_items": 1,
    "page_start": 0,
    "page_size": 100,
    "next_cursor": null
  }
}
```

## CLI Usage

git-prism also works as a standalone CLI for scripting and debugging:

```bash
# Change manifest between two refs
git-prism manifest HEAD~1..HEAD

# Manifest for a specific repo path
git-prism manifest main..feature-branch --repo /path/to/repo

# Per-commit history for a range
git-prism history HEAD~5..HEAD

# Smaller pages for constrained environments
git-prism manifest main..HEAD --page-size 50

# File snapshots for specific paths
git-prism snapshot HEAD~3..HEAD --paths src/main.rs src/lib.rs

# List supported languages
git-prism languages
```

The `manifest`, `snapshot`, and `history` commands output JSON to stdout. The
`manifest` and `history` commands auto-paginate internally (default page size
500, configurable with `--page-size`) and always return the complete result. The
`languages` command outputs plain text.

## Agent Workflow

The typical agent pattern is two calls -- triage, then deep dive:

**Step 1: Triage with `get_change_manifest`**

Ask for the manifest to understand the shape of a change. The summary tells you
file counts, line counts, and affected languages. The per-file entries tell you
which functions changed signatures, which imports were added, and whether files
are generated (safe to skip).

**Step 2: Deep dive with `get_file_snapshots`**

Once you know which files matter, request full snapshots of just those files.
You get complete before/after content -- no reconstructing files from diff hunks.
Use `line_range` to focus on specific sections and `include_before: false` when
you only need the current state.

This two-step approach keeps token usage low. The manifest is compact metadata;
snapshots are requested only for files that need inspection.

For large changesets, the manifest may span multiple pages. When `next_cursor`
is non-null in the response, pass it back as the `cursor` parameter to fetch
the next page. The `summary` always reflects all files regardless of pagination,
so you can triage before paging through file entries.

## Supported Languages

Function-level analysis uses [tree-sitter](https://tree-sitter.github.io/tree-sitter/)
to extract functions, methods, and imports from source code.

| Language | Extensions | Extracts |
|----------|------------|----------|
| C | `.c`, `.h` | functions, declarations, `#include` directives |
| C++ | `.cpp`, `.hpp`, `.cc`, `.cxx`, `.hh`, `.hxx` | class/namespace-qualified methods, functions, `#include` directives |
| C# | `.cs` | methods, constructors, `using` directives |
| Go | `.go` | functions, methods, imports |
| Java | `.java` | methods, constructors, imports |
| JavaScript | `.js`, `.jsx` | functions, arrow functions, methods, imports |
| Kotlin | `.kt`, `.kts` | functions, methods, extension functions, imports |
| PHP | `.php` | functions, methods, `use` declarations |
| Python | `.py` | functions, methods, imports |
| Ruby | `.rb` | methods, singleton methods, `require`/`require_relative` |
| Rust | `.rs` | functions, methods, use statements |
| Swift | `.swift` | functions, methods, init declarations, imports |
| TypeScript | `.ts`, `.tsx` | functions, arrow functions, methods, imports |

Files in unsupported languages still appear in the manifest with full
line/size/change-type metadata -- `functions_changed` is `null` (not an empty
array) to distinguish "no grammar available" from "analyzed, nothing changed."

### Content-Aware Function Diffing

Function changes are detected by comparing SHA-256 hashes of function body
content, not line positions. This means:

- **Reordered functions** (moved but not modified) produce no change entries,
  eliminating false positives that waste agent attention.
- **Body-only changes** (same signature, different implementation) are detected
  as `modified`, even when line numbers don't shift.
- **Renamed functions** are detected when an unmatched added function shares a
  body hash with an unmatched deleted function. These produce a single `renamed`
  entry with `old_name` populated, instead of separate `deleted` + `added`.

The `change_type` values for functions are: `added`, `modified`, `deleted`,
`signature_changed`, `renamed`.

## Dependency File Tracking

git-prism parses dependency files and reports added, removed, and version-changed
packages:

- `Cargo.toml` (Rust)
- `package.json` (Node.js)
- `go.mod` (Go)
- `pyproject.toml` (Python, PEP 621)

## Telemetry

Optional OpenTelemetry instrumentation, disabled by default and opt-in via environment variables.

| Variable | Purpose | Default |
|----------|---------|---------|
| `GIT_PRISM_OTLP_ENDPOINT` | OTLP gRPC endpoint URL. When unset, telemetry is disabled. | unset (disabled) |
| `GIT_PRISM_OTLP_HEADERS` | Comma-separated `key=value` pairs for auth headers. | unset |
| `GIT_PRISM_SERVICE_NAME` | Service name reported to the backend. | `git-prism` |
| `GIT_PRISM_SERVICE_VERSION` | Service version reported to the backend. | crate version |

Quick start with Jaeger (any OTLP-compatible backend works):
```bash
docker run -d --name jaeger -p 4317:4317 -p 16686:16686 jaegertracing/all-in-one:latest
GIT_PRISM_OTLP_ENDPOINT=http://localhost:4317 git-prism serve
```

**Privacy:** No raw paths, file contents, author names, or ref names are exported. Paths are
SHA-256 hashed; refs normalized to a bounded enum. See [docs/telemetry.md](docs/telemetry.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[Apache-2.0](LICENSE)
