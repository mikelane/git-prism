<!-- Narration script for git-prism v0.7.0 redirect-hook capstone demo (issue #242).
     Processed by scripts/build-demo.py — uses <!-- SEGMENT: name --> markers.
     TTS-friendly: no markdown syntax, no shell special characters in segment bodies.
     Segment names map 1-to-1 to sleep-calibrated sections in demo/redirect-demo.sh. -->

<!-- SEGMENT: problem_intro -->
I've got a notes file in a git repository here — tracked, committed, under version control. Now watch what happens when an agent runs an unguarded bash redirect.

<!-- SEGMENT: problem_clobber -->
The file is gone. One redirect operator, no confirmation, no diff, no way back. The original content is overwritten and git history cannot help because the change was never staged. That's the problem git-prism v 0.7.0 solves.

<!-- SEGMENT: hook_intro -->
git-prism ships a Claude Code PreToolUse hook that intercepts every Bash tool call before it runs. It reads the command from standard input and returns one of three decisions: block it, advise against it, or pass through silently.

<!-- SEGMENT: hook_block -->
A command like gh pr diff gets blocked outright — exit code 2 stops the tool call cold. Raw patch text isn't something an agent can act on reliably, so the hook prevents it entirely.

<!-- SEGMENT: hook_advisory -->
git diff between refs gets an advisory instead. The hook lets it through — exit code zero, no interruption — but adds a suggestion in the output: use get change manifest instead, which returns structured data.

<!-- SEGMENT: hook_silent -->
A plain echo command passes through silently. No output, no delay. The tokenizer is Python's shlex library, so compound commands, subshells, and pipelines all parse structurally rather than relying on fragile regex.

<!-- SEGMENT: hook_install -->
Installing is one command: git-prism hooks install. It copies the hook scripts into your dot claude hooks directory and writes a PreToolUse entry into Claude Code's settings dot json. User scope is the default because it covers both top-level agents and subagents equally.

<!-- SEGMENT: git_diff_problem -->
Now for the second half of the story. Here's git diff on a real commit in git-prism's own repo. You get hunk headers, plus and minus line prefixes, whitespace context — everything a human needs to read a patch, and none of what an agent needs to act on it.

<!-- SEGMENT: review_change -->
git-prism manifest returns the same information as structured JSON — per-file metadata, line counts, function-level changes, none of the hunk-header noise. Pair it with git-prism context and you also get callers, callees, and a blast-radius risk score for every changed function. The MCP tool review change combines both in a single call.

<!-- SEGMENT: closing -->
That's git-prism v 0.7.0 — redirect protection for agentic bash sessions, and agent-native git data when you need it. Install with brew install git-prism or cargo install git-prism. Source and docs at github dot com slash mikelane slash git-prism.
