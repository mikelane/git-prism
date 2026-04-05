@ISSUE-31 @not_implemented
Feature: Per-commit history manifests
  git-prism can produce a manifest for each individual commit in a
  range, rather than collapsing everything into a single diff. This
  lets agents understand the incremental evolution of a branch, seeing
  what changed in each commit separately.

  Rule: History produces one manifest per commit in a range

    Scenario: History returns per-commit manifests for a three-commit range
      Given a git repository with three sequential commits
      When I run "git-prism history HEAD~3..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And the JSON has key "commits"
      And the JSON value "commits" has length 3

  Rule: Each commit manifest contains its own files and metadata

    Scenario: Each commit in history has its own file list and summary
      Given a git repository with three sequential commits
      When I run "git-prism history HEAD~3..HEAD"
      Then the exit code is 0
      And the output is valid JSON
      And each commit entry has keys "metadata" and "files" and "summary"
