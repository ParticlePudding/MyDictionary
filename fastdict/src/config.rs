//! 設定ファイル(INI形式)の解析。GUI に依存しない。
//!
//! 例:
//! ```ini
//! [general]
//! hotkey = Ctrl+Alt+D
//! close_to_tray = true
//!
//! [dictionary.main]
//! name = メイン
//! path = dictionary.db
//!
//! [dictionary.ref]
//! name = 参照用
//! path = D:\dict\ref.db
//! readonly = true
//! ```

use std::path::{Path, PathBuf};

pub const MOD_ALT: u32 = 1;
pub const MOD_CONTROL: u32 = 2;
pub const MOD_SHIFT: u32 = 4;
pub const MOD_WIN: u32 = 8;

#[derive(Clone, Debug, PartialEq)]
pub struct Hotkey {
    pub mods: u32,
    pub vk: u32,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DictConf {
    pub name: String,
    pub path: PathBuf,
    pub readonly: bool,
}

/// 候補リストの並び順
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sort {
    /// 見出し語順(検索中は一致度の高い順)
    Word,
    /// 更新日時の古い順
    UpdatedAsc,
    /// 更新日時の新しい順
    UpdatedDesc,
    /// しおりに追加した順(しおり表示のときだけ。設定には保存しない)
    Bookmark,
}

impl Sort {
    pub fn key(self) -> &'static str {
        match self {
            Sort::UpdatedAsc => "updated_asc",
            Sort::UpdatedDesc => "updated_desc",
            _ => "word",
        }
    }

    pub fn from_key(s: &str) -> Option<Sort> {
        match s.trim().to_ascii_lowercase().as_str() {
            "word" => Some(Sort::Word),
            "updated_asc" => Some(Sort::UpdatedAsc),
            "updated_desc" => Some(Sort::UpdatedDesc),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    /// None ならホットキー無効
    pub hotkey: Option<Hotkey>,
    pub close_to_tray: bool,
    pub sort: Sort,
    pub dicts: Vec<DictConf>,
}

impl Config {
    pub fn default_for(base: &Path) -> Config {
        Config {
            hotkey: parse_hotkey("Ctrl+Alt+D"),
            close_to_tray: true,
            sort: Sort::Word,
            dicts: vec![DictConf { name: "メイン".into(), path: base.join("dictionary.db"), readonly: false }],
        }
    }
}

pub fn default_ini() -> String {
    "; fastdict 設定ファイル(UTF-8)。編集後はトレイアイコンの右クリック →「設定を再読み込み」で反映されます。\r\n\
; 先頭が ; または # の行はコメントです。\r\n\
\r\n\
[general]\r\n\
; 呼び出しホットキー。修飾キー(Ctrl / Alt / Shift / Win)を1つ以上 + キー(A-Z, 0-9, F1-F24, Space)。none で無効。\r\n\
hotkey = Ctrl+Alt+D\r\n\
; 閉じるボタンで終了せずトレイに隠す(false なら閉じると終了)\r\n\
close_to_tray = true\r\n\
\r\n\
; 辞書の定義。[dictionary.任意のID] を必要なだけ書きます。先頭が既定の登録先です。\r\n\
; path は相対パスなら、この設定ファイルのあるフォルダ基準。%APPDATA% などの環境変数も使えます。\r\n\
; ファイルが無ければ自動で作成されます(readonly = true の場合を除く)。\r\n\
[dictionary.main]\r\n\
name = メイン\r\n\
path = dictionary.db\r\n\
\r\n\
; 参照専用の辞書の例(行頭の ; を外して使います)\r\n\
;[dictionary.ref]\r\n\
;name = 参照用\r\n\
;path = D:\\dict\\ref.db\r\n\
;readonly = true\r\n"
        .to_string()
}

/// 修飾キーとキーコードから Hotkey を作る(表現できないキーは None)
pub fn hotkey_from_parts(mods: u32, vk: u32) -> Option<Hotkey> {
    let mut parts: Vec<String> = Vec::new();
    for (bit, name) in [(MOD_CONTROL, "Ctrl"), (MOD_ALT, "Alt"), (MOD_SHIFT, "Shift"), (MOD_WIN, "Win")] {
        if mods & bit != 0 {
            parts.push(name.to_string());
        }
    }
    let key = match vk {
        0x20 => "Space".to_string(),
        0x30..=0x39 | 0x41..=0x5A => (vk as u8 as char).to_string(),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        _ => return None,
    };
    parts.push(key);
    parse_hotkey(&parts.join("+"))
}

pub fn parse_hotkey(s: &str) -> Option<Hotkey> {
    let mut mods = 0u32;
    let mut vk: Option<u32> = None;
    let mut names: Vec<String> = Vec::new();
    for part in s.split('+').map(|p| p.trim()).filter(|p| !p.is_empty()) {
        let low = part.to_ascii_lowercase();
        let m = match low.as_str() {
            "ctrl" | "control" => Some((MOD_CONTROL, "Ctrl")),
            "alt" => Some((MOD_ALT, "Alt")),
            "shift" => Some((MOD_SHIFT, "Shift")),
            "win" | "windows" => Some((MOD_WIN, "Win")),
            _ => None,
        };
        if let Some((bit, name)) = m {
            if mods & bit == 0 {
                mods |= bit;
                names.push(name.to_string());
            }
            continue;
        }
        if vk.is_some() {
            return None; // キーは1つだけ
        }
        let (code, label) = if low == "space" {
            (0x20, "Space".to_string())
        } else if low.len() == 1 && low.as_bytes()[0].is_ascii_alphanumeric() {
            let c = low.as_bytes()[0].to_ascii_uppercase();
            (c as u32, (c as char).to_string())
        } else if low.starts_with('f') && low[1..].parse::<u32>().map_or(false, |n| (1..=24).contains(&n)) {
            let n: u32 = low[1..].parse().unwrap();
            (0x6F + n, format!("F{n}"))
        } else {
            return None;
        };
        vk = Some(code);
        names.push(label);
    }
    let vk = vk?;
    if mods == 0 {
        return None; // 修飾キー無しのグローバルホットキーは通常のタイピングを奪うため不可
    }
    Some(Hotkey { mods, vk, text: names.join("+") })
}

/// 設定をINIテキストにする(設定ダイアログからの保存用)。
/// `base` と同じフォルダにある辞書はファイル名だけで書く。
pub fn to_ini(cfg: &Config, base: &Path) -> String {
    let mut s = String::new();
    s.push_str("; fastdict 設定ファイル(UTF-8)。設定ダイアログからも変更できます。\r\n\r\n[general]\r\n");
    s.push_str(&format!("hotkey = {}\r\n", cfg.hotkey.as_ref().map_or("none".to_string(), |h| h.text.clone())));
    s.push_str(&format!("close_to_tray = {}\r\n", cfg.close_to_tray));
    s.push_str(&format!("sort = {}\r\n", cfg.sort.key()));
    for (i, d) in cfg.dicts.iter().enumerate() {
        let path = match d.path.parent() {
            Some(p) if p == base => d.path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
            _ => d.path.to_string_lossy().to_string(),
        };
        s.push_str(&format!("\r\n[dictionary.d{}]\r\nname = {}\r\npath = {}\r\n", i + 1, d.name, path));
        if d.readonly {
            s.push_str("readonly = true\r\n");
        }
    }
    s
}

/// `%NAME%` を環境変数で置換する。未定義ならそのまま残す。
pub fn expand_env(s: &str, getenv: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let name = &after[..end];
                match getenv(name) {
                    Some(v) => out.push_str(&v),
                    None => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

#[derive(Default)]
struct PendingDict {
    id: String,
    name: Option<String>,
    path: Option<String>,
    readonly: bool,
}

/// 設定を解析する。不正な行は無視して警告として返す(起動は止めない)。
pub fn parse(text: &str, base: &Path, getenv: &dyn Fn(&str) -> Option<String>) -> (Config, Vec<String>) {
    let mut cfg = Config::default_for(base);
    cfg.dicts.clear();
    let mut warns = Vec::new();
    let mut section = String::new();
    let mut pending: Option<PendingDict> = None;

    let finish = |p: Option<PendingDict>, cfg: &mut Config, warns: &mut Vec<String>| {
        if let Some(p) = p {
            match p.path {
                Some(path) => {
                    let expanded = expand_env(&path, getenv);
                    let pb = PathBuf::from(&expanded);
                    let full = if pb.is_absolute() { pb } else { base.join(pb) };
                    let name = p.name.unwrap_or_else(|| p.id.clone());
                    cfg.dicts.push(DictConf { name, path: full, readonly: p.readonly });
                }
                None => warns.push(format!("設定: 辞書「{}」に path がありません", p.id)),
            }
        }
    };

    for (n, raw) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            finish(pending.take(), &mut cfg, &mut warns);
            section = line[1..line.len() - 1].trim().to_string();
            if let Some(id) = section.strip_prefix("dictionary") {
                let id = id.trim_start_matches('.').to_string();
                let id = if id.is_empty() { format!("辞書{}", cfg.dicts.len() + 1) } else { id };
                pending = Some(PendingDict { id, ..Default::default() });
            } else if section != "general" {
                warns.push(format!("設定: {}行目 未知のセクション [{}]", n + 1, section));
            }
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            warns.push(format!("設定: {}行目 「key = value」の形式ではありません", n + 1));
            continue;
        };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        if let Some(p) = pending.as_mut() {
            match k.as_str() {
                "name" => p.name = Some(v.to_string()),
                "path" => p.path = Some(v.to_string()),
                "readonly" => match parse_bool(v) {
                    Some(b) => p.readonly = b,
                    None => warns.push(format!("設定: {}行目 readonly は true / false で指定してください", n + 1)),
                },
                _ => warns.push(format!("設定: {}行目 未知のキー {}", n + 1, k)),
            }
        } else if section == "general" {
            match k.as_str() {
                "hotkey" => {
                    if v.eq_ignore_ascii_case("none") || v.is_empty() {
                        cfg.hotkey = None;
                    } else {
                        match parse_hotkey(v) {
                            Some(h) => cfg.hotkey = Some(h),
                            None => warns.push(format!("設定: {}行目 ホットキー「{}」を解釈できません(既定を使用)", n + 1, v)),
                        }
                    }
                }
                "close_to_tray" => match parse_bool(v) {
                    Some(b) => cfg.close_to_tray = b,
                    None => warns.push(format!("設定: {}行目 close_to_tray は true / false", n + 1)),
                },
                "sort" => match Sort::from_key(v) {
                    Some(x) => cfg.sort = x,
                    None => warns.push(format!("設定: {}行目 sort は word / updated_asc / updated_desc", n + 1)),
                },
                "timing_log" => {} // 廃止した設定(古い設定ファイルに残っていても無視する)
                _ => warns.push(format!("設定: {}行目 未知のキー {}", n + 1, k)),
            }
        } else {
            warns.push(format!("設定: {}行目 セクションの外に設定があります", n + 1));
        }
    }
    finish(pending.take(), &mut cfg, &mut warns);

    if cfg.dicts.is_empty() {
        cfg.dicts = Config::default_for(base).dicts;
    }
    (cfg, warns)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(k: &str) -> Option<String> {
        (k == "APPDATA").then(|| "/ap".to_string())
    }

    #[test]
    fn hotkey_parse() {
        let h = parse_hotkey("ctrl + alt + d").unwrap();
        assert_eq!((h.mods, h.vk, h.text.as_str()), (MOD_CONTROL | MOD_ALT, 'D' as u32, "Ctrl+Alt+D"));
        assert_eq!(parse_hotkey("Win+Shift+F12").unwrap().vk, 0x6F + 12);
        assert_eq!(parse_hotkey("Ctrl+Space").unwrap().vk, 0x20);
        assert!(parse_hotkey("D").is_none()); // 修飾キー無し
        assert!(parse_hotkey("Ctrl+A+B").is_none());
        assert!(parse_hotkey("Ctrl+Alt+").is_none());
        assert!(parse_hotkey("Ctrl+F25").is_none());
    }

    #[test]
    fn env_expand() {
        assert_eq!(expand_env("%APPDATA%/a.db", &env), "/ap/a.db");
        assert_eq!(expand_env("%NOPE%\\a", &env), "%NOPE%\\a");
        assert_eq!(expand_env("100%", &env), "100%");
    }

    #[test]
    fn default_ini_roundtrip() {
        let base = Path::new("/base");
        let (cfg, warns) = parse(&default_ini(), base, &env);
        assert!(warns.is_empty(), "{warns:?}");
        assert_eq!(cfg.hotkey.as_ref().unwrap().text, "Ctrl+Alt+D");
        assert!(cfg.close_to_tray);
        assert_eq!(cfg.dicts.len(), 1);
        assert_eq!(cfg.dicts[0].name, "メイン");
        assert_eq!(cfg.dicts[0].path, base.join("dictionary.db"));
    }

    #[test]
    fn multiple_dicts_and_errors() {
        let text = "\u{feff}[general]\nhotkey = Ctrl+Shift+F2\ntiming_log = yes\nbogus = 1\n\
[dictionary.a]\nname = A\npath = a.db\n\
[dictionary.b]\npath = %APPDATA%/b.db\nreadonly = true\n\
[dictionary.c]\nname = パス無し\n\
[wat]\nx\n";
        let (cfg, warns) = parse(text, Path::new("/base"), &env);
        assert_eq!(cfg.hotkey.unwrap().text, "Ctrl+Shift+F2");
        assert_eq!(cfg.dicts.len(), 2);
        assert_eq!(cfg.dicts[1].name, "b"); // name 省略時はID
        assert!(cfg.dicts[1].readonly);
        assert_eq!(cfg.dicts[1].path, PathBuf::from("/ap/b.db"));
        assert!(warns.iter().any(|w| w.contains("bogus")));
        assert!(warns.iter().any(|w| w.contains("path がありません")));
        assert!(warns.iter().any(|w| w.contains("未知のセクション")));
    }

    #[test]
    fn to_ini_roundtrip() {
        let base = Path::new("/base");
        let cfg = Config {
            hotkey: parse_hotkey("Ctrl+Shift+F5"),
            close_to_tray: false,
            sort: Sort::UpdatedDesc,
            dicts: vec![
                DictConf { name: "メイン".into(), path: base.join("dictionary.db"), readonly: false },
                DictConf { name: "参照 用".into(), path: PathBuf::from("/other/dir/ref.db"), readonly: true },
            ],
        };
        let (back, warns) = parse(&to_ini(&cfg, base), base, &env);
        assert!(warns.is_empty(), "{warns:?}");
        assert_eq!(back.hotkey, cfg.hotkey);
        assert!(!back.close_to_tray);
        assert_eq!(back.sort, Sort::UpdatedDesc);
        assert_eq!(back.dicts, cfg.dicts);
        // ホットキー無し
        let none = Config { hotkey: None, ..cfg };
        assert!(parse(&to_ini(&none, base), base, &env).0.hotkey.is_none());
    }

    #[test]
    fn hotkey_parts() {
        let h = hotkey_from_parts(MOD_CONTROL | MOD_ALT, 'D' as u32).unwrap();
        assert_eq!(h.text, "Ctrl+Alt+D");
        assert_eq!(hotkey_from_parts(MOD_SHIFT | MOD_CONTROL, 0x74).unwrap().text, "Ctrl+Shift+F5");
        assert!(hotkey_from_parts(MOD_CONTROL, 0x1B).is_none()); // Esc は不可
        assert!(hotkey_from_parts(0, 'A' as u32).is_none()); // 修飾キー無し
    }

    #[test]
    fn hotkey_none_and_empty_file() {
        let (cfg, _) = parse("[general]\nhotkey = none\n", Path::new("/b"), &env);
        assert!(cfg.hotkey.is_none());
        assert_eq!(cfg.dicts.len(), 1); // 辞書未定義なら既定
        let (cfg, w) = parse("", Path::new("/b"), &env);
        assert!(w.is_empty() && cfg.hotkey.is_some());
    }
}
