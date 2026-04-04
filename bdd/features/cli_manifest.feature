@ISSUE-15
Feature: Change manifest via CLI

  Background:
    Given a git repository with two commits

  Scenario: Manifest outputs valid JSON
    When I run "git-prism manifest HEAD~1..HEAD"
    Then the exit code is 0
    And the output is valid JSON

  Scenario: Manifest contains required structure
    When I run "git-prism manifest HEAD~1..HEAD"
    Then the JSON has key "metadata"
    And the JSON has key "summary"
    And the JSON has key "files"
    And the JSON value "summary.total_files_changed" is greater than 0

  Scenario: Manifest includes function analysis for supported languages
    When I run "git-prism manifest HEAD~1..HEAD"
    Then at least one file has "functions_changed" that is not null
