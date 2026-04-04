@ISSUE-17 @not_implemented
Feature: README documentation

  Scenario: README exists with installation instructions
    Given the project root directory
    Then the file "README.md" exists
    And the file "README.md" contains "install" (case insensitive)
    And the file "README.md" contains "get_change_manifest"
    And the file "README.md" contains "get_file_snapshots"
    And the file "README.md" contains "claude mcp add"
