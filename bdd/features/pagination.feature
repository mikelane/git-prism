@ISSUE-74
Feature: Paginated manifest and history responses

  Agents working in large repositories need access to all changed files,
  not just the first page. The manifest and history tools support cursor-based
  pagination so agents can traverse results at their own pace, stopping
  when they find what they need.

  Background:
    Given a git repository with 150 changed files

  @ISSUE-75 @ISSUE-76 @ISSUE-78 @not_implemented
  Scenario: First page returns files with a continuation cursor
    When the agent requests a change manifest with page size 50
    Then the response contains exactly 50 files
    And the response includes a pagination cursor
    And the pagination shows 150 total files

  @ISSUE-75 @ISSUE-76 @ISSUE-78 @not_implemented
  Scenario: Following the cursor retrieves the next page
    Given the agent has received the first page of a manifest with page size 50
    When the agent requests the next page using the cursor
    Then the response contains exactly 50 files
    And none of the files overlap with the first page

  @ISSUE-76 @ISSUE-78 @not_implemented
  Scenario: Traversing all pages collects every changed file
    When the agent pages through the entire manifest with page size 50
    Then all 150 changed files are collected across 3 pages
    And no files are duplicated

  @ISSUE-76 @not_implemented
  Scenario: Summary reflects all files regardless of page
    When the agent pages through the entire manifest with page size 50
    Then the summary reports 150 total files changed
    And the summary is identical on every page

  @ISSUE-75 @ISSUE-78 @not_implemented
  Scenario: Invalid cursor produces a clear error
    When the agent requests a manifest with an invalid cursor
    Then the response is an error
    And the error message mentions the cursor

  @ISSUE-75 @ISSUE-78 @not_implemented
  Scenario: Stale cursor after repository change produces an error
    Given the agent has received the first page of a manifest with page size 50
    And a new commit is added to the repository
    When the agent requests the next page using the cursor
    Then the response is an error
    And the error message indicates the repository has changed

  @ISSUE-78 @not_implemented
  Scenario: Default request without pagination parameters returns first page
    When the agent requests a change manifest without pagination parameters
    Then the response contains at most 100 files
    And the response includes a pagination cursor

  @ISSUE-78 @not_implemented
  Scenario: Custom page size is respected
    When the agent requests a change manifest with page size 25
    Then the response contains exactly 25 files

  @ISSUE-78 @not_implemented
  Scenario: Page size is clamped to maximum
    When the agent requests a change manifest with page size 999
    Then the response contains at most 500 files

  @ISSUE-76 @not_implemented
  Scenario: Last page has no continuation cursor
    When the agent pages through the entire manifest with page size 50
    Then the last page has no pagination cursor

  @ISSUE-76 @not_implemented
  Scenario: Small manifest fits in one page without cursor
    Given a git repository with 5 changed files
    When the agent requests a change manifest with page size 100
    Then the response contains exactly 5 files
    And the response has no pagination cursor

  @ISSUE-77 @ISSUE-78 @not_implemented
  Scenario: History responses support pagination across commits
    Given a git repository with 20 sequential commits
    When the agent requests commit history with page size 5
    Then the response contains exactly 5 commits
    And the response includes a pagination cursor

  @ISSUE-76 @not_implemented
  Scenario: Working tree mode supports pagination
    Given a git repository with 150 unstaged changed files
    When the agent requests a working tree manifest with page size 50
    Then the response contains exactly 50 files
    And the response includes a pagination cursor

  @ISSUE-79 @not_implemented
  Scenario: CLI outputs all files without manual pagination
    Given a git repository with 150 changed files
    When the user runs the manifest CLI command
    Then the output contains all 150 files in a single JSON response
