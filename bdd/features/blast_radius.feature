@ISSUE-126
Feature: Blast radius scoring for function context

  The function context tool should compute a blast radius score for
  each changed function, classifying risk based on production caller
  count and test coverage. This eliminates mechanical token-burning
  work for agents that would otherwise count callers manually.

  # ---------------------------------------------------------------
  Rule: Every function context entry includes blast radius
  # ---------------------------------------------------------------

    @ISSUE-132
    Scenario: blast_radius object is present in function context output
      Given a git repository with function context test fixtures
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And each function context entry has a "blast_radius" object

    @ISSUE-132
    Scenario: blast_radius has required fields
      Given a git repository with function context test fixtures
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And each blast_radius has fields "production_callers" and "test_callers" and "has_tests" and "risk"

  # ---------------------------------------------------------------
  Rule: Risk classification matches the defined table
  # ---------------------------------------------------------------

    @ISSUE-133
    Scenario: function with no production callers has risk "none"
      Given a git repository with function context test fixtures
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the function "unused_func" has blast_radius risk "none"

    @ISSUE-133
    Scenario: function with callers and tests has appropriate risk
      Given a git repository with function context test fixtures
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the function "calculate" has blast_radius with has_tests true
      And the function "calculate" has blast_radius risk "low"

    @ISSUE-133
    Scenario: function with callers but no tests has higher risk
      Given a git repository with a blast radius no-tests fixture
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the function "process_data" has blast_radius with has_tests false
      And the function "process_data" has blast_radius risk not "none"
