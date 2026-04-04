@ISSUE-15
Feature: File snapshots via CLI

  Background:
    Given a git repository with two commits

  Scenario: Snapshot outputs valid JSON with before/after content
    When I run "git-prism snapshot HEAD~1..HEAD --paths main.rs"
    Then the exit code is 0
    And the output is valid JSON
    And the JSON has key "files"
    And the JSON value "token_estimate" is greater than 0
