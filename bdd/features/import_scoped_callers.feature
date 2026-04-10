@ISSUE-127
Feature: Import-aware caller scoping

  The function context tool should use import data to scope its caller
  scan, excluding files that have no import relationship with the changed
  module. This reduces false positives from leaf-name collisions and
  improves performance on large repos.

  # ---------------------------------------------------------------
  Rule: Callers from non-importing files are excluded
  # ---------------------------------------------------------------

    @ISSUE-140
    Scenario: Rust file without import is excluded from callers
      Given a Rust repo where only one file imports the changed module
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the function "compute" has only callers from importing files

    @ISSUE-140
    Scenario: Python file without import is excluded from callers
      Given a Python repo where only one file imports the changed module
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the function "compute" has only callers from importing files

    @ISSUE-140
    Scenario: TypeScript file without import is excluded from callers
      Given a TypeScript repo where only one file imports the changed module
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the function "compute" has only callers from importing files

  # ---------------------------------------------------------------
  Rule: Callers from importing files are included
  # ---------------------------------------------------------------

    @ISSUE-140
    Scenario: File that imports the changed module is included as caller
      Given a Rust repo where only one file imports the changed module
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context for function "compute" lists callers
      And the function "compute" has at least 1 caller

  # ---------------------------------------------------------------
  Rule: Unsupported languages fall back to full scan
  # ---------------------------------------------------------------

    @ISSUE-141
    Scenario: Ruby callers use full scan fallback
      Given a Ruby repo with callers of a changed function
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context for function "compute" lists callers

    @ISSUE-141
    Scenario: C callers use full scan fallback
      Given a C repo with callers of a changed function
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context for function "compute" lists callers

  # ---------------------------------------------------------------
  Rule: Same-directory files are always scanned
  # ---------------------------------------------------------------

    @ISSUE-140
    Scenario: Same-directory caller is included without explicit import
      Given a Go repo where a same-package file calls the changed function
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context for function "Compute" lists callers

  # ---------------------------------------------------------------
  Rule: Rust crate-root and pub use re-exports are matched
  # ---------------------------------------------------------------

    @ISSUE-140
    Scenario: Rust pub use re-export site is counted as caller
      Given a Rust repo where a file pub-re-exports the changed function
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the function "compute" has at least 1 caller

    @ISSUE-140
    Scenario: Rust crate-root function is reached via extern crate name
      Given a Rust repo with an integration test that uses the extern crate name
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the function "compute" has at least 1 caller

  # ---------------------------------------------------------------
  Rule: Python relative imports resolve against importer position
  # ---------------------------------------------------------------

    @ISSUE-140
    Scenario: Python relative import is counted as caller
      Given a Python repo where a sibling uses a relative import
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the function "compute" has at least 1 caller

  # ---------------------------------------------------------------
  Rule: Response indicates scoped vs fallback scan
  # ---------------------------------------------------------------

    @ISSUE-141
    Scenario: Scoped Rust function has scoping_mode "scoped"
      Given a Rust repo where only one file imports the changed module
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the function "compute" has scoping_mode "scoped"

    @ISSUE-141
    Scenario: Fallback Ruby function has scoping_mode "fallback"
      Given a Ruby repo with callers of a changed function
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the function "compute" has scoping_mode "fallback"
