# BDD bootstrap for the PATH-shim wrapper epic (#284).
#
# Each scenario is tagged with the implementation issue that will make it
# GREEN. Until that issue lands, the @not_implemented tag keeps the scenario
# excluded from CI. The implementation PR's first commit must remove
# @not_implemented from its targeted scenarios (the RED commit) before any
# production code is written.
#
# Tag conventions:
#   @ISSUE-287  -- scenarios exercising git invocations through the shim symlink
#                  (argv[0] dispatch). Goes green when #287 lands.
#   @ISSUE-288  -- scenarios exercising `git-prism hooks install/uninstall/status
#                  --path-shim`. Goes green when #288 lands.
#   @not_implemented -- applied to every scenario; CI excludes these.
#
# Step definitions shell out to the real compiled `git-prism` binary or to the
# shim symlink built in a per-scenario tempdir. None of them mock or stub —
# when the implementation does not yet exist the steps fail with assertion
# errors that document the contract being tested.

Feature: PATH-shim wrapper for structured git output

  # -------------------------------------------------------------------------
  # Shim execution scenarios (#287)
  #
  # The shim is a symlink tmpdir/bin/git -> git-prism binary. When invoked as
  # "git" with PATH pointing at tmpdir/bin, an agent-indicating env var
  # (CLAUDECODE=1) causes the shim to route supported subcommands to
  # git-prism structured JSON output and pass everything else through to the
  # real git binary.
  # -------------------------------------------------------------------------

  @ISSUE-287
  Scenario: Agent plus git diff with two-dot range produces JSON with files array
    Given a fixture git repository with two commits
    When I run the shim as "git diff HEAD~1..HEAD" with CLAUDECODE=1
    Then the exit code is 0
    And the output is valid JSON
    And the JSON output has a "files" array
    And the JSON has key "git_prism_hint"

  @ISSUE-287
  Scenario: Agent plus git diff with three-dot range produces JSON with files array
    Given a fixture git repository with two commits
    When I run the shim as "git diff HEAD~1...HEAD" with CLAUDECODE=1
    Then the exit code is 0
    And the output is valid JSON
    And the JSON output has a "files" array

  @ISSUE-287
  Scenario: Agent plus git log with two-dot range produces JSON with commits array
    Given a fixture git repository with two commits
    When I run the shim as "git log HEAD~1..HEAD" with CLAUDECODE=1
    Then the exit code is 0
    And the output is valid JSON
    And the JSON output has a "commits" array

  @ISSUE-287
  Scenario: Agent plus git log with pickaxe flag produces function_context JSON
    Given a fixture git repository with two commits
    When I run the shim as "git log -Sfoo" with CLAUDECODE=1
    Then the exit code is 0
    And the output is valid JSON
    And the JSON output has a "function_context" shape

  @ISSUE-287
  Scenario: Pickaxe flag takes priority over log range — both present yields function_context
    Given a fixture git repository with two commits
    When I run the shim as "git log -Sfoo HEAD~1..HEAD" with CLAUDECODE=1
    Then the exit code is 0
    And the output is valid JSON
    And the JSON output has a "function_context" shape

  @ISSUE-287
  Scenario: Agent plus git show produces JSON with snapshots key
    Given a fixture git repository with two commits
    When I run the shim as "git show HEAD" with CLAUDECODE=1
    Then the exit code is 0
    And the output is valid JSON
    And the JSON output has a "snapshots" key

  @ISSUE-287
  Scenario: Agent plus git blame produces JSON with snapshots key
    Given a fixture git repository with two commits and a tracked file
    When I run the shim as "git blame README.md" with CLAUDECODE=1
    Then the exit code is 0
    And the output is valid JSON
    And the JSON output has a "snapshots" key

  @ISSUE-287
  Scenario: Agent plus git status passes through to real git output
    Given a fixture git repository with two commits
    When I run the shim as "git status" with CLAUDECODE=1
    Then the exit code is 0
    And the output is not JSON
    And the output contains "On branch"

  @ISSUE-287
  Scenario: Agent plus git commit passes through and creates a real commit
    Given a fixture git repository with two commits and a staged file
    When I run the shim as "git commit -m shim-test" with CLAUDECODE=1
    Then the exit code is 0
    And the output is not JSON
    And the new commit exists in the repository

  @ISSUE-287
  Scenario: Agent plus git diff without ref range passes through as raw diff
    Given a fixture git repository with an unstaged modification
    When I run the shim as "git diff" with CLAUDECODE=1
    Then the exit code is 0
    And the output is not JSON
    And the output contains "diff --git"

  @ISSUE-287
  Scenario: Agent plus git diff with single ref and no range passes through
    Given a fixture git repository with two commits
    When I run the shim as "git diff HEAD~1" with CLAUDECODE=1
    Then the exit code is 0
    And the output is not JSON
    And the output contains "diff --git"

  @ISSUE-287
  Scenario: Agent plus git log without ref range passes through as raw log
    Given a fixture git repository with two commits
    When I run the shim as "git log --oneline" with CLAUDECODE=1
    Then the exit code is 0
    And the output is not JSON

  @ISSUE-287
  Scenario: Non-agent invocation of git diff with range passes through as raw diff
    Given a fixture git repository with two commits
    When I run the shim as "git diff HEAD~1..HEAD" without any agent env vars
    Then the exit code is 0
    And the output is not JSON
    And the output contains "diff --git"

  @ISSUE-287
  Scenario: Sentinel env var GIT_PRISM_INSIDE_SHIM causes passthrough even with agent flag
    Given a fixture git repository with two commits
    When I run the shim as "git diff HEAD~1..HEAD" with CLAUDECODE=1 and GIT_PRISM_INSIDE_SHIM=1
    Then the exit code is 0
    And the output is not JSON
    And the output contains "diff --git"

  @ISSUE-287
  Scenario: Shim does not leak GIT_PRISM_INSIDE_SHIM into the child process environment
    # Real git's children (hooks, aliases, editors) must see an unmodified
    # environment. Recursion is prevented by the resolver skipping git-prism
    # binaries, not by an inherited sentinel.
    Given a fixture git repository with two commits
    And an env-dumper script that captures the child environment
    When I run the shim as "git" pointing at the env-dumper with CLAUDECODE=1
    Then the env-dumper output does not contain "GIT_PRISM_INSIDE_SHIM"

  @ISSUE-299
  Scenario: Real-git resolver skips the shim directory when walking PATH
    # The shim binary lives in tmpdir/bin. The resolver must skip that
    # directory and find the system git elsewhere in PATH. Without this
    # guard, the shim would exec itself in an infinite loop.
    #
    # The resolver logic itself is unit-tested in src/shim/real_git.rs.
    # This integration assertion requires #299 (a --debug-resolver
    # flag that prints the resolved path to stderr).
    Given a fixture git repository with two commits
    When I run the shim as "git diff HEAD~1..HEAD" without any agent env vars
    Then the resolved real-git binary path does not live in the shim directory

  # -------------------------------------------------------------------------
  # Hooks install/uninstall/status scenarios (#288, reworked for #387)
  #
  # git-prism hooks install --path-shim is a deprecated alias for
  # `git-prism shim install`: it writes the rc `git` function block (no
  # symlink, no PATH change). Operations use a tempdir HOME for isolation.
  # -------------------------------------------------------------------------

  @ISSUE-288 @ISSUE-387
  Scenario: hooks install --path-shim writes the function block in an isolated HOME
    Given an isolated HOME directory
    When I run "git-prism hooks install --path-shim" with the isolated HOME
    Then the exit code is 0
    And the path "$HOME/.zshrc" contains "function git {"
    And the path "$HOME/.local/share/git-prism/bin/git" does not exist

  @ISSUE-288 @ISSUE-387
  Scenario: hooks install --path-shim tells the user to restart the agent session
    Given an isolated HOME directory
    When I run "git-prism hooks install --path-shim" with the isolated HOME
    Then the exit code is 0
    And the output contains "Restart your agent session"

  @ISSUE-302 @ISSUE-387
  Scenario: hooks install --path-shim names the rc file it wrote
    Given an isolated HOME directory
    When I run "git-prism hooks install --path-shim" with the isolated HOME
    Then the exit code is 0
    And the output contains "~/.zshrc"

  @ISSUE-288 @ISSUE-387
  Scenario: hooks install --path-shim is idempotent
    Given an isolated HOME directory
    When I run "git-prism hooks install --path-shim" with the isolated HOME
    And I run "git-prism hooks install --path-shim" with the isolated HOME again
    Then the exit code is 0
    And the path "$HOME/.zshrc" contains "function git {"

  @ISSUE-288 @ISSUE-387
  Scenario: hooks status reports the shim as installed after install
    Given an isolated HOME directory
    When I run "git-prism hooks install --path-shim" with the isolated HOME
    And I run "git-prism hooks status" with the isolated HOME
    Then the exit code is 0
    And the output contains "shim: installed in ~/.zshrc"

  @ISSUE-288 @ISSUE-387
  Scenario: hooks status reports the shim as absent before install
    Given an isolated HOME directory
    When I run "git-prism hooks status" with the isolated HOME
    Then the exit code is 0
    And the output contains "shim: not installed"

  @ISSUE-288 @ISSUE-387
  Scenario: hooks uninstall --path-shim removes the function block
    Given an isolated HOME directory
    When I run "git-prism hooks install --path-shim" with the isolated HOME
    And I run "git-prism hooks uninstall --path-shim" with the isolated HOME
    Then the exit code is 0
    And the path "$HOME/.zshrc" does not contain "function git {"

  # -------------------------------------------------------------------------
  # Blob/object-spec passthrough (#381)
  #
  # git show <rev>:<path> prints the file content at that revision.  Before
  # this fix the shim tried to peel the whole "<rev>:<path>" string to a
  # commit, which always errored with "was blob while trying to peel to
  # commit".  The shim must now pass these specs straight through to real git.
  # -------------------------------------------------------------------------

  @ISSUE-381
  Scenario: git show HEAD:<file> passes through and prints the file content
    Given a fixture git repository with two commits and a tracked file
    When I run the shim as "git show HEAD:README.md" with CLAUDECODE=1
    Then the exit code is 0
    And the output contains "# Readme"
    And the output does not contain the handler error marker

  @ISSUE-381
  Scenario: plain git show <sha> (no colon) still returns the JSON snapshot manifest
    Given a fixture git repository with two commits
    When I run the shim as "git show HEAD" with CLAUDECODE=1
    Then the exit code is 0
    And the output is valid JSON
    And the JSON output has a "snapshots" key

  # -------------------------------------------------------------------------
  # shim-run entry point and pure-passthrough symlink (#387)
  #
  # Interception is reachable only through `git-prism shim-run git ...`, which
  # an interactive rc shell function calls. A binary invoked as git/gh (stale
  # symlink on PATH) never intercepts.
  # -------------------------------------------------------------------------

  @ISSUE-387
  Scenario: A program that runs git through a stale git symlink gets real git output
    Given a fixture git repository with two commits
    When a program runs "git diff -M --no-color --no-ext-diff HEAD~1..HEAD" through the stale git symlink with CLAUDECODE=1
    Then the output equals real git's output and the exit codes match

  @ISSUE-387
  Scenario: shim-run with stdout redirected to a file writes real git output
    Given a fixture git repository with two commits
    When I run shim-run as "git diff HEAD~1..HEAD" with CLAUDECODE=1 and stdout redirected to a file
    Then the output equals real git's output and the exit codes match
