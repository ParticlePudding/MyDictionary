#![cfg_attr(windows, windows_subsystem = "windows")]

mod core;
#[cfg(windows)]
mod gui;

#[cfg(windows)]
fn main() {
    gui::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("fastdict は Windows 専用です(テストは `cargo test` で実行できます)。");
}
