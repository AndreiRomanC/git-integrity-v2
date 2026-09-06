fn main() {
    // Embeds the exact commit this binary was built from, so "which build am
    // I running" is answerable by looking at the app itself instead of
    // guessing from a file's modified time — the reason the wrong (hours-old)
    // Windows executable went untested for so long during today's fixes.
    // Falls back to "unknown" (never a build failure) when git isn't
    // available at build time, e.g. a source archive without a .git folder.
    let sha = std::process::Command::new("git")
        .args(["rev-parse", "--short=10", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    // Without this, a binary built from a working tree with uncommitted
    // changes reported the same SHA as a clean build of that exact commit —
    // indistinguishable in the UI, even though its actual source doesn't
    // match that commit at all. `git status --porcelain` here (not
    // `diff --quiet`, which only covers tracked-file *modifications`) also
    // catches untracked new files, matching what a real "is this tree dirty"
    // check needs to mean.
    let dirty = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| !output.stdout.is_empty())
        .unwrap_or(false);
    let sha = if dirty { format!("{sha}-dirty") } else { sha };
    println!("cargo:rustc-env=GIT_INTEGRITY_BUILD_SHA={sha}");
    // Re-run build.rs (and so re-check dirtiness) on every build — without
    // this, cargo only reruns it when build.rs itself or files it explicitly
    // declares change, so editing repository.rs without touching build.rs
    // could leave a stale -dirty (or stale clean) SHA baked in from the
    // previous build.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/index");
    tauri_build::build()
}
