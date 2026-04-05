@ISSUE-32
Feature: crates.io package publishing
  git-prism is published to crates.io so users can install it with
  "cargo install git-prism". The package must be available on the
  public registry.

  Rule: The package is available on crates.io

    Scenario: git-prism is listed on crates.io
      When I query crates.io for package "git-prism"
      Then the package exists on crates.io
