<!-- Narration script for the git-prism PATH-shim capstone demo (issue #291, epic #284).
     Processed by scripts/build-demo.py — uses <!-- SEGMENT: name --> markers.
     TTS-friendly: no markdown syntax, no shell special characters in segment bodies.
     Segment names map 1-to-1 to sleep-calibrated sections in demo/path-shim-demo.sh. -->

## Metadata

- Issue: 291
- Epic: 284
- Recording date: 2026-05-30

---

## Segments

<!-- SECTION: opening -->

<!-- SEGMENT: title -->
The redirect hook nudges agents after they reach for raw git. The PATH shim goes one step further. It wraps git itself, so an agent gets structured data and a human gets vanilla git, from the very same command.

<!-- SEGMENT: install -->
Installing is one command: git-prism hooks install, with the path shim flag. It creates a symlink named git in a git-prism owned bin directory, and prints the one line you add to your PATH. The status command confirms the shim is in place.

<!-- SECTION: behavior -->

<!-- SEGMENT: intercept -->
Now an agent shell, where the calling agent is detected from environment variables. The agent runs git diff against a ref range. The shim recognizes the agent, classifies the command, and returns a structured manifest as JSON. Per-file metadata, line counts, no hunk noise.

<!-- SEGMENT: vanilla -->
Same command, a clean human shell with no agent markers. The shim detects no agent and steps aside completely, exec-ing the real git binary. The human sees the familiar diff git output, byte for byte. One wrapper, two audiences.

<!-- SEGMENT: sentinel -->
Nested git calls are the danger: the shim must never exec itself in a loop. Two guards prevent it. The inside-shim sentinel forces passthrough even in an agent shell, and the resolver skips its own directory when it walks PATH. The debug resolver flag shows exactly which real git it delegated to.

<!-- SECTION: closing -->

<!-- SEGMENT: uninstall -->
Uninstall is just as clean: git-prism hooks uninstall, with the path shim flag, removes the symlink and its directory. Status confirms it is gone, and your shell is back to ordinary git.

<!-- SEGMENT: closing -->
The git-prism PATH shim. Structured git for agents, vanilla git for humans, decided per invocation, with no loops. Source and docs at github dot com slash mike lane slash git-prism.
