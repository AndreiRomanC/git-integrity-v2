# Git Integrity

A cross-platform visual Git client inspired by the information density and structured workflows of MKS Integrity, while using standard Git terminology.

## Architecture

- Tauri 2 desktop shell
- Rust backend with an embedded libgit2 engine
- Plain HTML, CSS and JavaScript frontend
- No Node.js or npm runtime/build step
- SVG branch graph

The source project is under 1 MB. Build artifacts are written to the operating system's temporary directory by the launchers and removed when the application closes, so the project does not grow by gigabytes.

## Browser preview

Open `frontend/index.html` directly or serve the folder with any static server. Browser mode uses representative demo data because a normal web page cannot access local repositories.

## Desktop development

On this Mac, double-click `run-macos.command`. It opens the optimized standalone application from `dist/Git Integrity.app` and does not compile or install anything.

Alternatively, run:

```sh
cd src-tauri
cargo tauri dev
```

`cargo run` from `src-tauri` also starts the application without installing the Tauri CLI.

## Windows

The Windows application is a portable `git-integrity.exe`. End users need neither Node.js, Rust, nor an installer.

Most Git operations (browsing, staging, committing, branching, history, submodule version switching) run entirely through the embedded libgit2 engine and need nothing else installed. **Network operations — push, fetch, pull, and "restore from remote"** — shell out to the system `git` command so they transparently reuse whatever credentials (SSH agent, credential helper, OS keychain) already work in a terminal on that machine, instead of libgit2's much narrower built-in credential search. This means **Git for Windows must be installed and on `PATH`** for those specific actions; everything else works without it.

Build the executable on a Windows build machine with:

```powershell
.\build-windows.ps1
```

The script writes `dist\windows\git-integrity.exe`, rejects files over 30 MB, and removes temporary build artifacts. The included GitHub Actions workflow can perform the Windows build without installing build tools on the user's computer.

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
