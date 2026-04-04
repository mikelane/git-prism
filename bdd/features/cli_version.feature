@ISSUE-19
Feature: CLI version output

  Scenario: git-prism reports its version
    When I run "git-prism --version"
    Then the exit code is 0
    And the output contains "git-prism"
