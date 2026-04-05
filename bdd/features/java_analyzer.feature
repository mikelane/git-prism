@ISSUE-29
Feature: Java language analysis
  git-prism extracts function and import information from Java source
  files using tree-sitter, enabling agents to understand which methods
  and imports changed in a commit without reading full file diffs.

  Rule: Java is a recognized language

    Scenario: Java appears in the supported languages list
      When I run "git-prism languages"
      Then the exit code is 0
      And the languages list includes "java"

  Rule: Java method extraction uses class-qualified names

    Scenario: Manifest extracts Java methods with class qualification
      Given a git repository with a Java commit
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And at least one file has "functions_changed" that is not null
      And the functions list includes a class-qualified method name

  Rule: Java import statements are extracted

    Scenario: Manifest extracts Java import statements
      Given a git repository with a Java commit
      When I run "git-prism manifest HEAD~1..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And at least one file has "imports_changed" that is not null
