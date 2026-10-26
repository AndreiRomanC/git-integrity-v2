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

In comparison List and Drill-down views, the same contributor selector offers **All differences**, **Shared files** (two or more recorded author identities), **Files touched by author** and **Introduced by author**. Touched/Shared include intermediate non-merge commits, even if a later commit overwrote that contributor's edits. Only items differing between the selected endpoints can appear in a committed changes-only comparison; opening an item always shows its complete endpoint diff. Reversed and divergent comparisons use commits reachable from either endpoint but not both. Repeated commits by the same recorded identity count once per item; different email identities can still belong to the same person.

Drill-down filters the same leaf items as List, then retains their parent containers for navigation. Counts describe matching items, not the number of visible folders. Two authors touching different children do not make a folder a shared file. The existing unfiltered tree can still show identical entries. Search and change-type filters use the leaf scope too. A directory's leaf list is read locally on demand (filters/search), pinned to the displayed revisions; plain unfiltered browsing adds no history walk or descendant enumeration. Successful leaf results can be reused when changing layout at the same immutable scope; workspace refreshes are reread. Exclusions remain collapsed below the results. Filters, layout buttons and a short summary share the existing toolbar; explanations stay in Details. Submodule pointers remain atomic in the parent: internal authors require opening the submodule comparison, not a background scan of all submodules.

Expand an item's contributor details for the relevant authors and dated commit evidence. Each event identifies its kind, repository (full path on hover) and left/right history membership; that side is reachability, not proof its edits survive. Gitlink explanations distinguish authors updating the parent pointer from authors of files inside the submodule. View change opens parent-to-commit evidence in the correct repository; gitlink updates and conversions use submodule details instead of a file viewer. In Touched/Shared, merge-only edits are not attributed to the merge author. A missing/shallow history fails with an explicit error, not a claim of no contribution. Shared mode reuses the normal metadata response and does not initiate another history walk or fetch.

Folder/submodule conversion descendants are structural imports/removals, not individual file touches. All differences keeps their structural evidence separate from file contributors; Introduced by retains them with a conversion reason. The conversion root is still an atomic structural change. Independent file edits before or after a conversion can qualify in Touched/Shared; a conversion does not suppress their evidence. If content was edited during the conversion itself, the conversion alone cannot establish that individual authorship. Detection reuses changed gitlinks and the existing parent/commit trees, without opening submodule histories.

The separate **Require surviving contribution** option is under Details and applies only to a selected author in **Files touched by author**. Its active state stays visible in the summary, even when Details is closed. It narrows that same author's candidate population, unlike the intermediate-history filters above. Counts refer to comparison **items**, after the current view's path, search and state filters; rename aliases do not count as extra rows. Included content, grouped structural changes, inconclusive results and items without a surviving witness are distinct. Choosing a filter/author or navigating to another comparison/folder resets this advanced option; ordinary rerenders preserve an explicit choice. Cached strict results can still be reused when explicitly enabling it again.

The author menu is scoped to the actual loaded comparison paths (including the selected folder or submodule's inner folder), not every path in the commits. Mixed-folder commits contribute only their matching paths; authors with no matching touches are omitted. Metadata and strict results are cached separately for each path set and revision pair. Text/state filters narrow the displayed items and summary without restarting history analysis. A retained author with no matches in a newly selected folder shows no items, never all items silently.

Strict analysis reads local Git objects only. It traces surviving unique lines and atomic gitlink changes through both sides of the history. A bounded contextual fallback handles large UTF-8 text and repeated lines: a unique surrounding block identifies an occurrence, but the witness commit must also actually add the changed line or perform a standalone deletion. Editing only its neighbour is insufficient. Conversion commits do not establish authorship of imported file contents. Independent later content edits can still qualify.

Limits are explicit, not negative authorship claims: binary/NUL-encoded text, ambiguous origins, edited renames and changing context can remain inconclusive. Contextual analysis accepts up to 48 MiB per version, 512 witness windows (1024 candidates), 64 KiB per window and 128 changing history steps; the original unique-line trace allows 256 steps. The request has a 30-second/100,000-commit guard, fails closed and never applies partial results after a request error. Contextual buffers are bounded separately from the 64 MiB small-file provenance cache; there is no recursive submodule scan or fetch. The existing text-diff preview size limit is unchanged. Git authorship cannot prove whether a change was typed, copied or squash-imported.

**Introduced by author** is a separate, opt-in local history analysis. It includes ordinary commit changes and merge-result changes relative to the first parent (including resolution changes, without claiming they were manually edited). It is history involvement, not proof that every individual edit survived. A merge that keeps a file unchanged relative to its first parent does not establish introduction of that file by its author. Personal-touch, Shared and surviving-contribution results are not changed by this mode.

When opening a submodule's exact contents from a committed parent comparison, the app carries the parent revisions and gitlink path. The backend verifies both pointers and the exact local child repository. Each parent pointer update contributes only the differing child paths across **that update**, intersected with the current child comparison/folder, not every child path indiscriminately. The explanation shows the parent commit/date, old/new child revisions and internal file contributors separately. Changing child revisions drops this parent context; reversing the pair reverses both parent and child endpoints. An arbitrary child comparison does not invent a parent integration interval, and working-tree pointers do not have a recorded integration author.

Introduction analysis fails closed, with no partial results, for missing history/objects, shallow repositories, mismatched parent context or unsupported folder/gitlink conversions across the parent interval. It does not fetch. Request limits are 60 seconds, 100,000 walked commits, 512 distinct intermediate pointer pairs and 200,000 evidence/path entries. Evidence retains one witness per reason, side and source repository per author/item; it is not a complete commit list. There is no recursive expansion of every submodule: only the explicitly opened child and its immediate parent are analyzed. Expanding evidence logs the active mode, endpoint/witness IDs, paths, event kind, side and anonymous repository ID, never file contents, names, emails or remote credentials.

Tests: `node --test tests/comparison-authors.test.js tests/comparison-contributors.test.js` and `cargo test --lib repository::comparison_authors` (in `src-tauri`). Controlled histories include a 30 MB CDFX-like input, repetitive XML, shared/overwritten/reverted intermediate edits, merges, feature-to-feature comparisons and folder/gitlink conversions. Real company-repository attribution still needs validation against its actual witness commits.

## Merge preview

Branch Map offers **Preview merge with…** next to the existing merge action. It is also available from the branch sidebar and the merge dialog. The report explicitly shows incoming → current, pinned commit IDs, fast-forward/up-to-date status, paths kept from the current branch, incoming changes, automatically combined files and unresolved conflicts. Counts are relative to the current committed snapshot; automatically combined files are a subset of changed items. Expand individual rows for base/current/incoming/result and read-only file comparisons. Search and filters are local; long reports display 200 rows at a time.

The preview uses libgit2's `merge_commits` with the same defaults as the application's actual merge, not an authorship filter or an endpoint diff pretending to be a merge. It never checks out, writes the index, moves refs, starts a merge, commits, fetches or updates submodules. The engine can store newly merged blobs in the local object database. Local edits are excluded: a successful committed-snapshot preview does not bypass the real merge's clean-workspace checks. No textual conflict is not a guarantee of functional correctness. Recalculate after refs/configuration change or additional history is fetched; manual resolution choices are not predicted.

Submodule rows show parent gitlink decisions, not a recursive merge or checkout inside each child. If the engine cannot choose a pointer, the result remains unresolved. The simulated `.gitmodules` is checked for missing gitlink registrations using the existing merge validator. Commit messages, local tags and repository URLs can be loaded explicitly per submodule via the existing metadata reader, without fetching. Missing child objects remain unavailable, not fabricated. Shallow/unrelated histories and excessive reports fail explicitly. Multiple merge bases are handled by the engine, but the UI does not invent a single ancestor. Limits: 250,000 snapshot/index entries, 25,000 report items, 2 MiB UTF-8 text per file version; binary/oversized text is reported as unavailable without truncation.

Focused tests: `node --test tests/merge-preview.test.js` and `cargo test --lib branches::merge_preview`. Fixture tests compare simulated results with actual libgit2 merges in disposable repositories and verify unchanged worktree, staging, refs and merge state after previews. Company-repository/Windows behavior still needs verification on that workstation.

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
