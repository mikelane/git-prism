@ISSUE-264
Feature: Filter gitlink entries from change manifest output
  git-prism's change manifest should not include gitlink entries (mode
  0o160000) because they represent submodule references, not real file
  changes. Real binary files must still appear in the manifest.

  Rule: Committed submodule additions are filtered

    @not_implemented
    Scenario: Committed submodule addition is filtered
      Given a git repository with one commit
      And a submodule "sub" is added and committed
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And no file has path "sub"

  Rule: Working-tree submodule changes are filtered

    @not_implemented
    Scenario: Working-tree submodule change is filtered
      Given a git repository with one commit
      And a submodule "sub" is staged
      When I run "git-prism manifest HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And no file has path "sub"

  Rule: Real binary files are preserved after filter

    @not_implemented
    Scenario: Real binary file is preserved after filter
      Given a git repository with one commit
      And a binary file "image.png" is added and committed
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And at least one file has path "image.png"

  Rule: Deleted gitlinks are filtered

    @not_implemented
    Scenario: Deleted gitlink is filtered
      Given a git repository with one commit
      And a submodule "sub" is added and committed
      And the submodule "sub" is removed and committed
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And no file has path "sub"

  Rule: Mixed diffs show only real files

    @not_implemented
    Scenario: Mixed diff shows only real files
      Given a git repository with one commit
      And a binary file "image.png" is added and committed
      And a submodule "sub" is added and committed
      When I run "git-prism manifest HEAD~2..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And at least one file has path "image.png"
      And no file has path "sub"
