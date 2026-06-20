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
  Scenario: Shim sets GIT_PRISM_INSIDE_SHIM=1 in the child process environment
    # Verifies the sentinel propagation: the real git invoked by the shim
    # must run with GIT_PRISM_INSIDE_SHIM=1 set so nested git calls inside
    # lefthook/cargo/etc. never re-enter structured-JSON mode.
    Given a fixture git repository with two commits
    And an env-dumper script that captures the child environment
    When I run the shim as "git" pointing at the env-dumper with CLAUDECODE=1
    Then the env-dumper output contains "GIT_PRISM_INSIDE_SHIM=1"

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
  # Hooks install/uninstall/status scenarios (#288)
  #
  # git-prism hooks install --path-shim creates a symlink at
  # ~/.local/share/git-prism/bin/git pointing at the running git-prism binary.
  # Operations use a tempdir HOME for isolation.
  # -------------------------------------------------------------------------

  @ISSUE-288
  Scenario: hooks install --path-shim creates the symlink in an isolated HOME
    Given an isolated HOME directory
    When I run "git-prism hooks install --path-shim" with the isolated HOME
    Then the exit code is 0
    And the path "$HOME/.local/share/git-prism/bin/git" is a symlink

  @ISSUE-288
  Scenario: hooks install --path-shim prints the PATH update instruction
    Given an isolated HOME directory
    When I run "git-prism hooks install --path-shim" with the isolated HOME
    Then the exit code is 0
    And the output contains "PATH"
    And the output contains ".local/share/git-prism/bin"

  @ISSUE-302
  Scenario: hooks install --path-shim prints the symlink path
    Given an isolated HOME directory
    When I run "git-prism hooks install --path-shim" with the isolated HOME
    Then the exit code is 0
    And the output contains "Created symlink:"

  @ISSUE-288
  Scenario: hooks install --path-shim is idempotent
    Given an isolated HOME directory
    When I run "git-prism hooks install --path-shim" with the isolated HOME
    And I run "git-prism hooks install --path-shim" with the isolated HOME again
    Then the exit code is 0
    And the path "$HOME/.local/share/git-prism/bin/git" is a symlink

  @ISSUE-288
  Scenario: hooks status reports path-shim as present after install
    Given an isolated HOME directory
    When I run "git-prism hooks install --path-shim" with the isolated HOME
    And I run "git-prism hooks status" with the isolated HOME
    Then the exit code is 0
    And the hooks status output mentions "path-shim"

  @ISSUE-288
  Scenario: hooks status reports path-shim as absent before install
    Given an isolated HOME directory
    When I run "git-prism hooks status" with the isolated HOME
    Then the exit code is 0
    And the hooks status output indicates path-shim is not installed

  @ISSUE-288
  Scenario: hooks uninstall --path-shim removes the symlink
    Given an isolated HOME directory
    When I run "git-prism hooks install --path-shim" with the isolated HOME
    And I run "git-prism hooks uninstall --path-shim" with the isolated HOME
    Then the exit code is 0
    And the path "$HOME/.local/share/git-prism/bin/git" does not exist

  @ISSUE-288
  Scenario: hooks uninstall --path-shim removes the parent directory when empty
    Given an isolated HOME directory
    When I run "git-prism hooks install --path-shim" with the isolated HOME
    And I run "git-prism hooks uninstall --path-shim" with the isolated HOME
    Then the exit code is 0
    And the directory "$HOME/.local/share/git-prism/bin" does not exist

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
