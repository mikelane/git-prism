@not_implemented
Feature: Content-aware function diffs

  Function change detection should compare function bodies, not line positions.
  This eliminates false positives from reordering, detects body-only changes,
  and recognizes renames.

  Rule: Moved-but-unchanged functions are not reported

    Scenario: Reordering functions produces no function changes
      Given a git repository where functions are reordered between commits
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the file "lib.rs" has zero function changes

    Scenario: Inserting a new function above an existing one
      Given a git repository where a new function is added above an existing one
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the only function change for "lib.rs" is added "new_function"

  Rule: Body-only changes are detected

    Scenario: Changing a function body without changing its signature
      Given a git repository where a function body is modified
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the file "lib.rs" has a function change "compute" with type "modified"

  Rule: Renames are detected by matching body hashes

    Scenario: Renaming a function is reported as a single rename
      Given a git repository where a function is renamed
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the file "lib.rs" has a function change "new_name" with type "renamed"
      And the renamed function "new_name" has old_name "old_name"

    Scenario: Renaming and modifying a function is reported as deleted plus added
      Given a git repository where a function is renamed and modified
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the file "lib.rs" has a function change "old_name" with type "deleted"
      And the file "lib.rs" has a function change "new_name" with type "added"
