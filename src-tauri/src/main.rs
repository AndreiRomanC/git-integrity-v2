// Release builds are GUI applications; keep the console for development builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() {
    git_drilldown_lib::run();
}
