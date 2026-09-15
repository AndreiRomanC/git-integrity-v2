# Repository backend modularization plan

Status: **Phase 1 complete on `codex/repository-modularization`**

Stable rollback point: annotated tag `v1-stable` at commit `1854405`.
The tag is published on `origin`; `origin/main` was not moved by this work.

## Goal

Split the large `src-tauri/src/repository.rs` implementation into small,
domain-focused modules without changing Git behavior, Tauri command names,
frontend payloads, error messages, or performance characteristics.

This is a maintainability refactor. It is not expected to make repository
loading or navigation faster by itself.

## Start conditions

Do not begin this refactor until all of the following are true:

- the current macOS build has passed a manual smoke test;
- the critical submodule, stash, branch-switch and push flows work as expected;
- the current functional work is saved in a known-good checkpoint commit;
- `git status` contains no unexplained changes;
- the existing Rust and frontend test suites pass;
- Windows-specific behavior is either verified on Windows or explicitly listed
  as unverified.

## Safety rules

- Move code mechanically first; do not redesign behavior in the same commit.
- Keep all public Tauri command names and argument/response shapes unchanged.
- Preserve the existing repository locks, caches, stale-request guards and path
  validation. Do not duplicate them inside feature modules.
- Keep parent-repository and submodule contexts explicit. Every operation must
  receive or resolve the repository it acts on; never rely on stale UI state.
- Do not introduce extra Git commands, history walks, fetches or network calls.
- Preserve exact rollback and error behavior for destructive or multi-step Git
  operations.
- Make one small extraction per commit and run the relevant tests after each
  extraction.
- Stop and revert the current extraction if tests or observable behavior change.

## Current structure

```text
src-tauri/src/
  repository.rs                    # shared repository core, status/navigation,
                                   # submodules, PR and publish flows
  repository/
    stash.rs                        # stash list/create/restore/pop/drop
    branches.rs                     # branch CRUD, checkout and divergence
    branches/merge.rs               # merge and conflict workflow
    command_console.rs              # scoped Git and shell execution
    remotes.rs                      # list/fetch/pull/push synchronization
```

Shared locks, caches, repository opening, path validation and cache invalidation
remain deliberately centralized in `repository.rs`. Feature modules import that
infrastructure instead of creating parallel state or subtly different rules.

Phase 2 may extract `submodules`, Pull Requests/publishing, and status/navigation,
but only in that order and only with focused tests around every boundary. Moving
`repository.rs` mechanically to `repository/mod.rs` is intentionally deferred:
it would add a large rename without improving behavior or ownership.

## Recommended extraction order

1. ✅ `stash.rs` — extracted with focused stash tests.
2. ✅ `branches.rs` and `branches/merge.rs` — extracted with checkout, DAG,
   merge, conflict and abort tests.
3. ✅ `command_console.rs` — extracted with explicit folder/submodule scope tests.
4. ✅ `remotes.rs` — extracted with multi-remote fetch and upstream sync tests.
5. ⏳ `submodules.rs` — next only after a separate stable checkpoint.
6. ⏳ Pull Request and publish adapters — separate generic Git transport from
   GitHub-specific provider behavior.
7. ⏳ `status.rs` / `navigation.rs` — last because cache behavior is performance
   critical and externally edited files must still be detected correctly.

Extract `core.rs`, `cache.rs` and `locks.rs` only as their shared responsibilities
become clear. Do not create speculative abstractions before moving the first
feature module.

## Verification for every extraction

- `cargo test` passes with no new ignored tests or warnings;
- frontend tests pass;
- no Tauri command registration or serialization contract changes;
- `git diff --stat` and `git diff` show moves/import changes only;
- a short manual smoke test covers the extracted feature;
- compare performance logs before and after if the moved code touches status,
  navigation, history or submodule discovery;
- on the final checkpoint, build and smoke-test both macOS and Windows artifacts.

## Functional scenarios that must remain covered

- failed branch checkout leaves HEAD, index and working tree unchanged;
- a full stash pop cannot overwrite unrelated existing work;
- submodule fetch/pull/push uses the configured upstream remote and branch;
- detached submodule HEAD is never presented as safely pushable;
- local-only submodule remotes are detected and explained without pretending
  that commits are published to a shared server;
- identical branch names in different submodules never share cached data;
- parent repository actions never execute against the previously viewed
  submodule, and submodule actions never execute against the parent;
- ignored or untracked files are not silently overwritten by checkout/merge;
- caches are invalidated only after successful state-changing operations.

## Completion criteria

Phase 1 is complete when all automated suites and a packaged macOS smoke test
pass. The complete refactor remains incremental: it is finished only when the
remaining high-value domains have safe boundaries, critical manual scenarios
pass, and there is no measurable regression in repository loading, navigation,
staging or submodule operations.
