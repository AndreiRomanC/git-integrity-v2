# Git DrillDown — development preferences

## Git execution policy (user decision, 2026-10-31)

- For future sensitive Git operations, prefer direct commands through the installed Git executable, particularly for changes to history, merge state, the index, the working tree, or submodule revisions. Reuse the application's existing command runner and safety checks.
- libgit2 may remain useful for read-only queries and navigation. Do not assume it is faster than Git CLI; performance claims require measurements of the actual workflow.
- Keep merge preview and actual merge semantics aligned. Do not automatically retry a failed mutating operation with a different engine: the first attempt may already have changed repository state.
- This is a direction for future work, not authorization to migrate existing implementations. The user explicitly chose to leave the current implementation unchanged for now. Discuss and test any proposed migration separately.
