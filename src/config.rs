//! 設定ファイル(INI形式)の解析。GUI に依存しない。
//!
//! 例:
//! ```ini
//! [general]
//! hotkey = Ctrl+Alt+D
//! close_to_tray = false
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

#[derive(Clone, Debug)]
pub struct DictConf {
    /// 設定ファイルでの ID(`[dictionary.<ID>]`)。保存しても変わらないように覚えておく。空なら保存のときに付ける
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub readonly: bool,
    /// 設定ファイルに書かれていたとおりの path の値と、それを解決したパス。
    /// パスを変えていなければ、保存のときも書かれていたとおりに書く(相対パスや %APPDATA% を保つため)
    pub written: Option<(String, PathBuf)>,
}

/// 比べるのは、辞書の中身(ID・名前・パス・参照のみ)だけ。書かれていた形(`written`)は比べない
impl PartialEq for DictConf {
    fn eq(&self, o: &Self) -> bool {
        self.id == o.id && self.name == o.name && self.path == o.path && self.readonly == o.readonly
    }
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

/// 画面の配色
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Theme {
    /// 明るい配色(既定)
    Light,
    /// 暗い配色
    Dark,
    /// Windows の設定(アプリの色)に合わせる
    System,
}

impl Theme {
    pub fn key(self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "dark",
            Theme::System => "system",
        }
    }

    pub fn from_key(s: &str) -> Option<Theme> {
        match s.trim().to_ascii_lowercase().as_str() {
            "light" => Some(Theme::Light),
            "dark" => Some(Theme::Dark),
            "system" => Some(Theme::System),
            _ => None,
        }
    }

    /// 設定の画面に表示する名前
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn label(self) -> &'static str {
        match self {
            Theme::Light => "明るい",
            Theme::Dark => "暗い",
            Theme::System => "Windows の設定に合わせる",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    /// None ならホットキー無効
    pub hotkey: Option<Hotkey>,
    pub close_to_tray: bool,
    pub sort: Sort,
    /// ウィンドウのタイトルに表示する名前(何の辞書かを示す)。そのまま表示する。空ならアプリの名前
    pub title: String,
    /// 共用フォルダでの運用(この設定ファイルを複数人で使う)。
    /// 個人ごとの情報を設定の場所に書き込まないよう、しおりを無効にし、並び順も保存しない
    pub shared: bool,
    /// 画面の配色(変更は、次に起動したときから反映する)
    pub theme: Theme,
    /// 暗い配色のとき、画像の透明な部分を暗い地に重ねる(既定は明るい地。黒い線の図が見えるように)
    pub image_dark_bg: bool,
    pub dicts: Vec<DictConf>,
}

impl Config {
    pub fn default_for(base: &Path) -> Config {
        Config {
            hotkey: parse_hotkey("Ctrl+Alt+D"),
            close_to_tray: false,
            sort: Sort::Word,
            title: String::new(),
            shared: false,
            theme: Theme::Light,
            image_dark_bg: false,
            dicts: vec![DictConf { id: "main".into(), name: "メイン".into(), path: base.join("dictionary.db"), readonly: false, written: None }],
        }
    }
}

pub fn default_ini() -> String {
    "; MyDictionary 設定ファイル(UTF-8)。編集後はトレイアイコンの右クリック →「設定ファイルを再読み込み」で反映されます。\r\n\
; 先頭が ; または # の行はコメントです。\r\n\
\r\n\
[general]\r\n\
; 呼び出しホットキー。修飾キー(Ctrl / Alt / Shift / Win)を1つ以上 + キー(A-Z, 0-9, F1-F24, Space)。none で無効。\r\n\
hotkey = Ctrl+Alt+D\r\n\
; 閉じるボタン / Esc で終了せず、トレイに隠して常駐する(既定は false = 閉じると終了)\r\n\
close_to_tray = false\r\n\
\r\n\
; ウィンドウのタイトルに表示する名前(例: 社内用語集)。書いたとおりに表示します。空なら MyDictionary\r\n\
title =\r\n\
; 共用フォルダで運用する(この設定ファイルを複数人で使う)ときは true。\r\n\
; しおりが無効になり、並び順の変更も保存されなくなります\r\n\
shared = false\r\n\
; 画面の配色: light(明るい)/ dark(暗い)/ system(Windows の設定に合わせる)。次に起動したときから反映されます\r\n\
theme = light\r\n\
; 暗い配色で、画像の透明な部分を重ねる地: light(明るい。黒い線の図が見える)/ dark(暗い。白い線の図や切り抜き写真が自然に見える)\r\n\
;image_background = dark\r\n\
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

/// ウィンドウのタイトル。設定した名前をそのまま表示する(何も足さない)。空ならアプリの名前
pub fn window_title(cfg: &Config, app: &str) -> String {
    let name = cfg.title.trim();
    if name.is_empty() {
        app.to_string()
    } else {
        name.to_string()
    }
}

/// 設定をINIテキストにする(設定ダイアログからの保存用)。
/// `base` と同じフォルダにある辞書はファイル名だけで書く。
pub fn to_ini(cfg: &Config, base: &Path) -> String {
    let mut s = String::new();
    // 設定の画面から保存すると、ファイル全体を書き直す(手で書いたコメントは残らない)。
    // その代わり、各項目の説明を毎回書いておく
    s.push_str("; MyDictionary 設定ファイル(UTF-8)。設定の画面から保存すると、この説明以外のコメントは消えます。\r\n");
    s.push_str("; 先頭が ; の行と、値の後ろの「 ; 」から行末までは、コメントです。\r\n");
    s.push_str("; 値に「 ;」を含めるときや、前後の空白を残すときは、値全体を \"…\" で囲みます。\r\n\r\n[general]\r\n");
    s.push_str("; 呼び出しホットキー(例: Ctrl+Alt+D)。none で無効\r\n");
    s.push_str(&format!("hotkey = {}\r\n", cfg.hotkey.as_ref().map_or("none".to_string(), |h| h.text.clone())));
    s.push_str("; true にすると、閉じてもトレイに常駐する\r\n");
    s.push_str(&format!("close_to_tray = {}\r\n", cfg.close_to_tray));
    s.push_str("; 候補の並び順: word(見出し語順)/ updated_asc / updated_desc\r\n");
    s.push_str(&format!("sort = {}\r\n", cfg.sort.key()));
    s.push_str("; ウィンドウに表示する名前。空なら MyDictionary\r\n");
    s.push_str(&format!("title = {}\r\n", ini_value(&cfg.title)));
    s.push_str("; 共用フォルダで運用する(しおりは無効、並び順は保存しない)\r\n");
    s.push_str(&format!("shared = {}\r\n", cfg.shared));
    // 既定(明るい)のときは書かない(この設定を知らない古い版が、同じ設定ファイルを読んでも警告を出さないように)
    if cfg.theme != Theme::Light {
        s.push_str(&format!("theme = {}\r\n", cfg.theme.key()));
    }
    if cfg.image_dark_bg {
        s.push_str("; 暗い配色で、画像の透明な部分を重ねる地: light(明るい)/ dark(暗い)\r\n");
        s.push_str("image_background = dark\r\n");
    }
    s.push_str("\r\n; 辞書。先頭が既定の登録先。path の相対パスは、この設定ファイルのあるフォルダが基準\r\n");
    let mut used: Vec<String> = Vec::new();
    for (i, d) in cfg.dicts.iter().enumerate() {
        // パスを変えていなければ、書かれていたとおりに書く。設定ファイルのフォルダの中なら、そこからの相対パスで書く
        let path = match (&d.written, d.path.strip_prefix(base)) {
            (Some((text, full)), _) if *full == d.path => text.clone(),
            (_, Ok(rel)) if d.path.is_absolute() && !rel.as_os_str().is_empty() => rel.to_string_lossy().to_string(),
            _ => d.path.to_string_lossy().to_string(),
        };
        // ID は、読み込んだときのものを使う(無い・重なるときは、空いている d<番号> を付ける)
        let valid = |id: &str| !id.is_empty() && !id.contains([']', '\r', '\n']) && !used.iter().any(|u| u.eq_ignore_ascii_case(id));
        let id = if valid(&d.id) {
            d.id.clone()
        } else {
            (i + 1..).map(|n| format!("d{n}")).find(|c| valid(c)).unwrap_or_default()
        };
        used.push(id.clone());
        s.push_str(&format!("\r\n[dictionary.{}]\r\nname = {}\r\npath = {}\r\n", id, ini_value(&d.name), ini_value(&path)));
        if d.readonly {
            s.push_str("readonly = true\r\n");
        }
    }
    s
}

/// 設定ファイルの文章のうち、[general] の `key` の行だけを `key = value` に書き換える(ほかの行・コメントは残す)。
/// その行が無ければ [general] の直後に足し、[general] も無ければ先頭に足す。改行の書き方は元に合わせる
pub fn set_general_key(text: &str, key: &str, value: &str) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let mut in_general = false;
    let mut header: Option<usize> = None;
    for i in 0..lines.len() {
        let t = strip_comment(lines[i].trim()).trim().to_string();
        if t.starts_with('[') && t.ends_with(']') {
            in_general = t[1..t.len() - 1].trim().eq_ignore_ascii_case("general");
            if in_general && header.is_none() {
                header = Some(i);
            }
            continue;
        }
        if in_general && !t.starts_with(';') && !t.starts_with('#') {
            if let Some((k, _)) = t.split_once('=') {
                if k.trim().eq_ignore_ascii_case(key) {
                    lines[i] = format!("{key} = {value}");
                    return lines.join(nl) + nl;
                }
            }
        }
    }
    match header {
        Some(h) => lines.insert(h + 1, format!("{key} = {value}")),
        None => {
            lines.insert(0, String::new());
            lines.insert(0, format!("{key} = {value}"));
            lines.insert(0, "[general]".into());
        }
    }
    lines.join(nl) + nl
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

/// 値の後ろのコメントを外す。空白(タブ)の後ろの `;` から行末までをコメントとみなす
/// (`shared = true   ; 共用で運用する` の形。値の中の `;` は、直前が空白でなければ値の一部)
fn strip_comment(v: &str) -> &str {
    // "…" で囲んだ値: 閉じる " の後ろが、空か「;」で始まるコメントなら、囲みの中をそのまま値とする
    let t = v.trim_start();
    if let Some(body) = t.strip_prefix('"') {
        for (i, _) in body.match_indices('"').collect::<Vec<_>>().into_iter().rev() {
            let rest = body[i + 1..].trim_start();
            if rest.is_empty() || rest.starts_with(';') {
                return &body[..i];
            }
        }
    }
    let b = v.as_bytes();
    for i in 1..b.len() {
        if b[i] == b';' && (b[i - 1] == b' ' || b[i - 1] == b'\t') {
            return &v[..i];
        }
    }
    v
}

/// 保存する値。そのままでは読み戻せない値(「 ;」を含む、前後に空白がある、" で始まる)は "…" で囲む
fn ini_value(v: &str) -> String {
    let needs = v.contains(" ;") || v.contains("\t;") || v.starts_with('"') || v.trim() != v;
    if needs { format!("\"{v}\"") } else { v.to_string() }
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
    let mut seen_ids: Vec<String> = Vec::new();

    let finish = |p: Option<PendingDict>, cfg: &mut Config, warns: &mut Vec<String>| {
        if let Some(p) = p {
            match p.path {
                Some(path) => {
                    let expanded = expand_env(&path, getenv);
                    let pb = PathBuf::from(&expanded);
                    let full = if pb.is_absolute() { pb } else { base.join(pb) };
                    let name = p.name.unwrap_or_else(|| p.id.clone());
                    cfg.dicts.push(DictConf { id: p.id.clone(), name, path: full.clone(), readonly: p.readonly, written: Some((path.clone(), full)) });
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
        // セクションの行([dictionary.d1]  ; 説明)も、後ろのコメントを外してから見る
        let head = strip_comment(line).trim();
        if head.starts_with('[') && head.ends_with(']') {
            finish(pending.take(), &mut cfg, &mut warns);
            section = head[1..head.len() - 1].trim().to_string();
            // セクションの名前の大文字・小文字は区別しない([General] [Dictionary.Main])。ID は書いたとおりに覚える
            let lower = section.to_ascii_lowercase();
            if lower == "general" {
                section = "general".into();
            }
            if let Some(id) = lower.starts_with("dictionary").then(|| &section["dictionary".len()..]) {
                let id = id.trim_start_matches('.').to_string();
                if !id.is_empty() && seen_ids.iter().any(|x: &String| x.eq_ignore_ascii_case(&id)) {
                    warns.push(format!("設定: {}行目 辞書の ID「{}」が重なっています(保存すると別の ID に変わります)", n + 1, id));
                }
                seen_ids.push(id.clone());
                let id = if id.is_empty() { format!("辞書{}", cfg.dicts.len() + 1) } else { id };
                pending = Some(PendingDict { id, ..Default::default() });
            } else if section != "general" {
                warns.push(format!("設定: {}行目 未知のセクション [{}](次のセクションまでの行は使いません)", n + 1, section));
                section = "?".into(); // 後ろの行を、一つずつ「セクションの外」と知らせないように
            }
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            warns.push(format!("設定: {}行目 「key = value」の形式ではありません", n + 1));
            continue;
        };
        let quoted = v.trim_start().starts_with('"') && strip_comment(v).len() < v.trim_start().len();
        let (k, v) = (k.trim().to_ascii_lowercase(), if quoted { strip_comment(v) } else { strip_comment(v).trim() });
        if let Some(p) = pending.as_mut() {
            match k.as_str() {
                "name" => p.name = Some(v.to_string()),
                "path" => p.path = Some(v.to_string()),
                "readonly" => match parse_bool(v) {
                    Some(b) => p.readonly = b,
                    None => {
                        // 読めない値は、安全な側(参照のみ)にする
                        p.readonly = true;
                        warns.push(format!("設定: {}行目 readonly の値「{}」を解釈できないため、参照のみで開きます(true / false で指定してください)", n + 1, v));
                    }
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
                    None => warns.push(format!("設定: {}行目 close_to_tray の値「{}」を解釈できないため、false を使います(true / false で指定してください)", n + 1, v)),
                },
                "sort" => match Sort::from_key(v) {
                    Some(x) => cfg.sort = x,
                    None => warns.push(format!("設定: {}行目 sort の値「{}」を解釈できないため、word を使います(word / updated_asc / updated_desc)", n + 1, v)),
                },
                "title" => cfg.title = v.to_string(),
                "shared" => match parse_bool(v) {
                    Some(b) => cfg.shared = b,
                    None => warns.push(format!("設定: {}行目 shared の値「{}」を解釈できないため、false を使います(true / false で指定してください)", n + 1, v)),
                },
                "theme" => match Theme::from_key(v) {
                    Some(t) => cfg.theme = t,
                    None => warns.push(format!("設定: {}行目 theme の値「{}」を解釈できないため、light を使います(light / dark / system)", n + 1, v)),
                },
                "image_background" => match v.to_ascii_lowercase().as_str() {
                    "light" => cfg.image_dark_bg = false,
                    "dark" => cfg.image_dark_bg = true,
                    _ => warns.push(format!("設定: {}行目 image_background の値「{}」を解釈できないため、light を使います(light / dark)", n + 1, v)),
                },
                "timing_log" => {} // 廃止した設定(古い設定ファイルに残っていても無視する)
                _ => warns.push(format!("設定: {}行目 未知のキー {}", n + 1, k)),
            }
        } else if section == "?" {
            // 未知のセクションの中(知らせは、セクションの行で1回だけ)
        } else {
            warns.push(format!("設定: {}行目 セクションの外に設定があります", n + 1));
        }
    }
    finish(pending.take(), &mut cfg, &mut warns);

    if cfg.dicts.is_empty() {
        cfg.dicts = Config::default_for(base).dicts;
    }
    // 同じファイルを2つの辞書として登録している(語が二重に出る。片方への変更が、もう片方にも出る)
    for (i, a) in cfg.dicts.iter().enumerate() {
        if let Some(b) = cfg.dicts[..i].iter().find(|b| b.path.to_string_lossy().to_lowercase() == a.path.to_string_lossy().to_lowercase()) {
            warns.push(format!("設定: 辞書「{}」と「{}」は、同じファイルです({})。片方を消してください", b.name, a.name, a.path.display()));
        }
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
        assert!(!cfg.close_to_tray); // 既定では、閉じるボタンで終了する
        assert!(cfg.title.is_empty() && !cfg.shared); // 既定では、名前なし・個人での運用
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
    fn saving_keeps_how_paths_were_written() {
        let base = Path::new("/base");
        let text = "[General]\nshared = true\n\n[Dictionary.main]\nname = 「A ; B」\npath = %APPDATA%\\MyDict\\a.db\n\n[dictionary.sub]\nname = \"  前後に空白 ; あり \"  ; コメント\npath = sub/b.db\n";
        let (cfg, w) = parse(text, base, &env);
        assert!(w.is_empty(), "{w:?}");
        assert!(cfg.shared); // [General] も general として読む
        assert_eq!(cfg.dicts[0].id, "main");
        assert_eq!(cfg.dicts[0].name, "「A"); // 「 ;」から後ろはコメント
        assert_eq!(cfg.dicts[1].name, "  前後に空白 ; あり ");
        let ini = to_ini(&cfg, base);
        // 書かれていたとおりのパス(環境変数・相対パス)のまま保存する
        assert!(ini.contains("path = %APPDATA%\\MyDict\\a.db\r\n"), "{ini}");
        assert!(ini.contains("path = sub/b.db\r\n"), "{ini}");
        // 「 ;」を含む名前は、囲んで保存し、読み戻しても変わらない
        assert!(ini.contains("name = \"  前後に空白 ; あり \"\r\n"), "{ini}");
        let (again, w) = parse(&ini, base, &env);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(again.dicts, cfg.dicts);
        // パスを変えた辞書は、設定ファイルのフォルダの中なら相対パスで書く
        let mut c2 = cfg.clone();
        c2.dicts[0].path = base.join("x").join("c.db");
        assert!(to_ini(&c2, base).contains(&format!("path = {}\r\n", Path::new("x").join("c.db").display())));
    }

    #[test]
    fn set_one_key_keeps_the_rest() {
        let text = "; 手で書いた説明\r\n[General]\r\nhotkey = none ; 使わない\r\nsort = word\r\n\r\n[dictionary.a]\r\npath = %APPDATA%\\a.db\r\n";
        let out = set_general_key(text, "sort", "updated_desc");
        assert_eq!(out, text.replace("sort = word", "sort = updated_desc"));
        let out = set_general_key("[general]\nhotkey = none\n", "sort", "word");
        assert_eq!(out, "[general]\nsort = word\nhotkey = none\n");
        assert_eq!(set_general_key("[dictionary.a]\npath = a.db\n", "sort", "word"), "[general]\nsort = word\n\n[dictionary.a]\npath = a.db\n");
    }

    #[test]
    fn unsafe_values_fall_back_safely() {
        let base = Path::new("/base");
        let text = "[general]\n[mystery]\na = 1\nb = 2\n[dictionary.a]\npath = a.db\nreadonly = はい\n[dictionary.A]\npath = A.DB\n";
        let (cfg, w) = parse(text, base, &env);
        assert!(cfg.dicts[0].readonly); // 読めない readonly は、参照のみにする
        assert_eq!(w.iter().filter(|x| x.contains("mystery")).count(), 1); // 未知のセクションの中の行は、1つずつ知らせない
        assert!(!w.iter().any(|x| x.contains("セクションの外")), "{w:?}");
        assert!(w.iter().any(|x| x.contains("「はい」")), "{w:?}");
        assert!(w.iter().any(|x| x.contains("重なって")), "{w:?}");
        assert!(w.iter().any(|x| x.contains("同じファイル")), "{w:?}");
    }

    #[test]
    fn to_ini_roundtrip() {
        let base = Path::new("/base");
        let cfg = Config {
            hotkey: parse_hotkey("Ctrl+Shift+F5"),
            close_to_tray: true,
            sort: Sort::UpdatedDesc,
            title: "社内 用語集".into(),
            shared: true,
            theme: Theme::System,
            image_dark_bg: false,
            dicts: vec![
                DictConf { id: "main".into(), name: "メイン".into(), path: base.join("dictionary.db"), readonly: false, written: None },
                DictConf { id: "ref".into(), name: "参照 用".into(), path: PathBuf::from("/other/dir/ref.db"), readonly: true, written: None },
            ],
        };
        let (back, warns) = parse(&to_ini(&cfg, base), base, &env);
        assert!(warns.is_empty(), "{warns:?}");
        assert_eq!(back.hotkey, cfg.hotkey);
        assert!(back.close_to_tray);
        assert_eq!(back.sort, Sort::UpdatedDesc);
        assert_eq!(back.title, "社内 用語集");
        assert!(back.shared);
        assert_eq!(back.theme, Theme::System);
        assert_eq!(back.dicts, cfg.dicts); // ID(main, ref)も変わらない
        // ID が無い・重なる辞書には、空いている番号を付ける
        let mut c2 = cfg.clone();
        c2.dicts[1].id = "MAIN".into();
        c2.dicts.push(DictConf { id: String::new(), name: "新".into(), path: base.join("new.db"), readonly: false, written: None });
        let ini = to_ini(&c2, base);
        assert!(ini.contains("[dictionary.main]") && ini.contains("[dictionary.d2]") && ini.contains("[dictionary.d3]"), "{ini}");
        // ホットキー無し
        let none = Config { hotkey: None, ..cfg };
        assert!(parse(&to_ini(&none, base), base, &env).0.hotkey.is_none());
    }

    #[test]
    fn titles() {
        let mut c = Config::default_for(Path::new("/b"));
        assert_eq!(window_title(&c, "MyDictionary"), "MyDictionary");
        // 設定した名前を、そのまま表示する(アプリの名前や「共用」は足さない)
        c.title = " 社内用語集 ".into();
        assert_eq!(window_title(&c, "MyDictionary"), "社内用語集");
        c.shared = true;
        assert_eq!(window_title(&c, "MyDictionary"), "社内用語集");
        c.title = "営業部の辞書 - MyDictionary".into();
        assert_eq!(window_title(&c, "MyDictionary"), "営業部の辞書 - MyDictionary");
        c.title.clear();
        assert_eq!(window_title(&c, "MyDictionary"), "MyDictionary");
        // 設定ファイルから読める
        let (c, w) = parse("[general]\ntitle = 営業部 用語集\nshared = yes\n", Path::new("/b"), &env);
        assert!(w.is_empty());
        assert_eq!((c.title.as_str(), c.shared), ("営業部 用語集", true));
    }

    #[test]
    fn trailing_comments() {
        // 説明書の例のように、値の後ろに「; 説明」を書ける
        let text = "[general]\nshared = true           ; 共用フォルダで運用する\ntitle = 社内用語集        ; ウィンドウに表示する名前\ntheme = dark\t; 配色\n\
[dictionary.d1]          ; 先頭の辞書\nname = メイン\npath = dictionary.db     ; 相対パス\nreadonly = true          ; 参照のみ\n";
        let (c, w) = parse(text, Path::new("/b"), &env);
        assert!(w.is_empty(), "{w:?}");
        assert!(c.shared);
        assert_eq!(c.title, "社内用語集");
        assert_eq!(c.theme, Theme::Dark);
        assert_eq!(c.dicts[0].path, PathBuf::from("/b/dictionary.db"));
        assert!(c.dicts[0].readonly);
        // 空白の無い ; は値の一部
        let (c, _) = parse("[general]\ntitle = A;B\n", Path::new("/b"), &env);
        assert_eq!(c.title, "A;B");
    }

    #[test]
    fn manual_examples_parse_cleanly() {
        // 説明書(docs/manual.md)の ini の例を、そのまま設定ファイルにしても、警告が出ず、書いたとおりに効くこと
        // Windows で取り出したファイルは、行末が CRLF になっていることがあるので、そろえてから探す
        let manual = include_str!("../docs/manual.md").replace("\r\n", "\n");
        let blocks: Vec<&str> = manual.split("```ini\n").skip(1).map(|b| b.split("```").next().unwrap_or("")).collect();
        assert!(blocks.len() >= 2);
        for b in &blocks {
            let (c, w) = parse(b, Path::new("/b"), &env);
            assert!(w.is_empty(), "{w:?}\n{b}");
            // 値の後ろのコメントが、値に混ざっていない
            assert!(!c.title.contains(';'));
            for d in &c.dicts {
                assert!(!d.name.contains(';') && !d.path.to_string_lossy().contains(';'), "{d:?}");
            }
            if b.contains("shared = true") {
                assert!(c.shared);
            }
            if b.contains("readonly = true") {
                assert!(c.dicts.iter().any(|d| d.readonly));
            }
        }
    }

    #[test]
    fn themes() {
        let base = Path::new("/b");
        // 既定は明るい配色。既定のときは、設定ファイルに書かない(古い版が読んでも、警告が出ないように)
        let c = Config::default_for(base);
        assert_eq!(c.theme, Theme::Light);
        assert!(!to_ini(&c, base).contains("theme"));
        assert_eq!(parse(&default_ini(), base, &env).0.theme, Theme::Light);
        assert!(parse(&default_ini(), base, &env).1.is_empty());
        // 書いた値を読める(大文字・小文字は区別しない)
        for (text, want) in [("dark", Theme::Dark), ("System", Theme::System), (" LIGHT ", Theme::Light)] {
            let (c, w) = parse(&format!("[general]\ntheme = {text}\n"), base, &env);
            assert!(w.is_empty(), "{w:?}");
            assert_eq!(c.theme, want);
        }
        let dark = Config { theme: Theme::Dark, ..c };
        assert!(to_ini(&dark, base).contains("theme = dark\r\n"));
        assert_eq!(parse(&to_ini(&dark, base), base, &env).0.theme, Theme::Dark);
        // 知らない値は、警告を出して既定のまま
        let (c, w) = parse("[general]\ntheme = blue\n", base, &env);
        assert_eq!(c.theme, Theme::Light);
        assert!(w.iter().any(|x| x.contains("theme")));
        assert_eq!((Theme::Dark.key(), Theme::from_key("x")), ("dark", None));
        // 画像の透明な部分の地
        let (c, w) = parse("[general]\nimage_background = dark\n", base, &env);
        assert!(w.is_empty() && c.image_dark_bg);
        assert!(parse(&to_ini(&c, base), base, &env).0.image_dark_bg);
        assert!(!to_ini(&Config::default_for(base), base).contains("image_background"));
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
