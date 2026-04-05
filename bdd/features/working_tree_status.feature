@ISSUE-28 @not_implemented
Feature: Working tree status manifest
  git-prism can produce a change manifest comparing HEAD against the
  current working tree. This lets agents see staged and unstaged changes
  without requiring a commit, mirroring the information in "git status"
  but as structured JSON.

  Rule: Staged additions appear in the manifest

    Scenario: Manifest shows a staged file addition vs HEAD
      Given a git repository with one commit
      And a new file "utils.py" is staged with content
        """
        def helper():
            return 42
        """
      When I run "git-prism manifest HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the JSON has key "files"
      And the JSON value "summary.total_files_changed" is greater than 0
      And at least one file has path "utils.py"

  Rule: Unstaged modifications appear in the manifest

    Scenario: Manifest shows an unstaged modification vs HEAD
      Given a git repository with a committed file "app.py" containing
        """
        def main():
            print("hello")
        """
      And the file "app.py" is modified on disk to
        """
        def main():
            print("goodbye")
        """
      When I run "git-prism manifest HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the JSON has key "files"
      And at least one file has path "app.py"

  Rule: Staged and unstaged changes are distinguishable

    Scenario: Manifest distinguishes staged from unstaged changes
      Given a git repository with a committed file "config.txt" containing
        """
        setting=old
        """
      And the file "config.txt" is modified and staged with content
        """
        setting=staged
        """
      And the file "config.txt" is further modified on disk to
        """
        setting=unstaged
        """
      When I run "git-prism manifest HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the manifest contains both staged and unstaged changes
