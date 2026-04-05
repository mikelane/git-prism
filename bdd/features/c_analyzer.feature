@ISSUE-30 @not_implemented
Feature: C and C++ language analysis
  git-prism extracts function and import information from C and C++
  source files using tree-sitter. This covers .c, .h, .cpp, and .hpp
  extensions, enabling agents to understand which functions changed
  across the C family of languages.

  Rule: C and C++ are recognized languages

    Scenario: C appears in the supported languages list
      When I run "git-prism languages"
      Then the exit code is 0
      And the languages list includes "c"

    Scenario: C++ appears in the supported languages list
      When I run "git-prism languages"
      Then the exit code is 0
      And the languages list includes "cpp"

  Rule: C function definitions are extracted

    Scenario: Manifest extracts C function definitions
      Given a git repository with a C commit
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And at least one file has "functions_changed" that is not null

  Rule: C++ methods include class or namespace qualification

    Scenario: Manifest extracts C++ methods with class qualification
      Given a git repository with a C++ commit
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And at least one file has "functions_changed" that is not null
      And the functions list includes a class-qualified method name

  Rule: Header files are analyzed for declarations

    Scenario: Header file function declarations are analyzed
      Given a git repository with a C header commit
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And at least one file has "functions_changed" that is not null
