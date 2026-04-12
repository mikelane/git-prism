@ISSUE-45
Feature: OpenTelemetry metrics and traces for the MCP server

  The git-prism MCP server emits OpenTelemetry metrics and traces over OTLP
  HTTP/protobuf when configured via environment variables. Telemetry is off
  by default. Exports must never include raw repo paths, commit SHAs, or
  literal ref names.

  Background:
    Given a mock OTLP collector is running

  @ISSUE-46
  Scenario: Server starts with no telemetry env vars and makes no network connections
    Given no telemetry environment variables are set
    When I start "git-prism serve" and send an MCP initialize request
    And I wait 3 seconds for any exports
    Then the mock collector received zero trace exports
    And the mock collector received zero metric exports

  @ISSUE-46
  Scenario: Server exports baseline sessions.started counter when GIT_PRISM_OTLP_ENDPOINT is set
    Given GIT_PRISM_OTLP_ENDPOINT points at the mock collector
    When I start "git-prism serve" and send an MCP initialize request
    And I wait 5 seconds for any exports
    Then the mock collector received a metric named "git_prism.sessions.started"
    And the "git_prism.sessions.started" counter value is at least 1

  @ISSUE-48
  Scenario: get_change_manifest invocation produces a trace with expected sub-spans
    Given GIT_PRISM_OTLP_ENDPOINT points at the mock collector
    And a git repository with two commits
    When I start "git-prism serve" against that repo and call "get_change_manifest"
    And I wait 5 seconds for any exports
    Then the mock collector received a span named "mcp.tool.get_change_manifest"
    And that span has a child span named "git.open_repo"
    And that span has a child span named "git.diff_commits"

  @ISSUE-47
  Scenario: get_change_manifest emits tokens_estimated histogram with tool label
    Given GIT_PRISM_OTLP_ENDPOINT points at the mock collector
    And a git repository with two commits
    When I start "git-prism serve" against that repo and call "get_change_manifest"
    And I wait 5 seconds for any exports
    Then the mock collector received a metric named "git_prism.response.tokens_estimated"
    And that metric has a data point with label "tool" equal to "get_change_manifest"

  @ISSUE-47 @ISSUE-48
  Scenario: Errors are recorded on spans and increment errors.total counter
    Given GIT_PRISM_OTLP_ENDPOINT points at the mock collector
    And a git repository with two commits
    When I start "git-prism serve" against that repo and call "get_change_manifest" with an invalid ref
    And I wait 5 seconds for any exports
    Then the mock collector received a span named "mcp.tool.get_change_manifest"
    And that span has status "error"
    And the mock collector received a metric named "git_prism.errors.total"

  @ISSUE-49
  Scenario: Repo paths and commit SHAs never appear as raw strings in exports
    Given GIT_PRISM_OTLP_ENDPOINT points at the mock collector
    And a git repository with two commits
    When I start "git-prism serve" against that repo and call "get_change_manifest"
    And I wait 5 seconds for any exports
    Then no exported attribute contains the raw repo path
    And no exported attribute contains any commit SHA from the repo

  @ISSUE-49
  Scenario: Ref patterns are normalized to bounded enum values
    Given GIT_PRISM_OTLP_ENDPOINT points at the mock collector
    And a git repository with two commits
    When I start "git-prism serve" against that repo and call "get_change_manifest"
    And I wait 5 seconds for any exports
    Then every "ref_base" span attribute value is in {"worktree", "single_commit", "range_double_dot", "range_triple_dot", "branch", "sha"}
    And every "ref_head" span attribute value is in {"worktree", "single_commit", "range_double_dot", "range_triple_dot", "branch", "sha"}

  @ISSUE-48
  Scenario: gix operations produce sub-spans
    Given GIT_PRISM_OTLP_ENDPOINT points at the mock collector
    And a git repository with two commits
    When I start "git-prism serve" against that repo and call "get_change_manifest"
    And I wait 5 seconds for any exports
    Then the mock collector received a span named "git.open_repo"
    And the mock collector received a span named "git.resolve_ref"

  @ISSUE-48
  Scenario: tree-sitter parse operations produce sub-spans with language label
    Given GIT_PRISM_OTLP_ENDPOINT points at the mock collector
    And a git repository with two commits
    When I start "git-prism serve" against that repo and call "get_change_manifest"
    And I wait 5 seconds for any exports
    Then the mock collector received a span named "treesitter.parse"
    And that span has an attribute "language" equal to "rust"
