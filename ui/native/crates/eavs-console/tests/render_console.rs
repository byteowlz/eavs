//! Headless renders of the console (see `src/render.rs`). Runs on the main
//! thread, which the macOS platform requires; skipped unless
//! `EAVS_CONSOLE_RENDER=<dir>` is set.

#![allow(dead_code, unused_imports)]

#[path = "../src/app.rs"]
mod app;
#[path = "../src/config.rs"]
mod config;
#[path = "../src/render.rs"]
mod render;
#[path = "../src/theme.rs"]
mod theme;
#[path = "../src/ui.rs"]
mod ui;

fn main() {
    render::run();
}
