@ISSUE-113
Feature: Function context for changed functions

  When an agent reviews a change, it needs to know the blast radius:
  which other functions call the changed ones, what the changed functions
  call, and which test files reference them. The function context tool
  provides this structured data so the agent never has to grep.

  Background:
    Given a git repository with function context test fixtures

  # ---------------------------------------------------------------
  Rule: Callers are listed with file and line references
  # ---------------------------------------------------------------

    @ISSUE-120
    Scenario: Changed function has callers in another file
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context for function "calculate" lists callers
      And a caller of "calculate" is in file "src/main.rs"
      And each caller entry has a line number

    @ISSUE-120
    Scenario: Changed function has callers in the same file
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context for function "helper" lists callers
      And a caller of "helper" is in file "src/lib.rs"

  # ---------------------------------------------------------------
  Rule: Callees are identified for changed functions
  # ---------------------------------------------------------------

    @ISSUE-120
    Scenario: Changed function calls other functions
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context for function "process" lists callees
      And a callee of "process" is "calculate"

  # ---------------------------------------------------------------
  Rule: Test file references are flagged separately
  # ---------------------------------------------------------------

    @ISSUE-120
    Scenario: Callers in test files are flagged as test references
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context for function "calculate" has test references
      And a test reference for "calculate" is in a file matching "test"

  # ---------------------------------------------------------------
  Rule: Functions with no callers report an empty list
  # ---------------------------------------------------------------

    @ISSUE-120
    Scenario: Changed function with no callers has empty caller list
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context for function "unused_func" has zero callers

  # ---------------------------------------------------------------
  Rule: Unsupported languages return null context
  # ---------------------------------------------------------------

    @ISSUE-120
    Scenario: Changed file in unsupported language has null context
      Given a git repository with an unsupported language change
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context result has a null entry for unsupported files

  # ---------------------------------------------------------------
  Rule: Multiple changed functions are handled independently
  # ---------------------------------------------------------------

    @ISSUE-120
    Scenario: Each changed function gets its own context entry
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context result has entries for at least 2 functions
      And each function context entry has callers and callees keys

  # ---------------------------------------------------------------
  Rule: Call extraction works across supported languages
  # ---------------------------------------------------------------

    @ISSUE-117
    Scenario: Python call sites are extracted
      Given a git repository with a Python function context fixture
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context for function "compute" lists callers

    @ISSUE-117
    Scenario: Go call sites are extracted
      Given a git repository with a Go function context fixture
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context for function "Compute" lists callers

    @ISSUE-117
    Scenario: TypeScript call sites are extracted
      Given a git repository with a TypeScript function context fixture
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context for function "compute" lists callers

    @ISSUE-118
    Scenario: Java call sites are extracted
      Given a git repository with a Java function context fixture
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context for function "Lib.compute" lists callers

    @ISSUE-118
    Scenario: C call sites are extracted
      Given a git repository with a C function context fixture
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context for function "compute" lists callers

    @ISSUE-119
    Scenario: Ruby call sites are extracted
      Given a git repository with a Ruby function context fixture
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context for function "compute" lists callers

  # ---------------------------------------------------------------
  Rule: CLI rejects invalid usage
  # ---------------------------------------------------------------

    @ISSUE-121
    Scenario: Context subcommand rejects working tree mode
      Given a git repository with one commit
      When I run "git-prism context HEAD"
      Then the exit code is not 0
      And the stderr is not empty

    @ISSUE-121
    Scenario: Context subcommand accepts a commit range
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
