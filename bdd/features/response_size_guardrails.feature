@ISSUE-212
Feature: Bounded tool responses
  The cheap read tools `get_change_manifest` and `get_function_context` advertise
  themselves as first- and second-resort calls that should fit comfortably inside
  an agent's context window. Without explicit response-size guardrails, a large
  change can produce tool outputs that exceed MCP context limits, forcing the
  client to dump the payload to disk and defeating the purpose of an agent-
  optimized server. This feature enforces a per-tool token budget, standardizes
  truncation metadata, and emits a bounded-cardinality metric when the budget
  is hit so operators can detect agents hammering the budget in production.

  Background:
    Given a git repository with a change affecting 20 files and 50 modified functions

  Rule: get_change_manifest defaults to a cheap response; function analysis is opt-in

    Scenario: Default manifest call omits function analysis
      When an agent requests the change manifest without opting in to function analysis
      Then the response lists every changed file with summary counts
      And the response omits per-function signature diffs

    Scenario: Opt-in manifest call includes function analysis
      When an agent requests the change manifest with function analysis enabled
      Then the response includes per-function signature diffs for files within the budget

  Rule: get_change_manifest clamps function detail to its token budget

    Scenario: Over-budget manifest trims function detail to signatures only
      When an agent requests the change manifest with function analysis enabled and a 2048 token budget
      Then the response token_estimate is at most 2048
      And the response metadata lists every file whose function detail was trimmed
      And the trimmed files preserve their function signatures

    Scenario: Over-budget manifest emits the token_budget truncation metric
      When an agent requests the change manifest with function analysis enabled and a 2048 token budget
      Then the git_prism.response.truncated metric records a token_budget event for get_change_manifest

    Scenario: Change manifest reports its payload size for budgeting follow-up calls
      When an agent requests the change manifest
      Then the response metadata includes a token_estimate for the payload

  Rule: get_function_context paginates over changed functions

    Scenario: Default function context call returns the first page with a next-page cursor
      When an agent requests function context without a cursor
      Then the response contains the first page of changed functions in deterministic order
      And the response metadata includes a next-page cursor

    Scenario: Cursor advances through remaining functions
      Given an agent has retrieved the first page of function context and received a next-page cursor
      When the agent requests function context with that cursor
      Then the response contains the next page of changed functions
      And no function appears in both pages

    Scenario: Agents can scope function context to a specific name list
      When an agent requests function context scoped to "function_0001" and "function_0002"
      Then the response contains exactly those two functions
      And functions outside the filter are not included

  Rule: get_function_context clamps caller and callee detail to its token budget

    Scenario: Over-budget context trims per-function caller and callee lists
      When an agent requests function context with a 512 token budget
      Then the response token_estimate is at most 512
      And at least one function entry is marked as truncated
      And the truncated entries have shortened caller and callee lists

    Scenario: Over-budget context emits the token_budget truncation metric
      When an agent requests function context with a 512 token budget
      Then the git_prism.response.truncated metric records a token_budget event for get_function_context

    Scenario: Function context reports its payload size for budgeting follow-up calls
      When an agent requests function context
      Then the response metadata includes a token_estimate for the payload

  Rule: Read tools stay within their token budget regardless of change size

    Scenario: Change manifest stays within the 8192 token budget on an extreme change
      Given a git repository with a change affecting 200 files and 1000 modified functions
      When an agent requests the change manifest with function analysis enabled
      Then the response token_estimate is at most 8192
      And the response metadata lists every file whose function detail was trimmed
      And the git_prism.response.truncated metric records a token_budget event for get_change_manifest

    Scenario: Function context stays within the 8192 token budget on an extreme change
      Given a git repository with a change affecting 200 files and 1000 modified functions
      When an agent requests function context without a function name filter
      Then the response token_estimate is at most 8192
      And at least one function entry is marked as truncated
      And the git_prism.response.truncated metric records a token_budget event for get_function_context
