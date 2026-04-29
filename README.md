# git-prism

Agent-optimized git data for LLM agents. Five MCP tools that replace human-oriented
diffs with structured JSON -- function-level granularity, import tracking,
dependency changes, complete file snapshots, per-commit history, function
context (callers, callees, test references), and a one-call `review_change`
orchestration that combines manifest and function-context for PR review.

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

## Bundled redirect hooks

### What it does

The bundled redirect hook blocks accidental bash redirections that overwrite tracked files in agentic sessions. It soft-warns on watch-listed paths and hard-blocks `gh pr diff` and `mcp__github__*` tool calls that use output redirection. The hook uses a Python stdlib tokenizer (`shlex`) to parse bash structurally, catching compound commands (`&&`), subshells, pipelines, and variable expansion — not just simple regexes.

Requires `python3` (3.9+) on PATH. macOS ships this; Linux users can install via the system package manager.

### Install

```bash
git-prism hooks install
```

The command copies `~/.claude/hooks/git-prism-redirect.sh` and the Python helper alongside it, then writes a `PreToolUse` hook entry into Claude Code's `~/.claude/settings.json`. Default scope is `user` because Claude Code issue [anthropics/claude-code#13898](https://github.com/anthropics/claude-code/issues/13898) prevents custom subagents from correctly calling project-scoped MCP servers — using user scope ensures the redirect works in both root agents and subagents.

### Uninstall and status

```bash
git-prism hooks uninstall   # removes the hook file and settings.json entry
git-prism hooks status      # shows whether the hook is installed and at which scope
```

## Tools

### `get_change_manifest`

Returns structured metadata about what changed between two git refs.

**Parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `base_ref` | string | _(required)_ | Base git ref (commit SHA, branch, tag, `HEAD~1`) |
| `head_ref` | string | _(omitted → working tree)_ | Head git ref. When the field is omitted from the request, the tool compares `base_ref` against the working tree (staged + unstaged changes) instead of diffing two commits. Passing `"HEAD"` explicitly produces a committed-mode diff against `HEAD`, which is distinct from working-tree mode. |
| `repo_path` | string | cwd | Path to the git repository |
| `include_patterns` | string[] | `[]` | Glob patterns to include (e.g. `["*.rs", "*.go"]`) |
| `exclude_patterns` | string[] | `[]` | Glob patterns to exclude (e.g. `["*.lock"]`) |
| `include_function_analysis` | bool | `false` | Enable tree-sitter function/import analysis (opt-in; default keeps the response compact) |
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
    "version": "x.y.z"
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
      "change_scope": "committed",
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

### `get_function_context`

Returns callers, callees, and test references for each function that changed
between two refs. Answers "what calls this function?" and "what does this
function call?" without the agent having to grep.

Uses import-aware scoping for Rust, Python, Go, and TypeScript/JavaScript to
filter the caller scan to files that actually import the changed module. This
eliminates false positives from leaf-name collisions and improves performance on
large repos. Unsupported languages fall back to full-repo scanning.

**Parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `base_ref` | string | _(required)_ | Base git ref |
| `head_ref` | string | _(required)_ | Head git ref |
| `repo_path` | string | cwd | Path to the git repository |
| `cursor` | string | `null` | Opaque pagination cursor from a previous response |
| `page_size` | int | `25` | Max function entries per page (1-500). Lower default than the manifest tool because each entry carries caller/callee lists |
| `function_names` | string[] | `null` | Restrict the response to functions with these names. Use this to re-query a function whose lists were clamped on a prior call |
| `max_response_tokens` | int | `8192` | Response-size budget in estimated tokens. When exceeded, caller/callee lists are trimmed per entry. `0` disables the budget |

**Example output:**

```json
{
  "metadata": {
    "base_ref": "HEAD~1",
    "head_ref": "HEAD",
    "base_sha": "a1b2c3d4",
    "head_sha": "f6e5d4c3",
    "generated_at": "2026-04-09T12:00:00Z"
  },
  "functions": [
    {
      "name": "validate_input",
      "file": "src/validation.rs",
      "change_type": "modified",
      "blast_radius": {
        "production_callers": 1,
        "test_callers": 1,
        "has_tests": true,
        "risk": "low"
      },
      "scoping_mode": "scoped",
      "callers": [
        { "file": "src/handler.rs", "line": 42, "caller": "handle_request", "is_test": false }
      ],
      "callees": [
        { "callee": "check_length", "line": 15 },
        { "callee": "check_format", "line": 18 }
      ],
      "test_references": [
        { "file": "tests/test_validation.rs", "line": 10, "caller": "test_validate_empty", "is_test": true }
      ],
      "caller_count": 2
    }
  ]
}
```

The `scoping_mode` field indicates how the caller scan was performed: `"scoped"`
means import-based filtering was used (more precise but may miss callers that
use unusual import patterns), while `"fallback"` means the scan parsed every
file in the repo (authoritative but slower). Use this to decide whether a
zero-caller result is definitive or potentially incomplete.

### `review_change`

Returns combined change manifest and function-level blast radius for a ref
range, in one call. Use this **instead of `git diff <ref>..<ref>`** when
reviewing a PR, auditing a refactor, or assessing merge safety -- it answers
"what changed and what might break" in one tool invocation, with structured
JSON instead of raw diff text.

Replaces the common two-step workflow (`get_change_manifest` then
`get_function_context`) with a single call that runs both internally and splits
the response-size budget 40/60 (manifest / function_context).

**Parameters:**

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `base_ref` | string | _(required)_ | Base git ref |
| `head_ref` | string | _(omitted → working tree)_ | Head git ref. Omit to compare against the working tree (manifest only; the function-context half returns empty since callers/callees need committed content) |
| `repo_path` | string | cwd | Path to the git repository |
| `include_patterns` | string[] | `[]` | Glob patterns to include |
| `exclude_patterns` | string[] | `[]` | Glob patterns to exclude |
| `function_names` | string[] | `null` | Restrict the function-context half to these names |
| `max_response_tokens` | int | `8192` | Combined budget; split 40/60 between manifest and function_context. `0` disables both halves' budgets |
| `manifest_cursor` | string | `null` | Opaque cursor advancing only the manifest half |
| `function_context_cursor` | string | `null` | Opaque cursor advancing only the function-context half |
| `page_size` | int | `25` | Page size used for both halves |

The two cursors are independent so an agent can advance one half (e.g., walk
through a long file list) without re-paginating the other. Each sub-response
carries its share of the budget in `metadata.budget_tokens` so downstream
observability can audit the split decision.

**Example output:**

```json
{
  "manifest": {
    "metadata": {
      "base_ref": "HEAD~1",
      "head_ref": "HEAD",
      "budget_tokens": 1638,
      "...": "..."
    },
    "summary": { "total_files_changed": 3, "...": "..." },
    "files": [{ "path": "src/lib.rs", "...": "..." }],
    "pagination": { "next_cursor": null, "...": "..." }
  },
  "function_context": {
    "metadata": {
      "base_ref": "HEAD~1",
      "head_ref": "HEAD",
      "budget_tokens": 2458,
      "...": "..."
    },
    "functions": [{ "name": "validate", "blast_radius": { "risk": "medium" }, "...": "..." }],
    "pagination": { "next_cursor": null, "...": "..." }
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

# Function context (callers, callees, test references)
git-prism context HEAD~1..HEAD

# List supported languages
git-prism languages
```

The `manifest`, `snapshot`, `history`, and `context` commands output JSON to
stdout. The `manifest` and `history` commands auto-paginate internally (default
page size 500, configurable with `--page-size`) and always return the complete
result. The `languages` command outputs plain text.

## Agent Workflow

### One-call path: PR review and refactor audits

For PR review, refactor audits, or any "what changed and what might break" question,
use `review_change` instead of `git diff`:

```
review_change(base_ref="main", head_ref="HEAD")
```

This returns a combined `{ manifest, function_context }` payload in one call, splitting
the token budget 40/60 between the two halves. It replaces the two-step manifest +
function_context workflow when you need both in the same session.

### Three-call path: targeted inspection

When you need finer control -- different budgets per half, filtering by function name,
or deep-diving into specific files -- use the individual tools in order:

**Step 1: Triage with `get_change_manifest`**

Ask for the manifest to understand the shape of a change. The summary tells you
file counts, line counts, and affected languages. The per-file entries tell you
which functions changed signatures, which imports were added, and whether files
are generated (safe to skip).

**Step 2: Blast radius with `get_function_context`**

For the changed functions identified in step 1, request function context. This
tells you which other files call each changed function, what each changed
function calls, and which test files reference it. Each function includes a
`blast_radius` object with a `risk` level (`none`, `low`, `medium`, `high`)
computed from production caller count and test coverage -- sort by risk to
review the highest-impact changes first. The agent never has to grep through
the codebase to find callers or guess which tests to check.

**Step 3: Deep dive with `get_file_snapshots`**

Once you know which files and callers matter, request full snapshots of the
highest-impact files. You get complete before/after content -- no reconstructing
files from diff hunks. Use `line_range` to focus on specific sections and
`include_before: false` when you only need the current state.

Both paths keep token usage low. `review_change` is the most efficient starting
point for most review tasks; the three-call path is worth it when you need
targeted re-queries or want to page through a large manifest separately from
a large function list.

## Pagination

`get_change_manifest` and `get_commit_history` return a `pagination` object on
every response:

```json
"pagination": {
  "total_items": 842,
  "page_start": 0,
  "page_size": 100,
  "next_cursor": "eyJvZmZzZXQiOjEwMCwiYmFzZV9zaGEiOiIuLi4ifQ=="
}
```

When `next_cursor` is non-null, pass it back as the `cursor` parameter on the
next call to fetch the following page. The cursor is an opaque base64 payload
that encodes the page offset plus the resolved base/head SHAs; the server
rejects a cursor whose SHAs no longer match the refs supplied in the follow-up
call, so a cursor minted against `main..feature` cannot be reused against a
different range. The manifest `summary` always reflects all files in the
changeset regardless of which page is returned, so agents can triage from
page 1 before deciding whether to page through the remaining file entries.

`page_size` is clamped server-side to the `1..=500` range; values outside that
range are coerced rather than rejected. The CLI (`git-prism manifest`,
`git-prism history`) paginates internally and always returns the complete
result; the cursor contract is only visible through the MCP tool interface.

## Supported Languages

Function-level analysis uses [tree-sitter](https://tree-sitter.github.io/tree-sitter/)
to extract functions, methods, imports, and call sites from source code.

| Language | Extensions | Extracts |
|----------|------------|----------|
| C | `.c`, `.h` | functions, declarations, `#include` directives |
| C++ | `.cpp`, `.hpp`, `.cc`, `.cxx`, `.hh`, `.hxx` | class/namespace-qualified methods, functions, `extern "C"` blocks, `#include` directives |
| C# | `.cs` | methods, constructors, `using` directives |
| Go | `.go` | functions, methods, imports |
| Java | `.java` | methods, constructors, imports |
| JavaScript | `.js`, `.jsx` | functions, exported functions, arrow functions, methods, imports |
| Kotlin | `.kt`, `.kts` | functions, methods, extension functions, imports |
| PHP | `.php` | functions, methods, `use` declarations |
| Python | `.py` | functions, decorated functions, methods, imports |
| Ruby | `.rb` | methods, singleton methods, `require`/`require_relative` |
| Rust | `.rs` | functions, methods, use statements |
| Swift | `.swift` | functions, methods, init declarations, imports |
| TypeScript | `.ts`, `.tsx` | functions, exported functions, arrow functions, methods, imports |

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
`signature_changed`, `renamed`. Renamed entries carry the previous name in
the `old_name` field (null for all other variants) so agents can correlate the
post-rename function back to its history.

**Whitespace and comment sensitivity.** The body hash is computed over the raw
byte span of the function body as tree-sitter sees it. Reformatting runs,
comment additions or removals, trailing-whitespace changes, and indentation
shifts will therefore change the hash and produce a `modified` entry even when
the executable logic is unchanged. A future release may normalize whitespace
and strip comments before hashing; until then, running a formatter on a file
will show every touched function as `modified` on the next manifest call.

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
| `GIT_PRISM_OTLP_HEADERS` | Planned, not yet wired ([#43](https://github.com/mikelane/git-prism/issues/43)). Setting this variable has no effect today; managed OTLP backends that require auth headers need a local collector proxy. | unset |
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
