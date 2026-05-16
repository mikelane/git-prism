# Agent Detection Environment Variables — Research Notes

**Status:** Research-only. Not a proposal, not a design.

**Why:** We're considering a `git` wrapper that only enriches output when the caller is an AI agent. That requires answering "is this process running on behalf of an agent?" with a low enough false-positive rate that humans and CI scripts get untouched git. The catalog below is what each agent actually sets, where the gaps are, and which signals are unreliable. Earlier conversation casually claimed Claude Code sets `CLAUDE_CODE=1`; it doesn't. That's the kind of mistake this doc exists to prevent.

## 1. Empirically Confirmed (this session)

Captured from `env` inside the Claude Code session running this research (Claude Code 2.1.141):

| Variable | Observed value | Notes |
|----------|----------------|-------|
| `CLAUDECODE` | `1` | The canonical Claude Code agent marker. **Not `CLAUDE_CODE=1`** — that was wrong in earlier conversation. |
| `CLAUDE_CODE_ENTRYPOINT` | `cli` | Distinguishes CLI vs other entrypoints. |
| `CLAUDE_CODE_SESSION_ID` | `<uuid>` | Per-session UUID. Could be used to scope cache keys. |
| `CLAUDE_CODE_EXECPATH` | `/Users/.../share/claude/versions/2.1.141` | Path to the running binary. Version-sniffable. |
| `CLAUDE_CODE_ENABLE_TELEMETRY` | `1` | User-config; not a reliable agent marker on its own. |
| `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS` | `1` | Feature flag; unstable. |
| `CLAUDE_CODE_NEW_INIT` | `1` | Feature flag; unstable. |
| `CLAUDE_CODE_NO_FLICKER` | `0` | UI config; not a marker. |
| `AI_AGENT` | `claude-code_2-1-141_agent` | Cross-tool convention popularized by Vercel's `@vercel/detect-agent`. Claude Code sets it; format is `<tool>_<version>_agent`. |
| `ANTHROPIC_BASE_URL` | `http://127.0.0.1:9801` | Provider-level, not agent-marker. Present whenever an Anthropic SDK is configured, including in human-driven scripts. **Do not treat as an agent signal.** |
| `ANTHROPIC_ADMIN_KEY` | `<redacted>` | Same caveat as above. |
| `WARP_CLI_AGENT_PROTOCOL_VERSION` | `1` | Set by the **Warp terminal**, not by Claude Code. Appears here because the Claude Code process was launched from Warp. Tells you the *terminal* is agent-aware, not that the current process is an agent. |

**Key takeaway for verification:** The two stable Claude Code-set agent markers are `CLAUDECODE=1` and `AI_AGENT=claude-code_*`. The `CLAUDE_CODE_*` suffix family is internal-facing; some are feature flags that may not appear in future versions.

**Not verified in this session** (only documented): subagent invocations spawned via Claude Code's `Task` tool, which inherit the same env. There is no separate `CLAUDE_CODE_SUBAGENT=1` marker observed.

## 2. Documented in Other Agents (research-sourced, not empirically verified here)

### Confirmed via official docs

| Agent | Variable | Value | Source |
|-------|----------|-------|--------|
| Cursor | `CURSOR_AGENT` | non-empty (typically `1`) | [Cursor Terminal docs](https://docs.cursor.com/en/agent/terminal) |
| Gemini CLI | `GEMINI_CLI` | `1` | agents.md issue #136 (third-party report; not in official docs we found) |
| Codex CLI (OpenAI) | `CODEX_SANDBOX` | `seatbelt` | agents.md issue #136 |
| Codex CLI (OpenAI) | `CODEX_HOME` | `<path>` | [Codex CLI Reference](https://developers.openai.com/codex/cli/reference) — config var, also implies Codex is running |
| Cline | `CLINE_ACTIVE` | `true` | agents.md issue #136 |
| OpenCode | `OPENCODE_CLIENT` | `1` | agents.md issue #136 |
| OpenCode | `OPENCODE_API_URL` | `<url>` | [OpenCode CLI](https://opencode.ai/docs/cli/) — config var |
| Augment | `AUGMENT_AGENT` | `1` | agents.md issue #136 |
| TRAE AI | `TRAE_AI_SHELL_ID` | session id | agents.md issue #136 |
| Goose | `AGENT` | `goose` | agents.md issue #136 — uses both proposed standard and its own |
| Amp (Sourcegraph) | `AGENT` | `amp` | agents.md issue #136 |
| Aider | `AIDER_*` | many config vars | [Aider Options](https://aider.chat/docs/config/options.html). **No documented "I am running" marker.** All `AIDER_*` vars are user-set config, not aider-set runtime flags. Detection has to fall back on parent-process inspection. |

### Active feature requests — no marker exists yet

| Agent | Status | Source |
|-------|--------|--------|
| GitHub Copilot CLI | Feature request open ([vscode #265446](https://github.com/microsoft/vscode/issues/265446)). Proposed names: `VSCODE_COPILOT_TERMINAL`, `COPILOT_TERMINAL`, `VSCODE_ASSISTANT_SESSION`. None shipped. | [GitHub Copilot CLI docs](https://docs.github.com/copilot/how-tos/use-copilot-agents/use-copilot-cli) |
| Zed (Agent panel) | Feature request open ([zed #47038](https://github.com/zed-industries/zed/discussions/47038)). Users currently cannot distinguish Zed Agent terminal from interactive terminal. | [Zed Agent docs](https://zed.dev/docs/ai/agent-panel) |
| Replit Agent | No documented marker for Replit Agent execution specifically. Replit sets `REPL_ID` and others, but they fire in human-driven Repls too. | [Replit App Configuration](https://docs.replit.com/replit-app/configuration) |

### Indirect / terminal-level signals (NOT process-level agent markers)

| Variable | What it actually means |
|----------|------------------------|
| `TERM_PROGRAM=WarpTerminal` | Process is running under the Warp *terminal*, which may or may not be in Warp Agent mode. False positives common: humans use Warp interactively all the time. |
| `WARP_CLI_AGENT_PROTOCOL_VERSION` | Warp's CLI integration is loaded. Same caveat. |
| `VSCODE_INJECTION` | Terminal was launched by VS Code (any extension, including Copilot, but also direct human use). Not an agent marker. |
| `SSH_CONNECTION` | Shell was reached over SSH. Useful as a fallback when `TERM_PROGRAM` is stripped (Warp on Windows SSH bug, [warp #6990](https://github.com/warpdotdev/warp/issues/6990)). Still not an agent marker. |

## 3. Proposed Cross-Agent Standards

Two competing conventions, neither universally adopted:

### `AGENT=<name>` — agents.md proposal #136

- Mirrors the established `CI=true` convention.
- Implemented by: Goose, Amp.
- Open issues filed against: Claude Code (anthropics/claude-code #24838), Cursor.
- Risk of collision with non-AI tools that already use `AGENT` (SSH agent forwarding, build agents).
- Source: [agents.md issue #136](https://github.com/agentsmd/agents.md/issues/136).

### `AI_AGENT=<tool>_<version>_agent` — Vercel convention

- Implemented by: Claude Code (confirmed empirically above).
- Consumed by: `@vercel/detect-agent` (npm package; source repo currently returns 404, package page returns 403 to WebFetch — could not retrieve the canonical fallback list directly).
- Less collision-prone than bare `AGENT`.
- Value is structured (parseable for tool + version).

Neither convention is universal, so detection has to check both plus the tool-specific vars in §2.

## 4. Detection Signal Reliability — Things That Will Bite Us

1. **TTY presence is not a clean proxy.** `[[ ! -t 1 ]]` (stdout is not a terminal) is commonly suggested. But:
   - Some agents allocate a PTY (Warp Full Terminal Use, Cursor terminal tool with pseudo-tty enabled).
   - CI pipelines also have no TTY — false positive for "agent" when it's actually a build.
   - Combined with `CI=true` exclusion, TTY absence becomes more useful but still imperfect.

2. **Env vars inherit through `git` hooks.** When `git push` triggers a `pre-push` hook, the hook process inherits the agent env vars from the calling agent. A wrapper that only enriches output when an agent var is set will also "enrich" output destined for the hook — which is typically a script expecting raw git output. This is a real footgun.

3. **Subagents inherit parent env.** Claude Code's `Task` tool subagents get the same `CLAUDECODE=1`. If we ever want to behave differently for subagents (e.g., disable interactive output more aggressively), there's no built-in signal.

4. **User-set env vars survive.** A human who exports `CLAUDECODE=1` in their shell to test something will trigger agent mode in every subsequent terminal. Unlikely but not impossible — and worse, sticky if it lands in `.zshrc`.

5. **CI environments overlap.** Claude Code, Cursor, Codex can all run in GitHub Actions. In that case `CI=true` and `CLAUDECODE=1` are both set. The wrapper has to decide which behavior wins — and the answer probably isn't "agent" (CI tooling consumes git's normal output).

6. **`ANTHROPIC_BASE_URL` / `OPENAI_API_KEY` are red herrings.** They mean "an Anthropic/OpenAI SDK is configured in the environment," not "an agent is running." Many human-driven scripts set them.

7. **Terminal-level vars (`TERM_PROGRAM`) are wrong-layer.** They tell you about the terminal app, not the current process's caller. Warp users running git interactively will trigger false positives.

8. **Aider has no marker at all.** A real-world coding agent in active use with no documented runtime env var. Parent-process inspection (walk `ps` up the tree looking for `aider`, `cursor-agent`, `gh copilot`, etc.) is the only way to catch it — and that's brittle, slow, and a security smell.

## 5. Recommended Detection Order (for future design — not yet decided)

If/when we design the wrapper, a reasonable check ordering would be:

```
1. AI_AGENT is set and non-empty             → agent (Vercel convention)
2. AGENT is set AND value matches allowlist  → agent (agents.md convention; allowlist
                                                 prevents collision with ssh-agent etc.)
3. Tool-specific markers:
     CLAUDECODE, CURSOR_AGENT, GEMINI_CLI,
     CODEX_SANDBOX, CLINE_ACTIVE,
     AUGMENT_AGENT, OPENCODE_CLIENT,
     TRAE_AI_SHELL_ID                        → agent
4. CI is set                                 → NOT agent (CI wins over weak signals)
5. Otherwise                                 → NOT agent
```

This deliberately does **not** include TTY checks, terminal-program checks, or parent-process inspection. Each of those has too high a false-positive rate to be a primary signal. They could be additive evidence in a confidence-scoring scheme but shouldn't gate behavior alone.

## 6. Open Questions To Resolve Before Designing

Listed for future me, not answered here:

1. **Does Claude Code's `Task` subagent inherit `CLAUDECODE=1`?** Empirically yes (this session), but worth confirming the spec or hooks behavior.
2. **What does Cursor set when running its CLI vs the in-IDE agent?** The `CURSOR_AGENT` doc is for the IDE; the standalone `cursor-agent` CLI may set different/more vars.
3. **Does Warp Agent mode set anything beyond `WARP_CLI_AGENT_PROTOCOL_VERSION`?** Needed to disambiguate "Warp terminal running git interactively" from "Warp Agent running git on the user's behalf."
4. **What's the empirical env inside Aider?** Worth a one-shot test to see if undocumented vars exist (`PYTHONUNBUFFERED`, parent-process name leaks, etc.).
5. **Should we treat `gh copilot` runs as agent-driven even though Copilot CLI has no marker?** If yes, we need parent-process inspection or a `gh` wrapper, not just env vars.
6. **What happens to git hooks invoked by the wrapper?** If `pre-push` shells out and inspects `git status`, we'd re-enter our own wrapper recursively. Need a `GIT_PRISM_PASSTHROUGH=1` self-disable signal or `argv[0]` inspection to break the loop.

## 7. Sources

- [agents.md issue #136 — Standard env var for agent runtime detection](https://github.com/agentsmd/agents.md/issues/136) — canonical cross-agent comparison
- [Cursor Terminal docs — `CURSOR_AGENT`](https://docs.cursor.com/en/agent/terminal)
- [Cursor Agent Terminal Tool](https://cursor.com/docs/agent/tools/terminal)
- [Aider Options Reference](https://aider.chat/docs/config/options.html)
- [Aider .env Config](https://aider.chat/docs/config/dotenv.html)
- [GitHub Copilot CLI docs](https://docs.github.com/copilot/how-tos/use-copilot-agents/use-copilot-cli)
- [vscode #265446 — request for Copilot terminal marker](https://github.com/microsoft/vscode/issues/265446)
- [Codex CLI Reference](https://developers.openai.com/codex/cli/reference)
- [Codex CLI Features](https://developers.openai.com/codex/cli/features)
- [Codex Config Reference](https://developers.openai.com/codex/config-reference)
- [Cline GitHub](https://github.com/cline/cline)
- [Cline shell integration troubleshooting](https://github.com/cline/cline/wiki/Troubleshooting-%E2%80%90-Shell-Integration-Unavailable)
- [Warp Terminal Modes](https://docs.warp.dev/agent-platform/local-agents/interacting-with-agents/terminal-and-agent-modes/)
- [Warp env vars docs](https://docs.warp.dev/knowledge-and-collaboration/warp-drive/environment-variables/)
- [warp #6990 — TERM_PROGRAM not set under SSH on Windows](https://github.com/warpdotdev/warp/issues/6990)
- [Gemini CLI Configuration](https://google-gemini.github.io/gemini-cli/docs/get-started/configuration.html)
- [Gemini CLI GitHub](https://github.com/google-gemini/gemini-cli)
- [Sourcegraph Amp Owner's Manual](https://ampcode.com/manual)
- [@sourcegraph/cody-agent on npm](https://www.npmjs.com/package/@sourcegraph/cody-agent)
- [Zed Agent Panel](https://zed.dev/docs/ai/agent-panel)
- [zed #47038 — env vars for agent vs interactive terminals](https://github.com/zed-industries/zed/discussions/47038)
- [zed #37469 — env config for external agents](https://github.com/zed-industries/zed/issues/37469)
- [OpenCode CLI](https://opencode.ai/docs/cli/)
- [OpenCode Config](https://opencode.ai/docs/config/)
- [Replit App Configuration](https://docs.replit.com/replit-app/configuration)
- `@vercel/detect-agent` (npm) — package page returned 403 to automated fetch; would need a human-triggered check or `npm view` to retrieve the canonical fallback list. Referenced in agents.md #136.
