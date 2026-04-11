# ADR 0007: PR #125 Squash Merge Post-Mortem

- **Status**: Accepted
- **Date**: 2026-04-10
- **Context**: Release integrity crisis discovered during Wave 1a gauntlet reviews

## Context

PR #125, merged 2026-04-09, bore the title "docs: add TDD rules and Epic SDLC process to CLAUDE.md" and was approved under the assumption it contained only docs changes. In reality, the PR was a squash merge of the entire `claude/spike-function-context-kOgGL` branch — 9 commits spanning spike, ADR, BDD bootstrap, tree-sitter extract_calls() across 13 languages, MCP tool types, CLI wiring, docs, and release bump. The merged commit (`acb9652`) touched **31 files** with **+2884 insertions / -39 deletions** total, including a new ADR (0005), a new CHANGELOG section, and substantial production Rust changes. The single largest addition was `src/tools/context.rs` at +432 lines — an entire new MCP tool implementation file landed under a docs-titled PR.

The squash commit body references PRs #114 through #123 — **none of these PR numbers exist in the repository**. The work was performed on a long-running spike branch and merged as a single PR with a misleading title, without the individual feature PRs being opened and reviewed separately.

## Consequences of the mis-merge

- No reviewer saw the 9 feature commits individually
- CI never ran `cargo test` on individual commits; only on the squashed branch
- The "90% mutation threshold" badge reported a number that was (coincidentally) also broken — see ADR 0008 for the shard bug
- CHANGELOG.md gained a `[0.6.0]` section claiming a "New MCP tool" that was not actually registered in `src/server.rs` until this PR
- `Cargo.toml` was bumped to `0.6.0` without a corresponding git tag or crates.io publish
- Users running `cargo install git-prism` continued to receive v0.4.0

## Decision

Accept the work in place. Reverting would destroy real progress (function context is genuinely useful, ADR 0005 is valid, the extract_calls() implementations across 13 languages are real engineering). Instead:

1. **Register `get_function_context` as a real MCP tool** (this PR). The CHANGELOG and README claims are now true.
2. **Retroactively tag v0.5.0 and v0.6.0** via the normal release process (coordinator task, post-merge).
3. **Document the process failure here** so the pattern can be prevented.
4. **Harden the PR workflow** against this failure mode (see "Prevention" below).

## Prevention

- Pre-push hook or CI check that validates `(#NNN)` references in commit messages against the real GitHub PR list. Fictional references block the push.
- PR title sanity check: a PR whose title starts with `docs:` but whose diff touches `src/**` beyond a threshold should require an explicit override comment.
- No squash merges of long-running branches without a PR title that accurately describes the FULL scope of the merge.
- Never reuse a spike branch name as the head of a "docs" PR.

## Alternatives Considered

1. **Hard revert `acb9652` and re-submit via individual PRs.** Rejected because: (a) it destroys real work, (b) tests pass, (c) Wave 1a gauntlet reviews already provided retroactive code review, (d) the merge is deep enough in history that a revert would conflict with subsequent docs PRs #147 and this current fix PR.
2. **Accept in place with no documentation.** Rejected because: it normalizes the process failure. Future maintainers deserve to know.

## References

- Commit: `acb9652b2296d7da1fd5c9e3678cf637ba5e7d41`
- PR: https://github.com/mikelane/git-prism/pull/125
- Spike branch: `remotes/origin/claude/spike-function-context-kOgGL`
- Wave 1a gauntlet reports: `~/.claude/pr-reviews/2026-04-10/gauntlet-v0.6.0-context/final-report.md`, `~/.claude/doc-review/2026-04-10/CHANGELOG.md`
