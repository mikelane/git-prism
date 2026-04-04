@ISSUE-15
Feature: Language listing

  Scenario: Lists all supported languages
    When I run "git-prism languages"
    Then the exit code is 0
    And the output contains "go"
    And the output contains "python"
    And the output contains "typescript"
    And the output contains "javascript"
    And the output contains "rust"
