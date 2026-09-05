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
    println!("cargo:rustc-env=GIT_INTEGRITY_BUILD_SHA={sha}");
    tauri_build::build()
}
