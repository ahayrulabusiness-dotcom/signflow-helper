// No console window behind the tray app in a release build.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Everything lives in this one binary (no library target) so the unit tests
// run inside a test executable that carries the same Windows manifest as the
// app; a library test binary that links Tauri can fail to start on Windows.
mod api;
mod app;
mod blob;
mod config;
mod link;
mod logfile;
mod session;
mod store;
mod update;
mod watcher;

fn main() {
    app::run()
}
