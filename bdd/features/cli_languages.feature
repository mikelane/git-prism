@ISSUE-15
Feature: Language listing

  Scenario: Lists all supported languages
    When I run "git-prism languages"
    Then the exit code is 0
    And the languages list includes "go"
    And the languages list includes "python"
    And the languages list includes "typescript"
    And the languages list includes "javascript"
    And the languages list includes "rust"
