#![cfg_attr(windows, windows_subsystem = "windows")]
// 実行ファイル名(= クレート名)を MyDictionary にしているため
#![allow(non_snake_case)]

mod bench;
mod bookmarks;
mod config;
#[cfg(windows)]
mod dialogs;
mod core;
mod import;
mod json_io;
mod safe;
#[cfg(windows)]
mod ui;
#[cfg(windows)]
mod gui;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--bench") {
        let n = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(100_000);
        let report = bench::run(n);
        #[cfg(windows)]
        gui::show_report(&report);
        #[cfg(not(windows))]
        println!("{report}");
        return;
    }
    #[cfg(windows)]
    gui::run();
    #[cfg(not(windows))]
    eprintln!("MyDictionary は Windows 専用です(テストは `cargo test`、計測は `--bench` で実行できます)。");
}
