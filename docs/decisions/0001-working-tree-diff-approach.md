# ADR 0001: Working Tree Diff Approach

## Status

Accepted

## Context

git-prism currently compares two committed trees using `gix::object::tree::diff` (the `base_tree.changes().for_each_to_obtain_tree(&head_tree, ...)` pattern in `src/git/diff.rs`). This only works for committed snapshots. To support `git status`-style workflows, we need to compare HEAD against the current working tree, including both staged (index) and unstaged (on-disk) changes.

The hard constraint is that production code must use `gix` only -- never shell out to the `git` CLI. The project currently uses `gix 0.81` with features `basic`, `blob-diff`, `sha1`.

The key questions:

1. Does gix have a working tree diff API?
2. Can it distinguish staged vs unstaged changes?
3. What feature flags are required?
4. If gix cannot do this natively, what is the fallback?

## Investigation Findings

### gix has a `status` module

gix 0.81 ships a `gix::status` module that mirrors `git status` semantics. It is gated behind the `status` feature flag.

**Entry point:** `Repository::status(progress)` returns a `Platform<'repo, Progress>` builder.

**Two iteration modes:**

- `Platform::into_iter(patterns)` -- yields *both* tree-to-index (staged) and index-to-worktree (unstaged/untracked) changes in a single pass.
- `Platform::into_index_worktree_iter(patterns)` -- yields only index-to-worktree changes.

The combined iterator yields `gix::status::Item`, which is an enum with two variants:

```rust
enum Item {
    /// Changes between HEAD tree and the index (staged changes).
    /// Equivalent to `git diff --cached`.
    TreeIndex(gix::diff::index::Change),

    /// Changes between the index and the working directory (unstaged changes).
    /// Also includes untracked files.
    /// Equivalent to `git diff` (unstaged) + untracked listing.
    IndexWorktree(gix::status::index_worktree::Item),
}
```

### Staged changes: `gix::diff::index::ChangeRef` / `Change`

The `TreeIndex` variant wraps `gix::diff::index::Change`, an owned version of `ChangeRef`, with four variants:

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Addition` | `location`, `index`, `entry_mode`, `id` | File added to the index but not in HEAD |
| `Deletion` | `location`, `index`, `entry_mode`, `id` | File in HEAD but removed from index |
| `Modification` | `location`, `previous_id`, `id`, modes, indices | File exists in both but content/mode differs |
| `Rewrite` | `source_location`, `location`, `source_id`, `id`, `copy` | Rename or copy detected between HEAD and index |

Each variant provides the blob `id` (object hash), which can be used with `repo.find_object(id)` to retrieve file content from the object database.

### Unstaged changes: `gix::status::index_worktree::Item`

The `IndexWorktree` variant wraps an enum with three variants:

| Variant | Fields | Meaning |
|---------|--------|---------|
| `Modification` | `entry`, `entry_index`, `rela_path`, `status: EntryStatus` | Tracked file modified on disk |
| `DirectoryContents` | `entry`, `collapsed_directory_status` | Untracked file/directory found during dirwalk |
| `Rewrite` | `source`, `dirwalk_entry`, `dirwalk_entry_id`, `diff`, `copy` | Rename/copy detected between index and worktree |

The `EntryStatus<T, U>` enum (from `gix-status`) provides detailed change information:

| Variant | Meaning |
|---------|---------|
| `Change(Change::Removed)` | File deleted from working tree |
| `Change(Change::Type { worktree_mode })` | File type changed (e.g., file to symlink) |
| `Change(Change::Modification { executable_bit_changed, content_change, .. })` | Content and/or permission change |
| `Change(Change::SubmoduleModification(..))` | Submodule state changed |
| `Conflict { summary, entries }` | Merge conflict (stages 1-3) |
| `NeedsUpdate(Stat)` | Stat info stale but content unchanged |
| `IntentToAdd` | Placeholder from `git add --intent-to-add` |

For `Modification`, the `content_change` field is `Option<T>` -- when the status iterator is configured with blob comparison, `T` is a `SubmoduleStatus` or `()`, and actual content must be read from the worktree (disk) using `std::fs::read()` since it is not yet in the object database.

### Reading file content for unstaged changes

For **staged** changes, both the old (HEAD) and new (index) blob IDs are available, so content can be read via `repo.find_object(id)`.

For **unstaged** changes, the "new" content lives on disk, not in the object database. The approach is:
- **Old content (index version):** Use the `entry.id` from the index entry to read the blob via `repo.find_object(entry.id)`.
- **New content (worktree version):** Read from disk using `repo.workdir().join(rela_path)` and `std::fs::read()`.

This is exactly how `git diff` (without `--cached`) works internally.

### Feature flags required

The `status` feature flag is needed. It transitively enables:
- `dirwalk` (directory traversal for untracked files, via `gix-dir`)
- `index` (access to `.git/index`, via `gix-index`)
- `blob-diff` (already enabled by `basic`)
- `gix-diff/index` (tree-to-index diffing)
- `attributes` and `excludes` (for `.gitignore` support)

The project's current features are `basic`, `blob-diff`, `sha1`. Adding `status` is additive -- it does not conflict with existing features. The `basic` feature already includes `blob-diff` and `index`, so `status` primarily adds `dirwalk`, `gix-status`, and `gix-diff/index`.

The resulting Cargo.toml line would be:

```toml
gix = { version = "0.81", default-features = false, features = ["basic", "blob-diff", "sha1", "status"] }
```

### Compile-time and binary size impact

The `status` feature pulls in `gix-status`, `gix-dir`, `gix-ignore`, `gix-filter`, `gix-pathspec`, `gix-attributes`, and `gix-submodule`. These share types with crates already compiled via `basic`. The actual impact on compile time and binary size should be measured after adding the feature flag -- the numbers will depend on how much code the linker can share with existing gix dependencies.

### Alternative: Manual approach without `status` feature

If we wanted to avoid the `status` feature, we could:

1. **Tree-to-index (staged):** Use `gix::diff::index()` directly (requires `gix-diff/index` feature). Compare the HEAD tree against the loaded index entry-by-entry.
2. **Index-to-worktree (unstaged):** Iterate index entries, `stat()` each file on disk, compare timestamps/sizes, and read+hash files that differ.

This would reimplement a subset of `gix-status` with worse correctness (missing `.gitignore` handling, submodule awareness, race condition handling, racy-git detection). Not recommended.

## Decision

Use gix's built-in `status` module by adding the `status` feature flag. Use `Repository::status(progress).into_iter(patterns)` to get a unified stream of both staged and unstaged changes.

**Approach for git-prism integration:**

1. Add `"status"` to gix feature flags in `Cargo.toml`.
2. Add a new method `RepoReader::diff_worktree(&self)` (or similar) that:
   - Calls `self.repo.status(gix::progress::Discard).into_iter(None)` to get all changes.
   - Iterates the `Item` enum, mapping each variant to the existing `FileChange` struct.
   - For `TreeIndex` items, marks them as "staged".
   - For `IndexWorktree` items, marks them as "unstaged" (or "untracked" for `DirectoryContents`).
3. Extend `FileChange` (or create a wrapper) to carry a `change_scope` field indicating `Staged`, `Unstaged`, or `Untracked`.
4. For file content retrieval (snapshots):
   - **HEAD version:** `repo.find_object(id)` using the blob ID from the tree or index entry.
   - **Index version:** `repo.find_object(index_entry.id)` for staged content.
   - **Worktree version:** `std::fs::read(repo.workdir().join(path))` for on-disk content.

## Consequences

### Positive

- **Native gix support.** No shelling out, no reimplementing git internals. The `status` module handles racy-git detection, `.gitignore` parsing, submodule status, and case-insensitive filesystem handling.
- **Staged vs unstaged distinction.** The `Item` enum separates `TreeIndex` (staged) from `IndexWorktree` (unstaged) at the type level, which maps directly to what git-prism needs to expose.
- **Rename/copy detection.** Both the tree-index and index-worktree layers support rewrite tracking, consistent with git-prism's existing `Renamed`/`Copied` change types.
- **Untracked file discovery.** The `dirwalk` integration surfaces untracked files, which agents need to know about.
- **Additive API change.** The existing `DiffResult` / `FileChange` types can be extended with a `change_scope` field rather than replaced.

### Negative

- **Increased dependency footprint.** The `status` feature adds ~6 sub-crates (`gix-status`, `gix-dir`, `gix-ignore`, `gix-filter`, `gix-pathspec`, `gix-attributes`).
- **Worktree content requires disk I/O.** Unlike commit-to-commit diffs where all content is in the object database, unstaged file content must be read from the filesystem. This introduces potential race conditions (file modified between status check and content read) and I/O errors.
- **Progress plumbing.** `Repository::status()` requires a `Progress` parameter. For git-prism's use case, `gix::progress::Discard` is sufficient, but the type parameter propagates through the Platform builder.

### Implementation Notes

**Pseudocode for the core diff_worktree method:**

```rust
use gix::status::Item;

impl RepoReader {
    pub fn diff_worktree(&self) -> Result<WorktreeDiffResult, GitError> {
        let mut staged = Vec::new();
        let mut unstaged = Vec::new();
        let mut untracked = Vec::new();

        let status_iter = self.repo
            .status(gix::progress::Discard)?
            .into_iter(None)?;  // None = no pathspec filter

        for item in status_iter {
            let item = item.map_err(|e| GitError::ReadObject(e.to_string()))?;
            match item {
                Item::TreeIndex(change) => {
                    staged.push(tree_index_change_to_file_change(change)?);
                }
                Item::IndexWorktree(iw_item) => {
                    match &iw_item {
                        index_worktree::Item::Modification { .. } => {
                            unstaged.push(index_worktree_to_file_change(&iw_item)?);
                        }
                        index_worktree::Item::DirectoryContents { .. } => {
                            untracked.push(dirwalk_to_file_change(&iw_item)?);
                        }
                        index_worktree::Item::Rewrite { .. } => {
                            unstaged.push(index_worktree_to_file_change(&iw_item)?);
                        }
                    }
                }
            }
        }

        Ok(WorktreeDiffResult { staged, unstaged, untracked })
    }
}
```

**Reading file content at different stages:**

```rust
// HEAD version of a file (from committed tree)
fn read_from_head(repo: &gix::Repository, path: &str) -> Result<Vec<u8>, GitError> {
    let head_commit = repo.head_commit()?;
    let tree = head_commit.tree()?;
    let entry = tree.lookup_entry_by_path(path)?;
    let blob = entry.object()?;
    Ok(blob.data.to_vec())
}

// Index (staged) version of a file
fn read_from_index(repo: &gix::Repository, blob_id: gix::ObjectId) -> Result<Vec<u8>, GitError> {
    let obj = repo.find_object(blob_id)?;
    Ok(obj.data.to_vec())
}

// Worktree (on-disk) version of a file
fn read_from_worktree(repo: &gix::Repository, rela_path: &str) -> Result<Vec<u8>, GitError> {
    let workdir = repo.workdir().ok_or(GitError::BareRepo)?;
    let full_path = workdir.join(rela_path);
    std::fs::read(&full_path).map_err(|e| GitError::ReadObject(e.to_string()))
}
```

**Key type mappings to existing git-prism types:**

| gix status type | git-prism ChangeType |
|----------------|---------------------|
| `diff::index::Change::Addition` | `ChangeType::Added` |
| `diff::index::Change::Deletion` | `ChangeType::Deleted` |
| `diff::index::Change::Modification` | `ChangeType::Modified` |
| `diff::index::Change::Rewrite { copy: false }` | `ChangeType::Renamed` |
| `diff::index::Change::Rewrite { copy: true }` | `ChangeType::Copied` |
| `index_worktree::Item::Modification` with `Change::Removed` | `ChangeType::Deleted` |
| `index_worktree::Item::Modification` with `Change::Modification` | `ChangeType::Modified` |
| `index_worktree::Item::Modification` with `Change::Type` | `ChangeType::Modified` |
| `index_worktree::Item::DirectoryContents` | New: `ChangeType::Untracked` (or separate list) |
| `index_worktree::Item::Rewrite` | `ChangeType::Renamed` / `ChangeType::Copied` |

**Line count computation for unstaged changes:**

The existing `count_line_changes()` function in `src/git/diff.rs` takes `&[u8]` slices and uses `gix::diff::blob` for Myers diff. This works unchanged -- for unstaged changes, pass the index blob content as "old" and the disk file content as "new".
