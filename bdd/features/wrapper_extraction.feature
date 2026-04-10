@ISSUE-126
Feature: Wrapper-pattern function extraction

  Tree-sitter wraps certain language patterns (exported functions,
  decorated functions) in wrapper nodes. The analyzers must recurse
  into these wrappers to detect the inner function declarations.
  Without this, `export function` in TypeScript and `@decorator def`
  in Python silently produce no function data.

  # ---------------------------------------------------------------
  Rule: TypeScript exported functions are detected
  # ---------------------------------------------------------------

    @ISSUE-129
    Scenario: export function produces a FunctionChange entry
      Given a git repository with a TypeScript exported function change
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the file "lib.ts" has a function change "greet" with type "modified"

    @ISSUE-129
    Scenario: export default function produces a FunctionChange entry
      Given a git repository with a TypeScript export-default function change
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the file "handler.ts" has a function change "handler" with type "modified"

    @ISSUE-129
    Scenario: export class methods produce FunctionChange entries
      Given a git repository with a TypeScript exported class change
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the file "calculator.ts" has a function change "multiply" with type "added"

  # ---------------------------------------------------------------
  Rule: Python decorated functions are detected
  # ---------------------------------------------------------------

    @ISSUE-130
    Scenario: decorated function produces a FunctionChange entry
      Given a git repository with a Python decorated function change
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the file "app.py" has a function change "index" with type "modified"

    @ISSUE-130
    Scenario: stacked decorators produce a FunctionChange entry
      Given a git repository with a Python stacked-decorator function change
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the file "app.py" has a function change "admin_page" with type "modified"

  # ---------------------------------------------------------------
  Rule: C++ extern "C" functions are detected
  # ---------------------------------------------------------------

    @ISSUE-131
    Scenario: extern C block functions produce FunctionChange entries
      Given a git repository with a C++ extern-C function change
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the file "ffi.cpp" has a function change "ffi_init" with type "modified"

  # ---------------------------------------------------------------
  Rule: TypeScript export function works in function context
  # ---------------------------------------------------------------

    @ISSUE-129
    Scenario: exported function appears in function context callers
      Given a git repository with a TypeScript exported function context fixture
      When I run "git-prism context HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the context for function "compute" lists callers
