# Repository backend boundaries

This directory contains cohesive feature modules extracted from the repository
backend. The parent `repository.rs` remains the shared composition and runtime
core while the migration is in progress.

## Ownership

- `stash.rs`: stash lifecycle and partial restore behavior.
- `branches.rs`: branch creation, checkout, rename, delete and divergence.
- `branches/merge.rs`: merge state, conflicts, resolution, completion and abort.
- `command_console.rs`: explicitly scoped Git and shell process execution.
- `remotes.rs`: remote discovery, fetch and current-branch synchronization.

## Invariants

- A command resolves the exact parent or submodule repository before acting.
- All mutations use the shared per-repository write lock.
- All successful mutations invalidate the shared metadata caches.
- Feature modules do not own independent caches, locks or repository discovery.
- Tauri command names and serialized request/response shapes remain stable.
- No extraction adds Git commands, history walks, fetches or network requests.

Further extraction should stay mechanical first and behavioral second. Put a
behavior change in a separate commit with its own focused tests and measurement.
