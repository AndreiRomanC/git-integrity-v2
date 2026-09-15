# Repository backend modularization plan

Status: **Deferred until the current functional changes are stabilized**

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

## Proposed structure

```text
src-tauri/src/repository/
  mod.rs          # stable public surface and Tauri command exports
  core.rs         # repository opening, context resolution and shared helpers
  cache.rs        # shared caches and invalidation
  locks.rs        # per-repository operation coordination
  stash.rs        # stash list/create/apply/pop/drop operations
  branches.rs     # branch checkout, merge, reset and tracking information
  remote.rs       # fetch, pull, push and upstream resolution
  submodules.rs   # submodule status, versions, switch, commit and publish flows
  status.rs       # working-tree/status discovery and related aggregation
```

The final boundaries may be adjusted if the existing dependencies show that a
helper belongs in `core`, but feature modules must not build separate copies of
shared state.

## Recommended extraction order

1. `stash.rs` — relatively isolated and already covered by focused safety tests.
2. `branches.rs` — preserve checkout ordering and rollback guarantees exactly.
3. `remote.rs` — centralize upstream resolution and use it for preview and action.
4. `submodules.rs` — only after parent/submodule context tests are comprehensive.
5. `status.rs` — last, because status data affects much of the UI and performance.

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

The refactor is complete only when `repository.rs` has become the stable module
entry point, all existing behavior is preserved, the complete automated suites
pass, the critical manual scenarios pass, and there is no measurable regression
in repository loading, navigation, staging or submodule operations.
