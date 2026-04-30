<!-- SEGMENT: intro -->
git prism is an MCP server that gives AI agents structured git data instead of human-oriented diffs. Let me show you what it does.

<!-- SEGMENT: languages -->
First, let's see which languages git prism supports for function-level analysis. Five languages out of the box: Go, Python, TypeScript, JavaScript, and Rust.

<!-- SEGMENT: manifest -->
Now the main tool: change manifest. I'll run it against git prism's own repo to see what changed in the last commit. The output is structured JSON with file changes, function-level diffs, import analysis, and dependency tracking.

<!-- SEGMENT: manifest_detail -->
Notice the summary shows exactly how many files changed, lines added and removed, and which languages were affected. Each file entry includes whether it's generated, what functions changed, and what imports were added or removed.

<!-- SEGMENT: snapshot -->
The second tool is file snapshots. Instead of a diff, it gives the complete before and after content of specific files. This is what agents actually need — full architectural context, not three-line hunks.

<!-- SEGMENT: snapshot_detail -->
The response includes a token estimate so agents can budget their context window. And file content is the complete source, not a diff to reconstruct from.

<!-- SEGMENT: error -->
What about errors? If I pass an invalid ref, git prism returns a clear, actionable message — not a panic or stack trace. It tells you what went wrong and suggests what to check.

<!-- SEGMENT: mcp_register -->
To use git prism with Claude Code, one command: claude mcp add git prism, then the path to the binary with the serve argument. Every Claude Code session now has structured git intelligence.

<!-- SEGMENT: closing -->
That's git prism. Agent-native git data, structured JSON, function-level analysis. Install it with brew or download from GitHub Releases. The repo is at github dot com slash mike lane slash git prism.
