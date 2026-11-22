# Shim as an rc Shell Function — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** git-prism intercepts only git commands the agent types into its shell; any program that execs git gets byte-for-byte real git.

**Architecture:** A `function git { … }` block in the interactive rc calls a new `git-prism shim-run git …` entry point that runs the existing `run_shim` decision tree. The binary invoked as `git`/`gh` (stale symlink) becomes pure passthrough. `run_shim` gains three more passthrough exits: handler error, stdout redirected to a regular file, and exit-status flags.

**Tech Stack:** Rust (std, serde_json, existing `RealGitExec`), behave BDD (Python), zsh/bash.

**Spec:** `docs/superpowers/specs/2026-11-20-shim-shell-function-design.md`. Issue #387.

## Global Constraints

- Breaking change: commits use `feat(shim)!:` / `fix(shim):`; no `Cargo.toml` version bump; CHANGELOG `[Unreleased]` only.
- Production code never shells out to `git` except through `RealGitExec`; repo access via `gix`.
- The function block goes in the interactive rc only (`.zshrc` / `.bashrc`), never `.zshenv` / `.bash_profile`.
- Keep the existing markers `# >>> git-prism shim >>>` / `# <<< git-prism shim <<<` and `strip_all_marker_blocks`.
- Tests that expect interception must set `AI_AGENT=claude-code` on the child and `env_remove` `CI`, `GIT_PRISM_PASSTHROUGH`, `GIT_PRISM_DISABLE`, `GIT_PRISM_INSIDE_SHIM`. The dev shell may export `GIT_PRISM_PASSTHROUGH=1` (the installed 0.10.0 shim is broken); tests must not inherit it.
- Tests use the worktree build (`env!("CARGO_BIN_EXE_git-prism")` / `cargo build` output), never `~/.local/share/git-prism/bin/git`.
- Parity comparisons run the reference git as the absolute path that `resolve_real_git` returns for the test's PATH (build PATH = `<tmp shim dir>:/usr/bin:/bin` so both sides use `/usr/bin/git`).
- `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`, `cargo test`, `behave bdd/features/shim_*.feature` all green before each push.

## Review Focus

1. **`git-prism` missing from PATH** (brew uninstall, broken PATH) — the function must fall back to `command git "$@"`, never break every git call. Test in Task 4.
2. **User has `alias git=…`** — `git() {` would be alias-expanded and error in zsh; the block must use `function git { … }` syntax. Test in Task 4 (source block in `zsh -c` after `alias git=hub`… assert no error, function defined).
3. **`git diff --quiet A..B` / `--exit-code`** — exit status is the answer; JSON returns 0 where git returns 1. Must pass through. Test in Task 1.
4. **Stale symlink dir still first on PATH after migration** (user's hand-written `export PATH=…git-prism/bin…` outside markers) — a program running `git` hits the binary as `git` and must get pure real git (Task 2), and install removes the symlinks (Task 4).
5. **Stderr parity on handler failure** — no `git-prism shim:` text may reach stderr when falling back to real git; stderr must equal real git's. Test in Task 1.

---

### Task 1: Harden `run_shim` passthrough exits

**Files:**
- Modify: `src/shim/classify.rs` (scripted-flag list, `show` peel syntax)
- Modify: `src/shim/handlers.rs:29-40` (`handle` returns `Result`, no `eprintln!`)
- Modify: `src/shim/mod.rs` (`run_shim` signature + error/file passthrough)
- Modify: `src/main.rs:315` (call site)
- Test: unit tests in `src/shim/classify.rs`, `src/shim/mod.rs`; integration `tests/shim_exit_codes.rs`

**Interfaces:**
- Produces: `pub(crate) enum StdoutKind { RegularFile, Other }` and `pub(crate) fn detect_stdout_kind() -> StdoutKind` in `src/shim/mod.rs` (unix: dup fd 1 via `std::io::stdout().as_fd().try_clone_to_owned()`, `File::from(..).metadata()?.is_file()`; any error → `Other`; non-unix → `Other`).
- Produces: `run_shim<E: EnvSource, G: RealGitExec>(argv: &[&str], env: &E, exec: &G, stdout_kind: StdoutKind, telemetry_guard: &mut TelemetryGuard) -> ExitCode`.
- Produces: `handlers::handle<W: Write>(c: &Classification<'_>, repo_path: &Path, argv: &[&str], out: &mut W) -> anyhow::Result<()>` (argv used by Task 3).

- [ ] **Step 1: Failing classifier tests** (parametrize with a table or `#[test_case]` if already used; otherwise one test per row):
  - `classify(&["git","diff","--quiet","a..b"]) == Passthrough`
  - `classify(&["git","diff","--exit-code","a..b"]) == Passthrough`
  - `classify(&["git","show","HEAD^{tree}"]) == Passthrough`
  - `classify(&["git","show","v1^{}"]) == Passthrough`
  - `classify(&["git","show","HEAD"]) == ShowSnapshot { sha: "HEAD" }` (unchanged)
- [ ] **Step 2:** `cargo test --bin git-prism -- classify` → FAIL on the four new rows.
- [ ] **Step 3:** Add `--quiet`, `--exit-code` to `has_scripted_output_flag`; in `classify_show`, also pass through when `spec.contains("^{")`.
- [ ] **Step 4: Failing `run_shim` unit tests** in `src/shim/mod.rs` with a recording fake `RealGitExec` and agent env (`AI_AGENT=claude-code`):
  - `it_passes_through_when_stdout_is_a_regular_file`: argv `["git","diff","a..b"]`, `StdoutKind::RegularFile` → fake records one `passthrough` call with the same argv.
  - `it_passes_through_when_the_handler_fails`: repo path = empty tempdir (not a repo), argv `["git","diff","a..b"]`, `StdoutKind::Other` → fake records `passthrough`; nothing written to stdout.
- [ ] **Step 5:** Run → FAIL (signature mismatch counts).
- [ ] **Step 6:** Implement: in `run_shim`, after classification and before dispatch, `if stdout_kind == StdoutKind::RegularFile` → record `ShimOutcome::Passthrough`, bounded flush, `exec.passthrough(argv)`. Change `handle` to return `anyhow::Result<()>` (log the error with `tracing::debug!`, no `eprintln!`); on `Err`, drop `out_buf`, record `ShimOutcome::Passthrough`, bounded flush, `exec.passthrough(argv)`. Record `ShimOutcome::Structured` only on `Ok`. `main.rs` passes `shim::detect_stdout_kind()`.
- [ ] **Step 7: Integration test** `tests/shim_exit_codes.rs` (uses the existing symlink harness for now; Task 2 migrates it): `it_returns_real_git_stderr_and_exit_code_when_handler_fails` — in a temp repo, `git diff nosuchref..HEAD` through the shim vs `/usr/bin/git` → equal exit code (128), equal stderr, stderr does not contain `git-prism`.
- [ ] **Step 8:** `cargo test` → PASS. Commit `fix(shim): exec real git on handler error, file redirect, and exit-status flags (#387)`; push.

### Task 2: `shim-run` entry point; binary-as-`git` becomes pure passthrough

**Files:**
- Modify: `src/main.rs:270-325`
- Modify: `src/shim/real_git.rs` only if `shim_canonical_path` needs `current_exe` preference (see Step 3)
- Modify (test harness migration): `tests/shim_*.rs`, `tests/hooks_dry_run.rs`, `tests/test_gh_classify.rs`, `bdd/steps/shim_wrapper_steps.py:70-110`, `bdd/steps/shim_parity_steps.py:150-160,360-380`
- Modify: `bdd/features/shim_wrapper.feature`, `bdd/features/shim_parity.feature`
- Test: new `tests/shim_program_parity.rs`

**Interfaces:**
- Consumes: `run_shim(..., stdout_kind, ...)` from Task 1.
- Produces: CLI contract `git-prism shim-run git <args…>` — handled in `main` before clap; `argv` passed to `run_shim` is `["git", <args…>]`; `StdRealGitExec.argv0` = `std::env::current_exe()` path string (so `resolve_real_git` skips the git-prism binary and its dir).
- Produces: invoked with basename `git` or `gh` → `exec.passthrough(&args)` immediately, no telemetry, no classification.

- [ ] **Step 1: Update BDD first (RED).** In `shim_wrapper.feature`, interception scenarios now invoke via `shim-run`; add:
  - `Scenario: A program that runs git through a stale git symlink gets real git output` — Given a temp repo with two commits and the shim symlink on PATH, When a Python program runs `git diff -M --no-color --no-ext-diff HEAD~1..HEAD`, Then stdout equals real git's stdout byte-for-byte and the exit codes match.
  - `Scenario: shim-run with stdout redirected to a file writes real git output`.
  Update `shim_wrapper_steps.py` helper to call `[binary, "shim-run", "git", *args]`; keep the symlink helper only for the new passthrough scenario. `gh` scenarios in `shim_parity.feature` assert passthrough. Run `behave bdd/features/shim_wrapper.feature` → new scenarios FAIL.
- [ ] **Step 2: Failing integration test** `tests/shim_program_parity.rs` — temp repo (commits A, B; B modifies and renames a file), shim dir with `git` symlink → built binary, PATH=`<shim dir>:/usr/bin:/bin`, agent env per Global Constraints. One `#[test]` per row, each comparing `(stdout bytes, exit code)` from `Command::new("git")` (resolved via that PATH) vs `/usr/bin/git`:
  - `diff -M --no-color --no-ext-diff A..B`
  - `show A:<path>`, `show A:`, `show :<path>`, `show A^{tree}`
  - `log A..B`
  - `diff nosuchref..B` (exit 128), `show A:missing.txt` (exit 128)
  And `it_intercepts_via_shim_run`: `Command::new(bin).args(["shim-run","git","diff","A..B"])` stdout parses as JSON with a `files` key.
- [ ] **Step 3:** Run → FAIL. Implement in `main.rs`: compute `basename`; if `basename` is `git`/`gh` → build `StdRealGitExec` and `return exec.passthrough(&args)`. Else if `args.get(1) == Some("shim-run")` → `argv = args[2..]` (require `argv[0]` in {`git`}; anything else or empty → print usage to stderr, exit 2), `argv0 = current_exe`, then the existing opt-out check, telemetry init, `run_shim`, bounded flush. Confirm via test that real-git resolution from `shim-run` does not return the git-prism binary even when a stale symlink dir is first on PATH.
- [ ] **Step 4: Migrate existing tests** that build a `git` symlink and expect JSON or shim behaviour to `Command::new(bin).args(["shim-run","git", …])`. Tests that assert passthrough may keep the symlink. Each migrated file must still set the agent env explicitly.
- [ ] **Step 5:** `cargo test` and `behave bdd/features/shim_*.feature` → PASS. Commit `feat(shim)!: add shim-run entry point; binary invoked as git/gh is pure passthrough (#387)`; push.

### Task 3: Raw-git hint in intercepted JSON

**Files:**
- Modify: `src/shim/handlers.rs` (the five `serde_json::to_writer_pretty` sites: lines ~77, 95, 119, 182, 305)
- Test: unit test in `src/shim/handlers.rs`; one BDD assertion in `shim_wrapper.feature`

**Interfaces:**
- Consumes: `argv` parameter from Task 1.
- Produces: `fn write_with_hint<W: Write, T: Serialize>(out: &mut W, value: &T, argv: &[&str]) -> anyhow::Result<()>` — serialise to `serde_json::Value`, insert top-level key `"git_prism_hint"` with value `"git-prism structured output. For raw git output run: command git <args>"` where `<args>` is `argv[1..]` joined by single spaces, then `to_writer_pretty`. Shim-only: MCP tool responses and their schemas are unchanged.

- [ ] **Step 1:** Failing test `it_adds_a_raw_git_hint_to_intercepted_json`: `handle(&Manifest{range:"A..B"}, repo, &["git","diff","A..B"], &mut buf)` → parsed `buf["git_prism_hint"] == "git-prism structured output. For raw git output run: command git diff A..B"`.
- [ ] **Step 2:** Run → FAIL. Implement `write_with_hint`; replace the five sites.
- [ ] **Step 3:** Add `And the JSON has key "git_prism_hint"` to one existing interception scenario. `cargo test`, behave → PASS. Commit `feat(shim): add raw-git hint to intercepted JSON (#387)`; push.

### Task 4: `shim install|uninstall|status` manage an rc function block

**Files:**
- Modify: `src/shim_cmd.rs` (block body, `write_path_block`/`rc_files_for_shell` callers, `run_install_with_io`, `run_uninstall`, `run_status`, PATH-first consent prompt removed)
- Modify: `src/hooks.rs` only to reuse `uninstall_path_shim(home)` (already exists)
- Modify: `bdd/features/shim_wrapper.feature` install/status/uninstall scenarios
- Test: unit tests in `src/shim_cmd.rs`; integration `tests/shim_marker_pentest.rs`, `tests/shim_auto_path_pentest.rs` updated

**Interfaces:**
- Produces: `const SHIM_FUNCTION_BLOCK_BODY: &str` — exactly:
  ```sh
  function git {
    if command -v git-prism >/dev/null 2>&1; then
      command git-prism shim-run git "$@"
    else
      command git "$@"
    fi
  }
  ```
- Produces: `fn interactive_rc_for_shell(shell: &str) -> &'static str` (`zsh`→`.zshrc`, `bash`→`.bashrc`, default `.zshrc`); existing `rc_files_for_shell` stays for stripping legacy blocks from both files.
- Produces: `run_status` prints `shim: installed in ~/<rc>` / `shim: not installed`, plus `legacy PATH shim: present at <dir>` when the symlink dir exists.

- [ ] **Step 1: Failing unit tests** (tempdir HOME, injected shell name and stdin/stdout like existing `run_install_with_io` tests):
  - `it_writes_the_function_block_to_zshrc_only`: `.zshrc` contains markers + `function git {`; `.zshenv` contains no marker.
  - `it_migrates_the_legacy_path_block`: pre-seed `.zshenv` and `.zshrc` with the old PATH block (use the old body text verbatim from git history, `SHIM_PATH_BLOCK_BODY`) and `bin/git`,`bin/gh` symlinks → after install, neither file contains `git-prism/bin`, `.zshrc` has exactly one block, symlinks and the empty dir are gone.
  - `it_is_idempotent`: install twice → `.zshrc` byte-identical after second run.
  - `it_preserves_user_content`: lines before/after the block unchanged.
  - `it_uninstalls_cleanly`: install then uninstall → no marker lines in either file, no symlink dir.
  - `it_writes_to_bashrc_for_bash`.
- [ ] **Step 2:** Run → FAIL. Implement; install no longer prompts about PATH (nothing to put on PATH). Install prints the rc file written and `Restart your agent session so its shell snapshot picks up the function.`
- [ ] **Step 3: Shell-behaviour tests** (`tests/shim_function_shell.rs`, skip with a printed reason if `zsh` is absent): write the block to a temp file; PATH = `<dir with built binary symlinked as git-prism>:/usr/bin:/bin`; agent env.
  - `zsh -c 'source block; git diff A..B'` → stdout is JSON.
  - `zsh -c 'source block; python3 -c "import subprocess,sys; sys.stdout.buffer.write(subprocess.run([\"git\",\"diff\",\"A..B\"],capture_output=True).stdout)"'` → equals `/usr/bin/git diff A..B`.
  - `zsh -c 'source block; git diff A..B > out.patch'` → `out.patch` equals real git output.
  - PATH without git-prism: `zsh -c 'source block; git rev-parse HEAD'` → equals real git, exit 0 (Review Focus 1).
  - `zsh -c 'alias git=echo; source block; type git'` → exits 0 and reports a function (Review Focus 2). Same rows under `bash -c` if bash is present.
- [ ] **Step 4:** Update install/status/uninstall BDD scenarios (`hooks install --path-shim` alias now writes the function block and prints a deprecation warning). `cargo test`, behave → PASS. Commit `feat(shim)!: install an rc shell function instead of a PATH symlink (#387)`; push.

### Task 5: Docs and ADR

**Files:**
- Create: `docs/decisions/0012-shim-as-shell-function.md` (status Accepted; supersedes the PATH-layer interception in 0009/0010; records the classify_pr.py incident, the who-reads-the-output rule, the redirect-to-file rule, accepted trade-offs: `make`/`sh -c`/wrapper scripts no longer intercepted, pipes into programs are, bash support unverified against Claude Code's snapshot)
- Modify: `README.md` shim section, `CLAUDE.md` (shim mentions, `hooks.rs` module line), `CHANGELOG.md` `[Unreleased]` → `### Breaking` entry with migration: "re-run `git-prism shim install`; remove any hand-written `export PATH=…/git-prism/bin…` lines"
- Modify: `docs/decisions/0010-shim-direct-call-interception.md` status line → `Superseded by 0012`
- Commit the spec and this plan alongside.

- [ ] **Step 1:** Write the docs. `cargo test` (tool-drift test unaffected) and `scripts/validate-pr-refs.sh` pass. Commit `docs(shim): ADR-0012 and docs for rc-function shim (#387)`; push.
