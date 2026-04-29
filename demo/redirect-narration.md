<!-- Narration script for git-prism v0.7.0 redirect-hook capstone demo (issue #242).
     Processed by scripts/build-demo.py — uses <!-- SEGMENT: name --> markers.
     TTS-friendly: no markdown syntax, no shell special characters in segment bodies.
     Segment names map 1-to-1 to sleep-calibrated sections in demo/redirect-demo.sh. -->

<!-- SEGMENT: hook_intro -->
Here is git-prism registered as an MCP server in Claude Code. Five structured tools available — get change manifest, get function context, review change. But Claude was trained on millions of git commands. That muscle memory runs deep. Watch what happens.

<!-- SEGMENT: git_diff_problem -->
Claude reaches for git diff, exactly as its training taught it. Hunk headers, plus and minus prefixes, whitespace context. A format built for humans reading patches. Even with structured alternatives one tool call away, the default instinct is porcelain. Registration is not enough.

<!-- SEGMENT: hook_block -->
Some commands are dangerous no matter what. g-h p-r diff returns raw patch text — not something an agent can act on reliably. The hook hard-blocks it with exit code 2. The tool call never reaches the shell.

<!-- SEGMENT: hook_advisory -->
git diff between refs gets a softer touch. The hook lets it through — exit code zero — but injects a suggestion: use get change manifest instead. The agent still gets its answer, just through a structured path next time.

<!-- SEGMENT: hook_silent -->
A plain echo command passes through silently. No output, no delay. The tokenizer uses Python's shlex library, so compound commands, subshells, and pipelines all parse structurally rather than by fragile regex.

<!-- SEGMENT: hook_install -->
Installing is one command: git-prism hooks install. It copies the hook scripts into your dot claude hooks directory and writes a PreToolUse entry into Claude Code's settings dot json. User scope is the default because it covers both top-level agents and subagents equally.

<!-- SEGMENT: review_change -->
git-prism manifest returns the same information as structured JSON — per-file metadata, line counts, function-level changes, no hunk noise. Add git-prism context and you get callers, callees, and a blast-radius risk score for every changed function. The MCP tool review change combines both in one call.

<!-- SEGMENT: closing -->
git-prism v 0.7.0. A redirect hook that meets Claude where its training leads, and agent-native git data when it gets there. Install with brew install git-prism or cargo install git-prism. Source and docs at github dot com slash mike lane slash git-prism.
