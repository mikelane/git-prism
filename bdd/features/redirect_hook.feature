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
  Scenario: All four MCP tool descriptions include comparative framing vs raw git
    Given the git-prism MCP server is running over stdio
    When I send a "tools/list" JSON-RPC request
    Then the description for "get_change_manifest" mentions "git diff"
    And the description for "get_commit_history" mentions "git log"
    And the description for "get_file_snapshots" mentions "git show"
    And the description for "get_function_context" mentions "git log -S"

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
  Scenario: Variable expansion "git diff $BASE..HEAD" is recognized without expansion
    Given a hook input with bash command "git diff $BASE..HEAD"
    When I run the bundled redirect hook with that input
    Then the hook exit code is 0
    And the hook stdout is JSON containing redirect advice for "get_change_manifest"
    And the hook does not attempt to expand "$BASE"

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
    Then the exit code is 0
    And the user settings file contains a PreToolUse entry with id "git-prism-bash-redirect-v1"
    And the user hooks directory contains a "git-prism-redirect.sh" script

  @ISSUE-239 @not_implemented
  Scenario: "hooks install --scope project" writes to <repo>/.claude/settings.json
    Given an isolated HOME with an empty .claude directory
    And a temporary git repository as the working directory
    When I install the redirect hook at project scope in the repo
    Then the exit code is 0
    And the project settings file contains a PreToolUse entry with id "git-prism-bash-redirect-v1"
    And the project hooks directory contains a "git-prism-redirect.sh" script

  @ISSUE-239 @not_implemented
  Scenario: Re-running "hooks install --scope user" is idempotent
    Given an isolated HOME with an empty .claude directory
    When I install the redirect hook at user scope
    And I capture the user settings file sha256
    And I install the redirect hook at user scope
    Then the user settings file sha256 is unchanged

  @ISSUE-239 @not_implemented
  Scenario: "hooks uninstall --scope user" removes only this command's entries
    Given an isolated HOME with an empty .claude directory
    And the user settings file contains an unrelated PreToolUse entry with id "user-custom-hook"
    When I install the redirect hook at user scope
    And I uninstall the redirect hook at user scope
    Then the exit code is 0
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
      | cd /tmp && git diff main..HEAD   | 0    | get_change_manifest                           |
      | (git log main..HEAD)             | 0    | get_commit_history                            |
      | git diff $BASE..HEAD             | 0    | get_change_manifest                           |

  @ISSUE-239 @not_implemented
  Scenario: Bundled hook hard-blocks "gh pr diff" with exit code 2 and advisory text
    Given a hook input with bash command "gh pr diff 123"
    When I run the bundled redirect hook with that input
    Then the hook exit code is 2
    And the hook stderr contains "git-prism"
    And the hook stderr contains "get_change_manifest"

  # ------------------------------------------------------------------------
  # W5: review_change MCP tool (#240)
  #
  # `review_change(base_ref, head_ref)` returns a single combined payload
  # with `manifest` + `function_context` sub-responses, sharing one token
  # budget split 40/60 per ADR-0008. Pagination on either sub-response is
  # exposed via the same opaque cursor scheme as the existing tools.
  # ------------------------------------------------------------------------

  @ISSUE-240 @not_implemented
  Scenario: review_change returns combined manifest + function_context payload
    Given a git repository with two commits
    And the git-prism MCP server is running over stdio
    When I call the "review_change" tool with base "HEAD~1" and head "HEAD"
    Then the response has key "manifest"
    And the response has key "function_context"
    And the response value "manifest.summary.total_files_changed" is greater than 0

  @ISSUE-240 @not_implemented
  Scenario: review_change splits its token budget 40/60 between sub-responses
    Given a git repository with two commits
    And the git-prism MCP server is running over stdio
    When I call the "review_change" tool with base "HEAD~1", head "HEAD", and max_response_tokens 4096
    Then the response key "manifest.metadata.budget_tokens" is 1638
    And the response key "function_context.metadata.budget_tokens" is 2458

  @ISSUE-240 @not_implemented
  Scenario: review_change paginates when manifest or context exceeds page size
    Given a git repository with many changed files
    And the git-prism MCP server is running over stdio
    When I call the "review_change" tool with base "HEAD~1", head "HEAD", and page_size 5
    Then at least one sub-response in the result has a non-null "next_cursor"

  @ISSUE-240 @not_implemented
  Scenario: review_change tool description includes comparative framing vs git diff
    Given the git-prism MCP server is running over stdio
    When I send a "tools/list" JSON-RPC request
    Then the description for "review_change" mentions "git diff"
