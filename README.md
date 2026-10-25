# Git DrillDown

A cross-platform visual Git client inspired by the information density and structured workflows of MKS Integrity, while using standard Git terminology.

## Architecture

- Tauri 2 desktop shell
- Rust backend with an embedded libgit2 engine
- Plain HTML, CSS and JavaScript frontend
- No Node.js or npm runtime/build step
- SVG branch graph

The source project is under 1 MB. Build artifacts are written to the operating system's temporary directory by the launchers and removed when the application closes, so the project does not grow by gigabytes.

## Testing

The Rust backend's own tests (`cargo test`, from `src-tauri`) are the primary suite. Pure frontend logic such as the commit-graph layout and Terminal execution-context selection has automated coverage in `tests/`, runnable with `node --test` from the repository root. This uses only Node's own built-in test runner and assert module (Node 18+) — no npm install, no `node_modules`, no `package.json` — so it does not change the "no Node.js/npm runtime or build step" above: nothing here is invoked by the build or by the shipped app, only by a maintainer choosing to run it.

## Author filtering in comparisons

The normal author filter remains a non-merge commit/path history filter. The optional strict filter narrows that same candidate population. Counts refer to comparison **items**, after the current view's path, search and state filters; rename aliases do not count as extra rows. Included content, grouped folder/submodule conversions, inconclusive results and items without a surviving witness are distinct.

The author menu is scoped to the actual loaded comparison paths (including the selected folder or submodule's inner folder), not every path in the commits. Mixed-folder commits contribute only their matching paths; authors with no matching touches are omitted. Metadata and strict results are cached separately for each path set and revision pair. Text/state filters narrow the displayed items and summary without restarting history analysis. A retained author with no matches in a newly selected folder shows no items, never all items silently.

Strict analysis reads local Git objects only. It traces surviving unique lines and atomic gitlink changes through both sides of the history. A bounded contextual fallback handles large UTF-8 text and repeated lines: a unique surrounding block identifies an occurrence, but the witness commit must also actually add the changed line or perform a standalone deletion. Editing only its neighbour is insufficient. Conversion commits do not establish authorship of imported file contents. Independent later content edits can still qualify.

Limits are explicit, not negative authorship claims: binary/NUL-encoded text, ambiguous origins, edited renames and changing context can remain inconclusive. Contextual analysis accepts up to 48 MiB per version, 512 witness windows (1024 candidates), 64 KiB per window and 128 changing history steps; the original unique-line trace allows 256 steps. The request has a 30-second/100,000-commit guard, fails closed and never applies partial results after a request error. Contextual buffers are bounded separately from the 64 MiB small-file provenance cache; there is no recursive submodule scan or fetch. The existing text-diff preview size limit is unchanged. Git authorship cannot prove whether a change was typed, copied or squash-imported.

Tests: `node --test tests/comparison-authors.test.js` and `cargo test --lib repository::comparison_authors` (in `src-tauri`). Controlled histories include a 30 MB CDFX-like input, repetitive XML, merges/reverts, feature-to-feature comparisons and folder/gitlink conversions. Real company-repository attribution still needs validation against its actual witness commits.

## Browser preview

Open `frontend/index.html` directly or serve the folder with any static server. Browser mode uses representative demo data because a normal web page cannot access local repositories.

## Desktop development

On this Mac, double-click `run-macos.command`. It opens the optimized standalone application from `dist/Git DrillDown.app` and does not compile or install anything.

Alternatively, run:

```sh
cd src-tauri
cargo tauri dev
```

`cargo run` from `src-tauri` also starts the application without installing the Tauri CLI.

## Windows

The Windows application is a portable `GitDrillDown.exe`. End users need neither Node.js, Rust, nor an installer.

Most Git operations (browsing, staging, committing, branching, history, submodule version switching) run entirely through the embedded libgit2 engine and need nothing else installed. **Network operations — push, fetch, pull, and "restore from remote"** — shell out to the system `git` command so they transparently reuse whatever credentials (SSH agent, credential helper, OS keychain) already work in a terminal on that machine, instead of libgit2's much narrower built-in credential search. This means **Git for Windows must be installed and on `PATH`** for those specific actions; everything else works without it.

Build the executable on a Windows build machine with:

```powershell
.\build-windows.ps1
```

The script writes `dist\windows\GitDrillDown.exe`, rejects files over 30 MB, and removes temporary build artifacts. The included GitHub Actions workflow can perform the Windows build without installing build tools on the user's computer.

If the Tauri Cargo subcommand is missing:

```sh
cargo install tauri-cli --version '^2.0'
```

## Implemented operations

- Select and open a local repository
- Initialize a repository
- Read local/remote branches, status and commit history
- Stage and unstage files
- Create commits
- Create and switch branches
