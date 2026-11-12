//! ライセンス表示: このアプリ自身のライセンス(LICENSE)と、
//! 使用しているオープンソースソフトウェアのライセンス(THIRD-PARTY-NOTICES.txt)。
//!
//! どちらも実行ファイルに埋め込み、設定画面の「ライセンス情報」から表示する。
//! THIRD-PARTY-NOTICES.txt は `python tools/third_party_notices.py` で作り直す。

/// このアプリのライセンス(MIT License)
pub const APP_LICENSE: &str = include_str!("../LICENSE");
/// 使用しているオープンソースソフトウェアのライセンス
pub const THIRD_PARTY_NOTICES: &str = include_str!("../THIRD-PARTY-NOTICES.txt");
/// ソースコードの置き場所
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// ライセンス画面に表示する全文。改行は `\n`
pub fn full_text(app_name: &str, app_version: &str) -> String {
    let mut s = String::new();
    s.push_str(&format!("{app_name} {app_version}\n{REPOSITORY}\n\n"));
    s.push_str(&normalize(APP_LICENSE));
    s.push_str("\n\n\n");
    s.push_str(&normalize(THIRD_PARTY_NOTICES));
    s.push('\n');
    s
}

/// 改行を `\n` にそろえ、末尾の空白行を落とす(チェックアウト時に CRLF になっていてもよいように)
fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n").trim_end().to_string()
}

/// EDIT コントロールに渡すため、改行を `\r\n` にする
pub fn to_crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THIRD-PARTY-NOTICES.txt の Components 欄にある「名前 版」の一覧(Rust 標準ライブラリと SQLite を除く)
    fn listed_crates() -> Vec<(String, String)> {
        let text = normalize(THIRD_PARTY_NOTICES);
        let start = text.find("\nComponents\n").expect("Components 欄がない");
        let body = &text[start..];
        let body = &body[body.find("\n\n").unwrap()..];
        let end = body.find("\n=====").unwrap();
        body[..end]
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with(' '))
            .filter(|l| !l.starts_with("Rust standard library") && !l.starts_with("SQLite "))
            .map(|l| {
                let (name, ver) = l.rsplit_once(' ').expect("「名前 版」の形でない");
                (name.to_string(), ver.to_string())
            })
            .collect()
    }

    fn lock_has(name: &str, version: &str) -> bool {
        let lock = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.lock")).unwrap().replace("\r\n", "\n");
        lock.contains(&format!("name = \"{name}\"\nversion = \"{version}\"\n"))
    }

    #[test]
    fn full_text_has_app_license_and_notices() {
        let t = full_text("MyDictionary", "9.99");
        assert!(t.starts_with("MyDictionary 9.99\nhttps://github.com/"));
        assert!(t.contains("MIT License\n\nCopyright (c) 2026 ParticlePudding"));
        assert!(t.contains("THIRD-PARTY SOFTWARE NOTICES AND INFORMATION"));
        assert!(!t.contains('\r'));
    }

    #[test]
    fn notices_list_main_dependencies() {
        let names: Vec<String> = listed_crates().into_iter().map(|(n, _)| n).collect();
        for dep in ["rusqlite", "libsqlite3-sys", "serde_json", "windows", "windows-core"] {
            assert!(names.iter().any(|n| n == dep), "{dep} が THIRD-PARTY-NOTICES.txt にない");
        }
        let t = normalize(THIRD_PARTY_NOTICES);
        assert!(t.contains("\nRust standard library\n"));
        assert!(t.contains("\nSQLite 3."));
    }

    /// 依存を更新したのに THIRD-PARTY-NOTICES.txt を作り直していなければ失敗する
    #[test]
    fn notices_match_cargo_lock() {
        let crates = listed_crates();
        assert!(crates.len() >= 10);
        for (name, ver) in &crates {
            assert!(
                lock_has(name, ver),
                "{name} {ver} が Cargo.lock にない。`python tools/third_party_notices.py` で THIRD-PARTY-NOTICES.txt を作り直す"
            );
        }
        // 主な依存は、Cargo.lock の版がそのまま載っていること
        for dep in ["rusqlite", "serde_json", "windows"] {
            assert!(crates.iter().any(|(n, v)| n == dep && lock_has(n, v)), "{dep} の版が Cargo.lock と合わない");
        }
    }

    /// 各ライセンスの文面がそろっていること(MIT の本文が、載せたクレートの数だけの区分にある)
    #[test]
    fn notices_include_license_texts() {
        let t = normalize(THIRD_PARTY_NOTICES);
        let sections = t.matches("Permission is hereby granted, free of charge").count();
        assert!(sections >= 5);
        // 区分は「Used by:」で始まる。MIT の区分の数と本文の数が合うこと(残り1つは SQLite のパブリックドメイン)
        assert_eq!(t.matches("\nUsed by:\n").count(), sections + 1);
        for (name, ver) in listed_crates() {
            assert!(t.contains(&format!("\n  {name} {ver}\n")), "{name} {ver} のライセンス文面の区分がない");
        }
    }

    #[test]
    fn crlf_conversion() {
        assert_eq!(to_crlf("a\nb\r\nc"), "a\r\nb\r\nc");
        assert_eq!(normalize("x\r\ny\r\n\r\n"), "x\ny");
    }
}
