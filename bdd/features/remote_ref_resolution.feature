@ISSUE-263
Feature: Actionable resolution for missing remote branches

  Background:
    Given a git repository with one commit

  Scenario: Missing head_ref produces JSON error with resolution field
    Given a bare remote repository "origin" with branch "feature/foo"
    And the remote "origin" is added but not fetched
    When I run "git-prism manifest main..feature/foo"
    Then the exit code is not 0
    And the JSON error contains "resolution" with value "git fetch origin feature/foo"

  Scenario: Missing base_ref produces JSON error with resolution field
    Given a bare remote repository "origin" with branch "feature/bar"
    And the remote "origin" is added but not fetched
    When I run "git-prism manifest feature/bar..HEAD"
    Then the exit code is not 0
    And the JSON error contains "resolution" with value "git fetch origin feature/bar"

  Scenario: Missing branch that does not exist anywhere produces plain error
    When I run "git-prism manifest main..totally-unknown"
    Then the exit code is not 0
    And the output does not contain "\"resolution\""

  Scenario: Bare SHA produces plain error without resolution field
    When I run "git-prism manifest main..deadbeef1234567890abcdef1234567890abcdef12"
    Then the exit code is not 0
    And the output does not contain "\"resolution\""
