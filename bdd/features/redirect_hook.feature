# BDD bootstrap for epic #234 (bundled redirect hooks).
#
# Each scenario is tagged with the implementation issue that will make it
# GREEN. Until that issue lands, the @not_implemented tag keeps the scenario
# excluded from CI. The implementation PR's first commit must remove
# @not_implemented from its targeted scenarios (the RED commit) before any
# production code is written.
#
# All step definitions shell out to the real `git-prism` binary or the
# bundled `hooks/git-prism-redirect.sh` script. None of them use `pass` or
# `raise NotImplementedError` — when the underlying feature does not yet
# exist, the steps fail with assertion errors that document the contract
# being tested.

Feature: Redirect hooks for raw git invocations

  # ------------------------------------------------------------------------
  # W2: Tool description rewrites (#237)
  #
  # The four MCP tool doc comments must include comparative framing vs the
  # raw git equivalent (e.g., `get_change_manifest` references `git diff`).
  # The assertion is end-to-end: shell out to `git-prism serve` over stdio,
  # send a JSON-RPC `tools/list` request, and read the description fields.
  # ------------------------------------------------------------------------

  @ISSUE-237 @not_implemented
  Scenario: All five MCP tool descriptions include comparative framing vs raw git
    Given the git-prism MCP server is running over stdio
    When I send a "tools/list" JSON-RPC request
    Then the description for "get_change_manifest" mentions "git diff"
    And the description for "get_commit_history" mentions "git log"
    And the description for "get_file_snapshots" mentions "git show"
    And the description for "get_function_context" mentions "git log -S"
    And the description for "review_change" mentions "git diff"

  # ------------------------------------------------------------------------
  # W3: Python bash tokenizer (#238)
  #
  # The bundled `hooks/bash_redirect_hook.py` exposes `tokenize_command` and
  # `decide_redirect`. ADR-0008 fixes the parser as `shlex.shlex(posix=True,
  # punctuation_chars=True)` with two wrappers (heredoc skip, backtick
  # normalization). These scenarios drive each shape through the real hook
  # script with mock JSON on stdin and assert on the exit code / stderr /
  # stdout JSON contract.
  # ------------------------------------------------------------------------

  @ISSUE-238 @not_implemented
  Scenario: Plain "git diff main..HEAD" is recognized as a redirect target
    Given a hook input with bash command "git diff main..HEAD"
    When I run the bundled redirect hook with that input
    Then the hook exit code is 0
    And the hook stdout is JSON containing redirect advice for "get_change_manifest"

  @ISSUE-238 @not_implemented
  Scenario: Compound "cd /tmp && git diff main..HEAD" is recognized
    Given a hook input with bash command "cd /tmp && git diff main..HEAD"
    When I run the bundled redirect hook with that input
    Then the hook exit code is 0
    And the hook stdout is JSON containing redirect advice for "get_change_manifest"

  @ISSUE-238 @not_implemented
  Scenario: Subshell "(git log main..HEAD)" is recognized
    Given a hook input with bash command "(git log main..HEAD)"
    When I run the bundled redirect hook with that input
    Then the hook exit code is 0
    And the hook stdout is JSON containing redirect advice for "get_commit_history"

  @ISSUE-238 @not_implemented
  Scenario: Pipeline "git diff main..HEAD | grep foo" is recognized via the first command
    # The tokenizer must walk into pipelines and recognize git as the head of
    # the first stage. The grep on the right-hand side is a normal command
    # and must not derail recognition.
    Given a hook input with bash command "git diff main..HEAD | grep foo"
    When I run the bundled redirect hook with that input
    Then the hook exit code is 0
    And the hook stdout is JSON containing redirect advice for "get_change_manifest"

  @ISSUE-238 @not_implemented
  Scenario: Command substitution "$(...)" is recognized for both inner and outer git calls
    # `git rev-parse` is not on the watch list (no redirect), but the outer
    # `git diff` must still be recognized after the substitution boundary.
    Given a hook input with bash command "cd $(git rev-parse --show-toplevel) && git diff main..HEAD"
    When I run the bundled redirect hook with that input
    Then the hook exit code is 0
    And the hook stdout is JSON containing redirect advice for "get_change_manifest"
    And the hook stdout does not contain redirect advice for "git rev-parse"

  @ISSUE-238 @not_implemented
  Scenario: Backtick command substitution is normalized before tokenization
    # Per ADR-0008, backticks are stripped to whitespace by a pre-pass, so the
    # outer `git diff` is the only watch-list match left after normalization.
    Given a hook input with bash command "cd `git rev-parse --show-toplevel` && git diff main..HEAD"
    When I run the bundled redirect hook with that input
    Then the hook exit code is 0
    And the hook stdout is JSON containing redirect advice for "get_change_manifest"
    And the hook stdout does not contain redirect advice for "git rev-parse"

  @ISSUE-238 @not_implemented
  Scenario: Variable expansion "git diff $BASE..HEAD" is recognized without expansion
    Given a hook input with bash command "git diff $BASE..HEAD"
    And the environment variable "BASE" is set to "SECRETSENTINEL"
    When I run the bundled redirect hook with that input
    Then the hook exit code is 0
    And the hook stdout is JSON containing redirect advice for "get_change_manifest"
    And the hook does not attempt to expand "$BASE"
    And the hook output does not leak the value "SECRETSENTINEL"

  @ISSUE-238 @not_implemented
  Scenario: "git blame src/server.rs" is recognized
    Given a hook input with bash command "git blame src/server.rs"
    When I run the bundled redirect hook with that input
    Then the hook exit code is 0
    And the hook stdout is JSON containing redirect advice for "get_file_snapshots"

  @ISSUE-238 @not_implemented
  Scenario Outline: Read-only/write-side git commands are NOT redirected
    Given a hook input with bash command "<command>"
    When I run the bundled redirect hook with that input
    Then the hook exit code is 0
    And the hook stdout is empty
    And the hook stderr is empty

    Examples:
      | command          |
      | git status       |
      | git add file.txt |
      | git commit -m hi |
      | git push origin  |
      | git fetch origin |

  @ISSUE-238 @not_implemented
  Scenario: Heredoc body skip — git inside heredoc is ignored, surrounding command is parsed
    Given a hook input with the bash command from "heredoc_with_git_inside.txt"
    When I run the bundled redirect hook with that input
    Then the hook exit code is 0
    And the hook stdout is empty
    And the hook stderr is empty

  @ISSUE-238 @not_implemented
  Scenario: Tab-stripped heredoc "<<-EOF" body is also skipped
    # `<<-` strips leading tabs from the body but the tokenizer must still
    # treat the body as opaque. Only the line after the closing tag should
    # be recognized — and the surrounding command (`git status`) is not on
    # the watch list, so no advice is emitted.
    Given a hook input with the bash command from "heredoc_dash_with_git_inside.txt"
    When I run the bundled redirect hook with that input
    Then the hook exit code is 0
    And the hook stdout is empty
    And the hook stderr is empty

  @ISSUE-238 @not_implemented
  Scenario: Quoted heredoc "<<'EOF'" suppresses expansion and is still skipped
    # The quoted form disables shell expansion inside the body. The
    # tokenizer must skip the body regardless — quoting is a shell concern,
    # not a parser one.
    Given a hook input with the bash command from "heredoc_quoted_with_git_inside.txt"
    When I run the bundled redirect hook with that input
    Then the hook exit code is 0
    And the hook stdout is empty
    And the hook stderr is empty

  @ISSUE-238 @not_implemented
  Scenario: Tokenizer resumes parsing after the heredoc terminator
    # Catches TWO failure modes at once:
    #   1. Over-eager-skip: parser swallows everything from "<<EOF" to
    #      end-of-input. Would emit no advice — fails the get_change_manifest
    #      assertion.
    #   2. Heredoc-ignored: parser doesn't recognize `<<EOF` syntax and
    #      tokenizes everything as one stream. Would emit advice for
    #      get_commit_history (from `git log a..b` inside the body) AND
    #      get_change_manifest (from the post-EOF line) — fails the
    #      no-advice-for-get_commit_history assertion.
    # The bait `git log a..b` inside the heredoc body forces the parser
    # to actually skip the body (not just ignore heredoc syntax) AND
    # resume after the closing tag.
    Given a hook input with the bash command from "heredoc_then_git_diff.txt"
    When I run the bundled redirect hook with that input
    Then the hook exit code is 0
    And the hook stdout is JSON containing redirect advice for "get_change_manifest"
    And the hook stdout does not contain redirect advice for "get_commit_history"

  # ------------------------------------------------------------------------
  # W4: Install-hooks subcommand + bundled hook script (#239)
  #
  # `git-prism hooks install --scope <user|project|local>` writes a
  # PreToolUse entry to the corresponding settings.json with a sentinel
  # `id: git-prism-bash-redirect-v1`, and copies the hook script + Python
  # helper alongside it. The end-to-end contract (exit code, stderr, stdout
  # JSON) of the bundled hook is exercised against the same shapes from W3.
  # ------------------------------------------------------------------------

  @ISSUE-239 @not_implemented
  Scenario: "hooks install --scope user" writes the expected entry to ~/.claude/settings.json
    Given an isolated HOME with an empty .claude directory
    When I install the redirect hook at user scope
    Then the hook exit code is 0
    And the user settings file contains a PreToolUse entry with id "git-prism-bash-redirect-v1"
    And the user hooks directory contains a "git-prism-redirect.sh" script

  @ISSUE-239 @not_implemented
  Scenario: "hooks install --scope project" writes to <repo>/.claude/settings.json
    Given an isolated HOME with an empty .claude directory
    And a temporary git repository as the working directory
    When I install the redirect hook at project scope in the repo
    Then the hook exit code is 0
    And the project settings file contains a PreToolUse entry with id "git-prism-bash-redirect-v1"
    And the project hooks directory contains a "git-prism-redirect.sh" script

  @ISSUE-239 @not_implemented
  Scenario: Re-running "hooks install --scope user" is idempotent
    # Triangulates the "no duplicate write" property two ways: the file
    # bytes are unchanged AND the PreToolUse array length is unchanged.
    # Either alone could be fooled by a writer that re-orders keys but
    # appends a duplicate entry; together they pin the contract.
    Given an isolated HOME with an empty .claude directory
    When I install the redirect hook at user scope
    And I capture the user settings file sha256
    And I capture the user settings PreToolUse length
    And I install the redirect hook at user scope
    Then the user settings file sha256 is unchanged
    And the user settings PreToolUse length is unchanged

  @ISSUE-239 @not_implemented
  Scenario: "hooks uninstall --scope user" removes only this command's entries
    Given an isolated HOME with an empty .claude directory
    And the user settings file contains an unrelated PreToolUse entry with id "user-custom-hook"
    When I install the redirect hook at user scope
    And I uninstall the redirect hook at user scope
    Then the hook exit code is 0
    And the user settings file contains a PreToolUse entry with id "user-custom-hook"
    And the user settings file does not contain a PreToolUse entry with id "git-prism-bash-redirect-v1"

  @ISSUE-239 @not_implemented
  Scenario Outline: Bundled hook redirects on tokenizer-recognized shapes (end-to-end)
    Given an isolated HOME with the bundled hook installed at user scope
    And a hook input with bash command "<command>"
    When I run the installed user-scope hook with that input
    Then the hook exit code is <exit>
    And the hook stdout matches "<stdout_match>"

    Examples:
      | command                          | exit | stdout_match                                  |
      | git diff main..HEAD              | 0    | get_change_manifest                           |
      | cd /tmp && git diff main..HEAD   | 0    | get_change_manifest                           |
      | (git log main..HEAD)             | 0    | get_commit_history                            |
      | git diff $BASE..HEAD             | 0    | get_change_manifest                           |

  @ISSUE-239 @not_implemented
  Scenario: Bundled hook hard-blocks "gh pr diff" with exit code 2 and advisory text
    # Decision logic lives in the bundled hook, not the tokenizer — `gh pr
    # diff` is a hard-block target because the redirect is not advisory:
    # the agent must use `get_change_manifest` instead. Hence #239, not #238.
    Given a hook input with bash command "gh pr diff 123"
    When I run the bundled redirect hook with that input
    Then the hook exit code is 2
    And the hook stderr contains "git-prism"
    And the hook stderr contains "get_change_manifest"

  @ISSUE-239 @not_implemented
  Scenario: Bundled hook hard-blocks "mcp__github__get_commit" with exit code 2
    # The MCP-shaped GitHub tools have the same structured-data overlap as
    # `gh pr diff` and are hard-blocked for the same reason. The hook must
    # detect them via the `tool_name` field, not just `tool_input.command`.
    Given a hook input with bash command "mcp__github__get_commit owner=foo repo=bar sha=abc"
    When I run the bundled redirect hook with that input
    Then the hook exit code is 2
    And the hook stderr contains "git-prism"

  @ISSUE-239 @not_implemented
  Scenario: Empty stdin is a no-op (exit 0, no stdout, no stderr)
    # If Claude Code invokes the hook without sending a payload (a
    # non-Bash tool, for instance), it must be a silent no-op. Exit 0 so
    # the wider workflow keeps running.
    When I run the bundled redirect hook with empty stdin
    Then the hook exit code is 0
    And the hook stdout is empty
    And the hook stderr is empty

  @ISSUE-239 @not_implemented
  Scenario: Whitespace-only stdin is treated as empty (silent exit 0)
    # Whitespace-only payloads (a stray newline, a tab, multiple newlines)
    # must NOT be treated as malformed JSON — they are functionally empty.
    # Without this scenario, the boundary between "empty" and "malformed"
    # is undefined and a fail-open impl could emit a stderr warning on
    # every harmless newline.
    When I run the bundled redirect hook with stdin "\n  \n"
    Then the hook exit code is 0
    And the hook stdout is empty
    And the hook stderr is empty

  @ISSUE-239 @not_implemented
  Scenario: Malformed JSON on stdin fails open with a one-line warning
    # Per ADR Decision 6: the hook never blocks on its own malfunction.
    # A garbage payload triggers a single-line stderr warning, exit 0.
    When I run the bundled redirect hook with stdin "this is not json {"
    Then the hook exit code is 0
    And the hook stdout is empty
    And the hook stderr contains "git-prism-redirect"
    And the hook stderr contains "malformed JSON"
    And the hook stderr is at most 1 line

  @ISSUE-239 @not_implemented
  Scenario: Missing python3 fails open with a documented stderr line
    # Per ADR Decision 6: if the script can't find a python3 interpreter
    # on PATH, it must announce that on stderr and exit 0 — never block
    # the agent because of a tooling gap on the host.
    #
    # Implementation note for #239: the bundled hook MUST use an absolute
    # shebang (`#!/bin/bash`), not `#!/usr/bin/env bash`. With PATH set to
    # `/nonexistent` the env-form shebang would also fail to find `bash`
    # and the hook would never run — the test would pass for the wrong
    # reason. The absolute shebang is the load-bearing convention.
    Given a hook input with bash command "git diff main..HEAD"
    When I run the bundled redirect hook with that input and PATH "/nonexistent"
    Then the hook exit code is 0
    And the hook stdout is empty
    And the hook stderr contains "python3 not found on PATH"
    And the hook stderr contains "skipping redirect"

  @ISSUE-239 @not_implemented
  Scenario: Re-install with a stale script path updates the entry in place
    # Path 3 from the ADR: settings.json already has a v1 entry but its
    # `command` field points at an old absolute path (the user moved
    # ~/.claude or upgraded an old install). A fresh install rewrites the
    # entry — does not append a duplicate.
    Given an isolated HOME with an empty .claude directory
    And the user settings file contains a "git-prism-bash-redirect-v1" entry pointing to "/old/stale/path/git-prism-redirect.sh"
    When I install the redirect hook at user scope
    Then the hook exit code is 0
    And the user settings file contains exactly one PreToolUse entry with id "git-prism-bash-redirect-v1"
    And the user settings file PreToolUse entry "git-prism-bash-redirect-v1" command does not contain "/old/stale/path"
    And the install stdout or stderr mentions "updated"

  @ISSUE-239 @not_implemented
  Scenario: User-edited entry is preserved by default and install logs a skip
    # Path 4a from the ADR: respect user customization. We detect drift by
    # checking the canonical sentinel fields; if `command` differs, skip.
    Given an isolated HOME with an empty .claude directory
    And the user settings file contains a "git-prism-bash-redirect-v1" entry with command "echo HAND-EDITED"
    When I install the redirect hook at user scope
    Then the hook exit code is 0
    And the user settings file PreToolUse entry "git-prism-bash-redirect-v1" command equals "echo HAND-EDITED"
    And the install stdout or stderr mentions "skipped"

  @ISSUE-239 @not_implemented
  Scenario: "--force" overwrites a user-edited entry with the canonical entry
    # Path 4b from the ADR: explicit opt-out of the safety check.
    Given an isolated HOME with an empty .claude directory
    And the user settings file contains a "git-prism-bash-redirect-v1" entry with command "echo HAND-EDITED"
    When I install the redirect hook at user scope with "--force"
    Then the hook exit code is 0
    And the user settings file PreToolUse entry "git-prism-bash-redirect-v1" command does not equal "echo HAND-EDITED"
    And the user settings file PreToolUse entry "git-prism-bash-redirect-v1" command contains "git-prism-redirect.sh"

  @ISSUE-239 @not_implemented
  Scenario: Mixed-version downgrade is refused
    # Per ADR Decision 3: never downgrade. If a v2 entry already exists
    # and this binary writes v1, we abort with a clear remediation
    # message and leave settings.json untouched.
    Given an isolated HOME with an empty .claude directory
    And the user settings file contains a "git-prism-bash-redirect-v2" entry with command "echo v2"
    When I install the redirect hook at user scope
    Then the hook exit code is not 0
    And the hook stderr contains "git-prism-bash-redirect-v2"
    And the hook stderr contains "this binary writes v1"
    And the hook stderr contains "uninstall"
    And the user settings file PreToolUse entry "git-prism-bash-redirect-v2" command equals "echo v2"
    And the user settings file does not contain a PreToolUse entry with id "git-prism-bash-redirect-v1"

  @ISSUE-239 @not_implemented
  Scenario: "--scope local" writes to settings.local.json, not settings.json
    Given an isolated HOME with an empty .claude directory
    And a temporary git repository as the working directory
    When I install the redirect hook at local scope in the repo
    Then the hook exit code is 0
    And the project local settings file contains a PreToolUse entry with id "git-prism-bash-redirect-v1"
    And the project settings file does not exist

  @ISSUE-239 @not_implemented
  Scenario: Cross-scope install warns about duplicate redirects and aborts on "n"
    # User has user-scope already; trying project-scope must prompt because
    # the result would be two redirect hooks firing on every Bash call.
    # Answer "n" — assert the project entry was NOT written.
    Given an isolated HOME with an empty .claude directory
    And a temporary git repository as the working directory
    And the redirect hook is installed at user scope
    When I run "hooks install --scope project" in the repo and answer "n"
    Then the hook exit code is 0
    And the hook stderr contains "already installed at user scope"
    And the hook stderr contains "duplicate redirects"
    And the project settings file does not exist

  @ISSUE-239 @not_implemented
  Scenario: Cross-scope install proceeds when the user answers "y"
    # Triangulates the cross-scope prompt — "n" rejects, "y" must accept.
    # Without this branch an impl that ignored the prompt entirely (always
    # aborting) would pass the "n" scenario above and silently break the
    # "I really do want both scopes" use case.
    Given an isolated HOME with an empty .claude directory
    And a temporary git repository as the working directory
    And the redirect hook is installed at user scope
    When I run "hooks install --scope project" in the repo and answer "y"
    Then the hook exit code is 0
    And the project settings file contains a PreToolUse entry with id "git-prism-bash-redirect-v1"

  @ISSUE-239 @not_implemented
  Scenario Outline: "hooks status" reports installed scopes and versions
    Given an isolated HOME with an empty .claude directory
    And a temporary git repository as the working directory
    And the redirect hook install state is "<state>"
    When I run "hooks status" in the repo
    Then the hook exit code is 0
    And the hook stdout contains "<expected>"

    Examples:
      | state              | expected                            |
      | none               | not installed                       |
      | user-only          | user: git-prism-bash-redirect-v1    |
      | project-only       | project: git-prism-bash-redirect-v1 |

  @ISSUE-239 @not_implemented
  Scenario: "hooks status" reports BOTH scopes when both are installed
    # The Scenario Outline above can pass for "user-and-project" with a
    # substring match on either user: or project: alone. This scenario
    # forces an AND — both lines must appear — so an impl that prints
    # only the first installed scope cannot fake it.
    Given an isolated HOME with an empty .claude directory
    And a temporary git repository as the working directory
    And the redirect hook install state is "user-and-project"
    When I run "hooks status" in the repo
    Then the hook exit code is 0
    And the hook stdout contains both "user: git-prism-bash-redirect-v1" and "project: git-prism-bash-redirect-v1"

  @ISSUE-239 @not_implemented
  Scenario: "--dry-run" prints a diff but does not write settings.json
    Given an isolated HOME with an empty .claude directory
    When I install the redirect hook at user scope with "--dry-run"
    Then the hook exit code is 0
    And the user settings file does not exist
    And the hook stdout contains "git-prism-bash-redirect-v1"

  # ------------------------------------------------------------------------
  # W5: review_change MCP tool (#240)
  #
  # `review_change(base_ref, head_ref)` returns a single combined payload
  # with `manifest` + `function_context` sub-responses, sharing one token
  # budget split 40/60 per ADR-0008. Pagination on either sub-response is
  # exposed via the same opaque cursor scheme as the existing tools.
  # ------------------------------------------------------------------------

  @ISSUE-240
  Scenario: review_change returns combined manifest + function_context payload
    Given a git repository with two commits
    And the git-prism MCP server is running over stdio
    When I call the "review_change" tool with base "HEAD~1" and head "HEAD"
    Then the response has key "manifest"
    And the response has key "function_context"
    And the response value "manifest.summary.total_files_changed" is greater than 0

  @ISSUE-240
  Scenario Outline: review_change splits its token budget 40/60 between sub-responses
    # Two budget values triangulate the split. A hard-coded 1638/2458 pair
    # would pass at 4096 but fail at 16384 — the test must show the split
    # scales with the input.
    Given a git repository with two commits
    And the git-prism MCP server is running over stdio
    When I call the "review_change" tool with base "HEAD~1", head "HEAD", and max_response_tokens <budget>
    Then the response key "manifest.metadata.budget_tokens" is <manifest_budget>
    And the response key "function_context.metadata.budget_tokens" is <context_budget>

    Examples:
      | budget | manifest_budget | context_budget |
      | 4096   | 1638            | 2458           |
      | 16384  | 6553            | 9830           |

  @ISSUE-240
  Scenario: review_change paginates and the cursor returns a different page
    # Triangulates pagination: a hardcoded "always emit cursor X" would
    # pass the existence check but fail the second-page diff. We assert
    # both that a cursor exists AND that following it returns a different
    # set of files than the first page.
    Given a git repository with many changed files
    And the git-prism MCP server is running over stdio
    When I call the "review_change" tool with base "HEAD~1", head "HEAD", and page_size 5
    Then at least one sub-response in the result has a non-null "next_cursor"
    And following the manifest "next_cursor" returns a different set of files than page 1

  @ISSUE-240
  Scenario: review_change tool description includes comparative framing vs git diff
    Given the git-prism MCP server is running over stdio
    When I send a "tools/list" JSON-RPC request
    Then the description for "review_change" mentions "git diff"
