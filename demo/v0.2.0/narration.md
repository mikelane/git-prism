# Demo Narration: git-prism v0.2.0

## Metadata

- Issue: 33
- Recording date: 2026-04-05

---

## Segments

<!-- SEGMENT: intro -->
git prism version 0.2.0 brings broader language coverage, working tree support, and per-commit history. Let me walk you through each one.

<!-- SEGMENT: languages -->
The languages command now shows eight supported languages. We've added Java, C, and C plus plus alongside Go, Python, TypeScript, JavaScript, and Rust.

<!-- SEGMENT: java_analysis -->
Let's see Java analysis in action. I have a repo with a Calculator class that added a multiply method. The manifest shows class-qualified function names — Calculator dot add, Calculator dot multiply — and extracted imports.

<!-- SEGMENT: cpp_analysis -->
C plus plus works too. Here the manifest detects namespace-qualified methods. Notice Calculator colon colon add and math colon colon Calculator colon colon multiply. Header files and include directives are also extracted.

<!-- SEGMENT: working_tree -->
The biggest feature in 0.2.0: working tree status. Instead of comparing two commits, I run manifest with just HEAD. git prism now compares HEAD against my working tree — showing both staged and unstaged changes.

<!-- SEGMENT: working_tree_detail -->
Each file entry has a change scope field. Staged means the change is in the index. Unstaged means the file is modified on disk but not yet added. The same file can appear twice if it has both staged and unstaged changes.

<!-- SEGMENT: history -->
Per-commit history breaks down a range into individual commits. Instead of one collapsed diff, I get a separate manifest for each commit — with the SHA, author, message, and file changes.

<!-- SEGMENT: error -->
Error handling stays clean. If I try to use snapshot in working tree mode, git prism rejects it with a clear message: use a commit range instead.

<!-- SEGMENT: install -->
You can now install git prism directly from crates dot I O. One command: cargo install git prism. It's also available via Homebrew and GitHub Releases.

<!-- SEGMENT: closing -->
That's git prism 0.2.0. Eight languages, working tree support, per-commit history, and available on crates dot I O. The repo is at github dot com slash mike lane slash git prism.
