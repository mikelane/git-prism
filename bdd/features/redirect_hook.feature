# Surviving scenarios from the original redirect-hook epic (#234).
#
# The redirect hook (bash_redirect_hook.py / git-prism-redirect.sh) was
# removed in v0.9.0 (ADR-0011). Scenarios that drove those deleted files
# have been removed. What remains tests surviving functionality:
#
#   @ISSUE-237  -- MCP tool descriptions (server.rs, unaffected by hook removal)
#   @ISSUE-239  -- hooks uninstall and hooks status (still present for legacy cleanup)
#   @ISSUE-240  -- review_change MCP tool (server.rs, unaffected by hook removal)

Feature: MCP tool descriptions and hooks legacy cleanup

  # ------------------------------------------------------------------------
  # W2: Tool description rewrites (#237)
  #
  # The MCP tool doc comments must include comparative framing vs the raw
  # git equivalent. The assertion is end-to-end: shell out to `git-prism
  # serve` over stdio, send a JSON-RPC `tools/list` request, and read the
  # description fields. This scenario tests server.rs — not the hook.
  # ------------------------------------------------------------------------

  @ISSUE-237
  Scenario: All five MCP tool descriptions include comparative framing vs raw git
    Given the git-prism MCP server is running over stdio
    When I send a "tools/list" JSON-RPC request
    Then the description for "get_change_manifest" mentions "git diff"
    And the description for "get_commit_history" mentions "git log"
    And the description for "get_file_snapshots" mentions "git show"
    And the description for "get_function_context" mentions "git log -S"
    And the description for "review_change" mentions "git diff"

  # ------------------------------------------------------------------------
  # W4: hooks uninstall and status (#239)
  #
  # install is removed (redirect hook is gone). uninstall and status remain
  # so users who had the old hook can clean it up.
  # ------------------------------------------------------------------------

  @ISSUE-239
  Scenario: "hooks uninstall --scope user" removes only this command's entries
    Given an isolated HOME with an empty .claude directory
    And the user settings file contains an unrelated PreToolUse entry with id "user-custom-hook"
    And the user settings file contains a "git-prism-bash-redirect-v1" entry pointing to "/old/path/git-prism-redirect.sh"
    When I uninstall the redirect hook at user scope
    Then the hook exit code is 0
    And the user settings file contains a PreToolUse entry with id "user-custom-hook"
    And the user settings file does not contain a PreToolUse entry with id "git-prism-bash-redirect-v1"

  @ISSUE-239
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

  @ISSUE-239
  Scenario: "hooks status" reports BOTH scopes when both are installed
    Given an isolated HOME with an empty .claude directory
    And a temporary git repository as the working directory
    And the redirect hook install state is "user-and-project"
    When I run "hooks status" in the repo
    Then the hook exit code is 0
    And the hook stdout contains both "user: git-prism-bash-redirect-v1" and "project: git-prism-bash-redirect-v1"

  # ------------------------------------------------------------------------
  # W5: review_change MCP tool (#240)
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
