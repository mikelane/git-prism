@ISSUE-18
Feature: Helpful error messages

  Background:
    Given a git repository with two commits

  Scenario: Invalid ref produces clear error
    When I run "git-prism manifest nonexistent-branch..HEAD"
    Then the exit code is not 0
    And the output does not contain "panicked"
    And the output does not contain "RUST_BACKTRACE"

  Scenario: Non-repo path produces clear error
    When I run "git-prism manifest HEAD~1..HEAD" in "/tmp"
    Then the exit code is not 0
    And the output contains "repository"
