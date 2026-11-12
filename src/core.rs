//! GUI に依存しない中核部分: SQLite の読み書きとメモリ上の絞り込み検索。

use rusqlite::{params, Connection, OpenFlags};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct Entry {
    /// 所属する辞書(設定上の添字)
    pub dict: usize,
    pub id: i64,
    pub word: String,
    /// 読み(ひらがな・カタカナ・ローマ字など)。無ければ空
    pub reading: String,
    pub meaning: String,
    /// スペース区切りのタグ
    pub tags: String,
    /// UNIX秒(UTC)
    pub updated_at: i64,
    /// 参照先。1行に1つ。「表示名 | URL」の形式で表示名を付けられる。無ければ空
    pub link: String,
    // 検索用に文字をそろえたもの(`fold`)
    word_lc: String,
    reading_norm: String,
    meaning_lc: String,
    tags_lc: String,
}

impl Entry {
    pub fn new(id: i64, word: String, meaning: String, tags: String, updated_at: i64) -> Self {
        let (word_lc, meaning_lc) = (fold(&word), fold(&meaning));
        // タグは「#a #b」の形で持つ。「#a」で検索するとタグの先頭に一致し、「a」ならタグの途中にも一致する
        let tags_lc = hash_tags(&fold(&tags));
        Entry { dict: 0, id, word, reading: String::new(), meaning, tags, updated_at, link: String::new(), word_lc, reading_norm: String::new(), meaning_lc, tags_lc }
    }

    pub fn with_dict(mut self, dict: usize) -> Self {
        self.dict = dict;
        self
    }

    pub fn with_link(mut self, link: String) -> Self {
        self.link = link;
        self
    }

    pub fn with_reading(mut self, reading: String) -> Self {
        self.reading_norm = fold(&reading);
        self.reading = reading;
        self
    }

    /// 見出し語を、検索用にそろえたもの(同じ見出し語かどうかの判定に使う)
    pub fn word_norm(&self) -> &str {
        &self.word_lc
    }
}

// ---- 文字をそろえる ---------------------------------------------------------
//
// 検索では、次の違いを区別しない。語の側(見出し語・読み・意味・タグ)と検索語の両方を
// `fold` でそろえてから比べる。
// - 英字の大文字と小文字
// - 全角の英数字・記号と半角(全角スペースも)
// - 半角カタカナと全角
// - カタカナとひらがな
// 濁点・半濁点が別の文字になっている表記(半角カタカナの「ｶﾞ」、結合文字)は、前の字と1文字に合成する。
// 長音や小さい字の違い(「サーバ」と「サーバー」)はそろえない。

/// 半角カタカナ(U+FF61〜U+FF9F)に対応する全角の字
const HALF_KANA: [char; 63] = [
    '。', '「', '」', '、', '・', 'ヲ', 'ァ', 'ィ', 'ゥ', 'ェ', 'ォ', 'ャ', 'ュ', 'ョ', 'ッ', 'ー', 'ア', 'イ', 'ウ', 'エ', 'オ', 'カ', 'キ', 'ク', 'ケ', 'コ',
    'サ', 'シ', 'ス', 'セ', 'ソ', 'タ', 'チ', 'ツ', 'テ', 'ト', 'ナ', 'ニ', 'ヌ', 'ネ', 'ノ', 'ハ', 'ヒ', 'フ', 'ヘ', 'ホ', 'マ', 'ミ', 'ム', 'メ', 'モ', 'ヤ',
    'ユ', 'ヨ', 'ラ', 'リ', 'ル', 'レ', 'ロ', 'ワ', 'ン', '゛', '゜',
];

/// 1文字をそろえる(濁点の合成は `fold_step` で行う)
fn fold_char(c: char) -> char {
    let c = match c {
        '\u{3000}' => ' ',
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        '\u{FF61}'..='\u{FF9F}' => HALF_KANA[(c as u32 - 0xFF61) as usize],
        _ => c,
    };
    match c {
        // カタカナ → ひらがな(ァ〜ヶ、ヽ ヾ)
        '\u{30A1}'..='\u{30F6}' | '\u{30FD}' | '\u{30FE}' => char::from_u32(c as u32 - 0x60).unwrap_or(c),
        _ => c.to_lowercase().next().unwrap_or(c),
    }
}

/// 濁点(`semi` なら半濁点)を付けた字。付けられない字なら None
fn voiced(base: char, semi: bool) -> Option<char> {
    let b = base as u32;
    if semi {
        matches!(base, 'は' | 'ひ' | 'ふ' | 'へ' | 'ほ').then(|| char::from_u32(b + 2)).flatten()
    } else {
        match base {
            'か' | 'き' | 'く' | 'け' | 'こ' | 'さ' | 'し' | 'す' | 'せ' | 'そ' | 'た' | 'ち' | 'つ' | 'て' | 'と' | 'は' | 'ひ' | 'ふ' | 'へ' | 'ほ' => {
                char::from_u32(b + 1)
            }
            'う' => Some('ゔ'),
            'ゝ' => Some('ゞ'),
            _ => None,
        }
    }
}

/// そろえた結果の1文字。`Merge` は、直前に出した字と合成して置き換える
enum Folded {
    Push(char),
    Merge(char),
}

/// `prev`(直前に出した字)の次に `c` が来たときの結果
fn fold_step(prev: Option<char>, c: char) -> Folded {
    let semi = matches!(c, '\u{FF9F}' | '\u{309A}' | '\u{309C}');
    if semi || matches!(c, '\u{FF9E}' | '\u{3099}' | '\u{309B}') {
        if let Some(v) = prev.and_then(|p| voiced(p, semi)) {
            return Folded::Merge(v);
        }
    }
    Folded::Push(fold_char(c))
}

/// 検索用に文字をそろえる(上の説明を参照)
pub fn fold(s: &str) -> String {
    if s.is_ascii() {
        return s.to_ascii_lowercase();
    }
    let mut out = String::with_capacity(s.len());
    let mut prev = None;
    for c in s.chars() {
        let f = match fold_step(prev, c) {
            Folded::Push(f) => f,
            Folded::Merge(f) => {
                out.pop();
                f
            }
        };
        out.push(f);
        prev = Some(f);
    }
    out
}

/// `fold` と同じ結果を、1文字ごとに「元の文字列での UTF-16 の範囲 (開始, 終了)」付きで返す。
/// 合成で文字数が変わっても、強調表示の位置を元の文字列の上で求められるようにするため。
fn fold_spans(s: &str) -> Vec<(char, usize, usize)> {
    let mut out: Vec<(char, usize, usize)> = Vec::with_capacity(s.len());
    let mut pos = 0;
    for c in s.chars() {
        let end = pos + c.len_utf16();
        match fold_step(out.last().map(|x| x.0), c) {
            Folded::Push(f) => out.push((f, pos, end)),
            Folded::Merge(f) => {
                if let Some(last) = out.last_mut() {
                    last.0 = f;
                    last.2 = end;
                }
            }
        }
        pos = end;
    }
    out
}

/// 表示・検索用に、各タグの頭に # を付ける("a b" → "#a #b")
pub fn hash_tags(tags: &str) -> String {
    tags.split(' ').filter(|t| !t.is_empty()).map(|t| format!("#{t}")).collect::<Vec<_>>().join(" ")
}

/// 検索条件。全体検索(all)は見出し語・意味・タグのどれかに含まれればよい。
/// word / meaning / tag は列ごとのカスタム検索で、すべてAND。
#[derive(Clone, Debug, Default)]
pub struct Query {
    pub all: String,
    pub word: String,
    pub meaning: String,
    pub tag: String,
    /// 検索に含める辞書(添字ごとの真偽)。None なら全辞書
    pub dicts: Option<Vec<bool>>,
}

pub const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let conn = Connection::open(path)?;
    // 共有フォルダで他の人が書き込み中でも、すぐ失敗せず少し待つ
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS words (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            word       TEXT NOT NULL,
            meaning    TEXT NOT NULL,
            tags       TEXT NOT NULL DEFAULT '',
            updated_at INTEGER NOT NULL,
            link       TEXT NOT NULL DEFAULT '',
            reading    TEXT NOT NULL DEFAULT ''
         );",
    )?;
    Ok(conn)
}

/// words テーブルにその列があるか。古い版で作った辞書には、後から増えた列(link, reading)が無い。
pub fn has_column(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.prepare("SELECT 1 FROM pragma_table_info('words') WHERE name = ?1")?.exists([name])
}

/// 古い版で作った辞書に、後から増えた列を追加する。書き込みの直前に呼ぶ。
/// 列を足すだけなので、古い版のアプリからも引き続き読み書きできる。
/// あわせて、この辞書に書き込んだことのある、いちばん新しいアプリの版を記録する(`stored_version`)。
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    for col in ["link", "reading"] {
        if !has_column(conn, col)? {
            conn.execute_batch(&format!("ALTER TABLE words ADD COLUMN {col} TEXT NOT NULL DEFAULT ''"))?;
        }
    }
    stamp_version(conn, app_version_num())
}

/// このアプリの版を、整数にしたもの(2.03 → 203)。Cargo のパッケージの版 `2.0.3` から作る
pub fn app_version_num() -> i64 {
    version_num(env!("CARGO_PKG_VERSION"))
}

/// `major.minor.patch` → `major * 100 + minor * 10 + patch`(画面に出す版 `2.03` の、小数点を除いた数と同じ)
fn version_num(cargo: &str) -> i64 {
    let mut it = cargo.split('.').map(|p| p.trim().parse::<i64>().unwrap_or(0));
    let (major, minor, patch) = (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0));
    major * 100 + minor * 10 + patch
}

/// 版の整数を、画面に出す形にする(203 → "2.03")
pub fn version_label(n: i64) -> String {
    format!("{}.{:02}", n / 100, n % 100)
}

/// 辞書に記録されている「この辞書に書き込んだことのある、いちばん新しいアプリの版」(`app_version_num` の形)。
/// 記録の無い辞書(この機能より前の版だけが書いた辞書)は 0。
/// SQLite にもとからある整数の欄 `user_version` を使う(表や列は増やさない。古い版のアプリは、この欄を見ないし変えない)。
pub fn stored_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
}

/// 記録されている版より `mine` が新しければ、記録を更新する(古ければ、何もしない)
fn stamp_version(conn: &Connection, mine: i64) -> rusqlite::Result<()> {
    if stored_version(conn)? < mine {
        conn.execute_batch(&format!("PRAGMA user_version = {mine}"))?;
    }
    Ok(())
}

/// 参照専用で開く(ファイルが無ければエラー)
pub fn open_ro(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(conn)
}

/// 全件を読み込む。並べ替えは呼び出し側(sort_entries)で行う。
/// 件数が多いときは、検索用に文字をそろえる処理を複数スレッドで行う。
pub fn load_all(conn: &Connection) -> rusqlite::Result<Vec<Entry>> {
    type Raw = (i64, String, String, String, i64, String, String);
    // 参照専用で開いた古い辞書には link / reading 列が無いことがある(その場合は空として読む)
    let col = |name: &str| -> rusqlite::Result<&'static str> {
        Ok(match (name, has_column(conn, name)?) {
            ("link", true) => "link",
            ("reading", true) => "reading",
            _ => "''",
        })
    };
    let sql = format!("SELECT id, word, meaning, tags, updated_at, {}, {} FROM words", col("link")?, col("reading")?);
    let mut stmt = conn.prepare(&sql)?;
    let raw: Vec<Raw> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let build = |(id, w, m, t, u, l, rd): Raw| Entry::new(id, w, m, t, u).with_link(l).with_reading(rd);
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(4);
    if raw.len() < 20_000 || threads < 2 {
        return Ok(raw.into_iter().map(build).collect());
    }
    let chunk = (raw.len() + threads - 1) / threads;
    let mut parts: Vec<Vec<Raw>> = Vec::new();
    let mut it = raw.into_iter();
    loop {
        let part: Vec<Raw> = it.by_ref().take(chunk).collect();
        if part.is_empty() {
            break;
        }
        parts.push(part);
    }
    let mut out = Vec::new();
    std::thread::scope(|sc| {
        let handles: Vec<_> = parts
            .into_iter()
            .map(|p| sc.spawn(move || p.into_iter().map(build).collect::<Vec<_>>()))
            .collect();
        for h in handles {
            out.extend(h.join().expect("load thread"));
        }
    });
    Ok(out)
}

pub fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// 登録・更新する内容
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fields {
    pub word: String,
    pub reading: String,
    pub meaning: String,
    pub tags: String,
    pub link: String,
}

/// 登録する。`ts` を渡すと更新日時をその値にする(インポートで元の日時を保つため)
pub fn insert_entry(conn: &Connection, f: &Fields, ts: Option<i64>) -> rusqlite::Result<Entry> {
    let ts = ts.unwrap_or_else(now_unix);
    conn.execute(
        "INSERT INTO words (word, meaning, tags, updated_at, link, reading) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![f.word, f.meaning, f.tags, ts, f.link, f.reading],
    )?;
    Ok(Entry::new(conn.last_insert_rowid(), f.word.clone(), f.meaning.clone(), f.tags.clone(), ts).with_link(f.link.clone()).with_reading(f.reading.clone()))
}

/// 読み無しで登録する(簡易版)
#[cfg_attr(not(test), allow(dead_code))]
pub fn insert(conn: &Connection, word: &str, meaning: &str, tags: &str, link: &str) -> rusqlite::Result<Entry> {
    insert_entry(conn, &Fields { word: word.into(), meaning: meaning.into(), tags: tags.into(), link: link.into(), ..Default::default() }, None)
}

/// 既存レコードを更新し、新しい更新時刻(UNIX秒)を返す。
/// `expected_ts`(読み込んだ時点の更新時刻)と一致しない場合は、他の人が先に変更/削除したとみなして
/// `QueryReturnedNoRows` を返す(上書きしない)。
pub fn update(conn: &Connection, id: i64, expected_ts: i64, f: &Fields) -> rusqlite::Result<i64> {
    // 同じ秒のうちに再編集しても更新時刻が必ず変わるようにする
    let ts = now_unix().max(expected_ts + 1);
    let n = conn.execute(
        "UPDATE words SET word = ?1, meaning = ?2, tags = ?3, updated_at = ?4, link = ?7, reading = ?8 WHERE id = ?5 AND updated_at = ?6",
        params![f.word, f.meaning, f.tags, ts, id, expected_ts, f.link, f.reading],
    )?;
    if n == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(ts)
}

/// 複数IDを削除する(トランザクションは呼び出し側で管理)。
/// 画面からの削除は、元に戻せるように `undo::delete_with_snapshots` を使う
#[cfg_attr(not(test), allow(dead_code))]
pub fn delete_ids(conn: &Connection, ids: &[i64]) -> rusqlite::Result<usize> {
    let mut n = 0;
    for id in ids {
        n += conn.execute("DELETE FROM words WHERE id = ?1", params![id])?;
    }
    Ok(n)
}

/// 見出し語順に並べる
pub fn sort_entries(entries: &mut [Entry]) {
    // 並べ替えに使う文字列:
    // - 見出し語が英数字で始まる語は、読みがあっても見出し語そのもの(アルファベット順を優先)
    // - それ以外(日本語)は、読みがあれば読み、無ければ見出し語
    // カタカナはひらがなと同じ扱いにして、読みの無いカタカナ語も五十音の中に並べる。
    // 読みの無い漢字の語は、文字コードの関係でかなの後ろに並ぶ。
    // (word_lc / reading_norm は `fold` 済みなので、カタカナはすでにひらがなになっている)
    fn key(e: &Entry) -> &str {
        let ascii_head = e.word_lc.starts_with(|c: char| c.is_ascii_alphanumeric());
        if ascii_head || e.reading_norm.is_empty() {
            &e.word_lc
        } else {
            &e.reading_norm
        }
    }
    entries.sort_unstable_by(|a, b| key(a).cmp(key(b)).then_with(|| a.word_lc.cmp(&b.word_lc)).then(a.dict.cmp(&b.dict)).then(a.id.cmp(&b.id)));
}

fn split_tags(input: &str) -> impl Iterator<Item = &str> {
    input.split(|c: char| c.is_whitespace() || matches!(c, ',' | '，' | '、')).filter(|t| !t.is_empty())
}

/// 「#」や「*」だけの語は検索語として扱わない(入力途中とみなす)
fn is_term(t: &str) -> bool {
    t.chars().any(|c| c != '#' && c != '*')
}

/// 検索語1つ(`fold` 済み)。`*` を含む語はワイルドカード(`*` は任意の文字列)。
enum Term {
    /// 部分一致
    Plain(String),
    /// `*` で区切った断片と、先頭・末尾が固定かどうか(`app*` は先頭固定、`*ing` は末尾固定)
    Glob { parts: Vec<String>, start: bool, end: bool },
    /// タグが、この名前そのものであること(`#"it"`。タグ欄では `"it"` でもよい)。
    /// `#it` は「it で始まるタグ」なので、`item` などにも一致する。タグの一覧やタグのクリックから検索するときに使う
    TagExact(String),
}

impl Term {
    /// `tag_field` はタグ欄の語(`#` が無くても、`"…"` をタグの完全一致として扱う)
    fn new(t: &str, tag_field: bool) -> Term {
        let (hash, body) = match t.strip_prefix('#') {
            Some(b) => (true, b),
            None => (false, t),
        };
        if (hash || tag_field) && body.starts_with('"') {
            let inner = &body[1..];
            return match inner.strip_suffix('"') {
                Some(name) if !name.is_empty() && !name.contains('"') => Term::TagExact(name.to_owned()),
                // 閉じる前(入力の途中)は、引用符を外した語として扱う
                _ => Term::new(&format!("{}{}", if hash { "#" } else { "" }, inner.trim_end_matches('"')), tag_field),
            };
        }
        if t.contains('*') {
            Term::Glob { parts: t.split('*').filter(|p| !p.is_empty()).map(str::to_owned).collect(), start: !t.starts_with('*'), end: !t.ends_with('*') }
        } else {
            Term::Plain(t.to_owned())
        }
    }

    /// 文字列全体に対して照合する(見出し語・読み・タグ用)。Plain は部分一致
    fn matches_whole(&self, text: &str) -> bool {
        match self {
            Term::Plain(t) => text.contains(t.as_str()),
            Term::Glob { parts, start, end } => glob(text, parts, *start, *end),
            Term::TagExact(_) => false,
        }
    }

    /// 文章の中から探す(意味用)。ワイルドカードは「断片がこの順に現れる」
    fn matches_within(&self, text: &str) -> bool {
        match self {
            Term::Plain(t) => text.contains(t.as_str()),
            Term::Glob { parts, .. } => glob(text, parts, false, false),
            Term::TagExact(_) => false,
        }
    }

    /// タグ(「#a #b」形式)のどれか1つに一致するか
    fn matches_tags(&self, tags_lc: &str) -> bool {
        match self {
            Term::Plain(t) => tags_lc.contains(t.as_str()),
            Term::Glob { parts, start, end } => {
                // 「#」から書かれていれば # 込みで、そうでなければ # を除いたタグ名と照合する
                let with_hash = *start && parts.first().map_or(false, |p| p.starts_with('#'));
                tags_lc.split(' ').any(|tag| glob(if with_hash { tag } else { tag.trim_start_matches('#') }, parts, *start, *end))
            }
            Term::TagExact(name) => tags_lc.split(' ').any(|tag| tag.strip_prefix('#') == Some(name.as_str())),
        }
    }

    /// 見出し語・読み・意味・タグのどれかに一致するか(全体検索用)
    fn matches_any(&self, e: &Entry) -> bool {
        self.matches_whole(&e.word_lc) || self.matches_whole(&e.reading_norm) || self.matches_within(&e.meaning_lc) || self.matches_tags(&e.tags_lc)
    }

    /// 強調表示する断片。ワイルドカードの語は、`*` で区切った断片をそれぞれ強調する
    /// (英数字1文字だけの断片は、あちこちが赤くなって読みにくいので強調しない)
    fn pieces(&self) -> Vec<String> {
        match self {
            Term::Plain(t) => vec![t.clone()],
            Term::TagExact(name) => vec![format!("#{name}")],
            Term::Glob { parts, .. } => {
                parts.iter().filter(|p| is_term(p)).filter(|p| !(p.chars().count() == 1 && p.chars().all(|c| c.is_ascii_alphanumeric()))).cloned().collect()
            }
        }
    }
}

/// `parts` が `text` の中にこの順で(重ならずに)現れるか。start / end が真なら先頭 / 末尾に固定する
fn glob(text: &str, parts: &[String], start: bool, end: bool) -> bool {
    let mut rest = text;
    let last = parts.len().saturating_sub(1);
    for (i, p) in parts.iter().enumerate() {
        let p = p.as_str();
        if i == 0 && start {
            match rest.strip_prefix(p) {
                Some(r) => rest = r,
                None => return false,
            }
            if i == last && end {
                return rest.is_empty();
            }
        } else if i == last && end {
            return rest.ends_with(p);
        } else {
            match rest.find(p) {
                Some(pos) => rest = &rest[pos + p.len()..],
                None => return false,
            }
        }
    }
    true
}

/// 検索条件の1節。選択肢(`alts`)のどれかに一致すればよい。`neg` なら、どれにも一致しないこと。
/// 節どうしは AND。
///
/// | 入力 | 節 |
/// |---|---|
/// | `a b` | [a] と [b] |
/// | `a|b`、`a OR b`、`a | b` | [a か b] |
/// | `-a` | [a でない] |
/// | `-a|b` | [a でも b でもない] |
struct Clause {
    neg: bool,
    alts: Vec<Term>,
}

impl Clause {
    fn hit(&self, f: impl Fn(&Term) -> bool) -> bool {
        self.alts.iter().any(f) != self.neg
    }
}

/// 「OR」は、大文字で書かれたときだけ記号として扱う(小文字の or は普通の検索語)
fn is_or_word(raw: &str) -> bool {
    raw == "OR" || raw == "ＯＲ"
}

/// 入力欄1つぶんを節の列にする。`tag_field` はタグ欄(カンマ・読点でも区切る)。
/// `-` や `|` だけ、`a|` のような入力途中の形は、その部分を無視する。
fn parse_clauses(input: &str, tag_field: bool) -> Vec<Clause> {
    let tokens: Vec<&str> = if tag_field { split_tags(input).collect() } else { input.split_whitespace().collect() };
    let mut out: Vec<Clause> = Vec::new();
    let mut join = false; // 次の語を、前の節の選択肢に加える(直前に OR や | があった)
    for raw in tokens {
        if is_or_word(raw) {
            join = !out.is_empty();
            continue;
        }
        let t = fold(raw);
        let (neg, body) = match t.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, t.as_str()),
        };
        if body.starts_with('|') && !neg && !out.is_empty() {
            join = true;
        }
        let alts: Vec<Term> = body.split('|').filter(|a| is_term(a)).map(|a| Term::new(a, tag_field)).collect();
        if alts.is_empty() {
            continue; // 「-」「|」「#」「*」だけ(入力の途中)
        }
        let ends = body.ends_with('|');
        match out.last_mut() {
            Some(last) if join && !neg && !last.neg => last.alts.extend(alts),
            _ => out.push(Clause { neg, alts }),
        }
        join = ends && !neg;
    }
    out
}

/// 検索条件を、欄ごとの節の列にしたもの
struct Parsed {
    all: Vec<Clause>,
    word: Vec<Clause>,
    meaning: Vec<Clause>,
    tag: Vec<Clause>,
}

impl Parsed {
    fn new(q: &Query) -> Parsed {
        Parsed { all: parse_clauses(&q.all, false), word: parse_clauses(&q.word, false), meaning: parse_clauses(&q.meaning, false), tag: parse_clauses(&q.tag, true) }
    }

    fn is_empty(&self) -> bool {
        self.all.is_empty() && self.word.is_empty() && self.meaning.is_empty() && self.tag.is_empty()
    }
}

/// タグの区切り(空白・全角空白・カンマ・読点)を単一スペースに正規化し、重複を除く
pub fn normalize_tags(input: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for t in split_tags(input) {
        let t = t.trim_start_matches('#'); // 表示時に # を付けるので、入力された # は外す
        if !t.is_empty() && !out.contains(&t) {
            out.push(t);
        }
    }
    out.join(" ")
}

/// 使われているタグと、その語数。書き方の違うタグ(`IT` と `it`)は別々に数える。
/// 並びは、そろえた名前の順(書き方の違うタグが隣り合う)
pub fn tag_counts<'a>(entries: impl Iterator<Item = &'a Entry>) -> Vec<(String, usize)> {
    let mut map: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for e in entries {
        for t in e.tags.split(' ').filter(|t| !t.is_empty()) {
            *map.entry(t).or_default() += 1;
        }
    }
    let mut v: Vec<(String, usize)> = map.into_iter().map(|(t, n)| (t.to_string(), n)).collect();
    v.sort_by(|a, b| fold(&a.0).cmp(&fold(&b.0)).then_with(|| a.0.cmp(&b.0)));
    v
}

/// タグ `tag` の語だけを出すための検索語(`#"tag"`。タグの完全一致)
pub fn tag_query(tag: &str) -> String {
    format!("#\"{}\"", tag.trim().trim_start_matches('#'))
}

/// 見出し語 `word` の語を一覧に出すための、検索欄の文字列。
/// 見出し語をそのまま検索欄に入れると、記法として解釈されてしまう部分を避ける
/// (`-` で始まる語は除外に、`OR` は記号になる)。完全一致の語は、呼び出し側で選ぶ。
pub fn search_text_for(word: &str) -> String {
    word.split_whitespace()
        .map(|t| {
            let t = t.trim_start_matches(['-', '－']);
            if is_or_word(t) {
                t.to_lowercase()
            } else {
                t.to_string()
            }
        })
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// 条件に合うエントリの添字を返す(`fold` でそろえて比べる。部分一致)。
/// 並び: 見出し語の完全一致 > 前方一致 > 見出し語に含む > その他(同順位は元の順)。
pub fn filter(entries: &[Entry], q: &Query) -> Vec<u32> {
    filter_in(entries, q, None)
}

/// 入力欄の内容に、文字を足すと一致範囲が広がることのある記法(ワイルドカード・OR・除外)が含まれるか
fn may_widen(s: &str) -> bool {
    let f = fold(s);
    f.contains('*') || f.contains('|') || split_tags(&f).any(|t| t.starts_with('-')) || s.split_whitespace().any(is_or_word)
}

/// `new` が `prev` の条件を絞り込むだけ(= `prev` のヒットの部分集合になる)か。
/// 入力欄に文字を追加していくときに成り立つ。
pub fn narrows(prev: &Query, new: &Query) -> bool {
    // ワイルドカードは文字を足すと一致範囲が広がることがある(例: 「a*b」→「a*bc」)。
    // OR は選択肢が増え、除外は「-a」→「-ab」で除外が減る。どれも対象外
    if [&new.all, &new.word, &new.meaning, &new.tag].iter().any(|s| may_widen(s)) {
        return false;
    }
    // そろえた後の文字で比べる(「ｶ」に濁点を足した「ｶﾞ」は「が」で、「か」の絞り込みではない)
    let ext = |p: &str, n: &str| fold(n).starts_with(&fold(p));
    prev.dicts == new.dicts
        && ext(&prev.all, &new.all)
        && ext(&prev.word, &new.word)
        && ext(&prev.meaning, &new.meaning)
        && ext(&prev.tag, &new.tag)
}

/// `filter` と同じ。`candidates`(昇順の添字)を渡すと、その中だけを調べる。
pub fn filter_in(entries: &[Entry], q: &Query, candidates: Option<&[u32]>) -> Vec<u32> {
    let p = Parsed::new(q);
    let dict_ok = |e: &Entry| q.dicts.as_ref().map_or(true, |m| m.get(e.dict).copied().unwrap_or(false));
    if p.is_empty() {
        return match candidates {
            Some(c) => c.iter().copied().filter(|&i| dict_ok(&entries[i as usize])).collect(),
            None => (0..entries.len() as u32).filter(|&i| dict_ok(&entries[i as usize])).collect(),
        };
    }

    // 順位付けの基準: 見出し語欄があればその先頭の節、なければ全体検索の先頭の節(除外の節は使わない)
    let first: Option<&Clause> = p.word.iter().find(|c| !c.neg).or_else(|| p.all.iter().find(|c| !c.neg));
    // 完全一致の判定に使う、欄全体の文字列(語の間の空白は1つにそろえる)
    let key_src = if !p.word.is_empty() { &q.word } else { &q.all };
    let key: String = fold(key_src).split_whitespace().collect::<Vec<_>>().join(" ");

    let mut hits: Vec<(u8, u32)> = Vec::new();
    let all_idx: Vec<u32>;
    let cands: &[u32] = match candidates {
        Some(c) => c,
        None => {
            all_idx = (0..entries.len() as u32).collect();
            &all_idx
        }
    };
    for &i in cands {
        let e = &entries[i as usize];
        if !dict_ok(e) {
            continue;
        }
        // 全体検索と見出し語欄は、読みにも当てる
        let ok = p.all.iter().all(|c| c.hit(|t| t.matches_any(e)))
            && p.word.iter().all(|c| c.hit(|t| t.matches_whole(&e.word_lc) || t.matches_whole(&e.reading_norm)))
            && p.meaning.iter().all(|c| c.hit(|t| t.matches_within(&e.meaning_lc)))
            && p.tag.iter().all(|c| c.hit(|t| t.matches_tags(&e.tags_lc)));
        if ok {
            // 見出し語/読みの完全一致 > 前方一致(ワイルドカードは一致) > 含む > その他
            let rd = &e.reading_norm;
            let rank = match first {
                None => 3,
                Some(_) if e.word_lc == key || (!rd.is_empty() && *rd == key) => 0,
                // 選択肢が複数あるときは、いちばん良い順位を使う
                Some(c) => c
                    .alts
                    .iter()
                    .map(|t| match t {
                        Term::Plain(f) => {
                            let f = f.as_str();
                            if c.alts.len() > 1 && (e.word_lc == f || (!rd.is_empty() && rd == f)) {
                                0
                            } else if e.word_lc.starts_with(f) || rd.starts_with(f) {
                                1
                            } else if e.word_lc.contains(f) || rd.contains(f) {
                                2
                            } else {
                                3
                            }
                        }
                        g => {
                            if g.matches_whole(&e.word_lc) || g.matches_whole(rd) {
                                1
                            } else {
                                3
                            }
                        }
                    })
                    .min()
                    .unwrap_or(3),
            };
            hits.push((rank, i));
        }
    }
    hits.sort_by_key(|h| h.0); // 安定ソート
    hits.into_iter().map(|h| h.1).collect()
}

impl Query {
    /// 強調表示する語(`fold` 済み)。(見出し語用, 意味用, タグ用)。除外の語は含めない
    pub fn mark_terms(&self) -> (Vec<String>, Vec<String>, Vec<String>) {
        let p = Parsed::new(self);
        let pieces = |cs: &[Clause]| cs.iter().filter(|c| !c.neg).flat_map(|c| c.alts.iter()).flat_map(|t| t.pieces()).collect::<Vec<_>>();
        let all = pieces(&p.all);
        let mut word = all.clone();
        word.extend(pieces(&p.word));
        let mut meaning = all.clone();
        meaning.extend(pieces(&p.meaning));
        let mut tag = all;
        tag.extend(pieces(&p.tag));
        (word, meaning, tag)
    }
}

/// `text` の中で `terms` に一致する範囲を、UTF-16 単位の (開始, 終了) で返す。
/// 検索と同じく `fold` でそろえて比べる。重なる範囲はまとめる。
pub fn mark_ranges(text: &str, terms: &[String]) -> Vec<(usize, usize)> {
    if terms.is_empty() || text.is_empty() {
        return Vec::new();
    }
    // そろえた各文字と、元の文字列での位置
    let spans = fold_spans(text);
    let chars: Vec<char> = spans.iter().map(|s| s.0).collect();

    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for t in terms {
        let tc: Vec<char> = fold(t).chars().collect();
        if tc.is_empty() || tc.len() > chars.len() {
            continue;
        }
        let mut i = 0;
        while i + tc.len() <= chars.len() {
            if chars[i..i + tc.len()] == tc[..] {
                ranges.push((spans[i].1, spans[i + tc.len() - 1].2));
                i += tc.len();
            } else {
                i += 1;
            }
        }
    }
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for r in ranges {
        match merged.last_mut() {
            Some(last) if r.0 <= last.1 => last.1 = last.1.max(r.1),
            _ => merged.push(r),
        }
    }
    merged
}

/// 別の語へのリンクを表す開く先の接頭辞(例: `word:apple`)
pub const WORD_SCHEME: &str = "word:";

/// 意味の文中のリンクを取り出す。
/// - `[[語]]` は別の語へのリンク(表示は括弧を外した「語」)
/// - `http://` / `https://` で始まる URL
/// 戻り値は (表示用の文字列, [(開始, 終了, 開く先)])。位置は表示用の文字列の UTF-16 単位。
pub fn body_links(text: &str) -> (String, Vec<(usize, usize, String)>) {
    let mut out = String::new();
    let mut links = Vec::new();
    let mut pos16 = 0usize;
    let mut rest = text;
    loop {
        let next_word = rest.find("[[");
        let next_url = [rest.find("http://"), rest.find("https://")].into_iter().flatten().min();
        let (at, is_word) = match (next_word, next_url) {
            (Some(w), Some(u)) => {
                if w < u {
                    (w, true)
                } else {
                    (u, false)
                }
            }
            (Some(w), None) => (w, true),
            (None, Some(u)) => (u, false),
            (None, None) => break,
        };
        let before = &rest[..at];
        out.push_str(before);
        pos16 += before.encode_utf16().count();
        let tail = &rest[at..];
        if is_word {
            // 同じ行の中で閉じている [[語]] だけをリンクにする
            let close = tail[2..].find("]]").filter(|&c| c > 0 && !tail[2..2 + c].contains('\n') && !tail[2..2 + c].trim().is_empty());
            match close {
                Some(c) => {
                    let word = tail[2..2 + c].trim();
                    let n = word.encode_utf16().count();
                    links.push((pos16, pos16 + n, format!("{WORD_SCHEME}{word}")));
                    out.push_str(word);
                    pos16 += n;
                    rest = &tail[2 + c + 2..];
                }
                None => {
                    out.push_str("[[");
                    pos16 += 2;
                    rest = &tail[2..];
                }
            }
        } else {
            // URL は空白や和文の括弧・句読点の手前まで。末尾の句読点は含めない
            let end = tail
                .find(|c: char| c.is_whitespace() || matches!(c, '「' | '」' | '『' | '』' | '（' | '）' | '、' | '。' | '<' | '>' | '"' | '　'))
                .unwrap_or(tail.len());
            let url = tail[..end].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '\'']);
            let n = url.encode_utf16().count();
            if url.len() > "https://".len() {
                links.push((pos16, pos16 + n, url.to_string()));
            }
            out.push_str(url);
            pos16 += n;
            rest = &tail[url.len()..];
        }
    }
    out.push_str(rest);
    (out, links)
}

/// 参照先の欄(1行に1つ)を (表示名, 開く先) の一覧にする。
/// 「表示名 | URL」または「[表示名](URL)」なら表示名付き。それ以外は行全体が表示名かつ開く先。
pub fn parse_links(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| {
            // [[語]] は、この辞書の別の語へのリンク
            if let Some(w) = l.strip_prefix("[[").and_then(|r| r.strip_suffix("]]")) {
                if !w.trim().is_empty() {
                    return (w.trim().to_string(), format!("{WORD_SCHEME}{}", w.trim()));
                }
            }
            if l.starts_with('[') && l.ends_with(')') {
                if let Some((t, u)) = l[1..l.len() - 1].split_once("](") {
                    if !t.trim().is_empty() && !u.trim().is_empty() {
                        return (t.trim().to_string(), u.trim().to_string());
                    }
                }
            }
            if let Some((t, u)) = l.split_once('|') {
                if !t.trim().is_empty() && !u.trim().is_empty() {
                    return (t.trim().to_string(), u.trim().to_string());
                }
            }
            (l.to_string(), l.to_string())
        })
        .collect()
}

/// 参照先の種類。開く前の確認の強さを決める。
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum LinkKind {
    /// この辞書の別の語(`word:語`)。アプリ内で移動する
    Word,
    /// http / https の Web ページ。そのまま開く
    Web,
    /// 実行される可能性のあるファイル(.exe / .bat / .lnk など)。強い警告を出す
    Executable,
    /// それ以外(文書ファイル、フォルダ、その他のスキーム)。開く前に確認する
    Other,
}

pub fn link_kind(link: &str) -> LinkKind {
    let l = link.trim().trim_matches('"').to_lowercase();
    if l.starts_with(WORD_SCHEME) {
        return LinkKind::Word;
    }
    if l.starts_with("http://") || l.starts_with("https://") {
        return LinkKind::Web;
    }
    const EXEC: &[&str] = &[
        "exe", "com", "bat", "cmd", "scr", "pif", "msi", "msp", "ps1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "hta", "lnk", "url",
        "reg", "jar", "cpl", "dll", "appref-ms",
    ];
    // mailto: などファイルではないスキーム(2文字以上。1文字はドライブ名)は、拡張子で判定しない
    if let Some((scheme, _)) = l.split_once(':') {
        let is_scheme = scheme.len() >= 2
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
        if is_scheme && scheme != "file" {
            return LinkKind::Other;
        }
    }
    let name = l.rsplit(['\\', '/']).next().unwrap_or("");
    match name.rsplit_once('.') {
        Some((_, ext)) if EXEC.contains(&ext.trim()) => LinkKind::Executable,
        _ => LinkKind::Other,
    }
}

/// 表示用: UNIX秒(UTC)を日本時間(UTC+9)の "yyyy/MM/dd HH:mm:ss" にする。
pub fn format_jst(unix: i64) -> String {
    let t = unix + 9 * 3600;
    let days = t.div_euclid(86400);
    let sod = t.rem_euclid(86400);
    // civil_from_days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}/{:02}/{:02} {:02}:{:02}:{:02}", y, m, d, sod / 3600, sod % 3600 / 60, sod % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    // サンプルデータ(sample/ フォルダ。アプリの「取り込む…」で取り込める TSV)
    const SAMPLE_TSV: &str = include_str!("../sample/sample.tsv");
    const SAMPLE2_TSV: &str = include_str!("../sample/sample2.tsv");

    fn sample() -> Vec<Entry> {
        vec![
            Entry::new(1, "apple".into(), "りんご".into(), "fruit".into(), 0),
            Entry::new(2, "pineapple".into(), "パイナップル".into(), "fruit".into(), 0),
            Entry::new(3, "Apple Inc".into(), "アップル社".into(), "company".into(), 0),
            Entry::new(4, "辞書".into(), "言葉を集めた本。apple の話はしない".into(), "".into(), 0),
        ]
    }

    fn q(all: &str) -> Query {
        Query { all: all.into(), ..Default::default() }
    }

    #[test]
    fn empty_query_returns_all() {
        assert_eq!(filter(&sample(), &q("")).len(), 4);
        assert_eq!(filter(&sample(), &q("   ")).len(), 4);
    }

    #[test]
    fn partial_match_and_case_insensitive() {
        let e = sample();
        let r = filter(&e, &q("APP"));
        assert_eq!(r.len(), 4); // 辞書は意味に apple を含む
        // 見出し語の前方一致(apple, Apple Inc)が先、pineapple、意味のみの辞書が最後
        assert_eq!(r, vec![0, 2, 1, 3]);
    }

    #[test]
    fn exact_word_ranks_first() {
        let e = sample();
        assert_eq!(filter(&e, &q("apple"))[0], 0);
    }

    #[test]
    fn and_search_and_tags() {
        let e = sample();
        assert_eq!(filter(&e, &q("apple company")), vec![2]);
        assert_eq!(filter(&e, &q("fruit")), vec![0, 1]);
        assert_eq!(filter(&e, &q("存在しない")), Vec::<u32>::new());
    }

    #[test]
    fn custom_column_search() {
        let e = sample();
        // 見出し語だけ: 意味にだけ apple を含む「辞書」は出ない
        let r = filter(&e, &Query { word: "apple".into(), ..Default::default() });
        assert_eq!(r, vec![0, 2, 1]);
        // 意味だけ
        let r = filter(&e, &Query { meaning: "りんご".into(), ..Default::default() });
        assert_eq!(r, vec![0]);
        // タグだけ
        let r = filter(&e, &Query { tag: "company".into(), ..Default::default() });
        assert_eq!(r, vec![2]);
        // 複数タグはAND(区切りはスペース/カンマ/読点)
        let mut e2 = sample();
        e2[0] = Entry::new(1, "apple".into(), "りんご".into(), "fruit red".into(), 0);
        let r = filter(&e2, &Query { tag: "fruit、red".into(), ..Default::default() });
        assert_eq!(r, vec![0]);
        let r = filter(&e2, &Query { tag: "fruit blue".into(), ..Default::default() });
        assert!(r.is_empty());
        // 全体 + 列指定の併用はAND
        let r = filter(&e, &Query { all: "fruit".into(), word: "pine".into(), ..Default::default() });
        assert_eq!(r, vec![1]);
    }

    #[test]
    fn narrowing_matches_full_filter() {
        let e = sample();
        let steps = ["a", "ap", "app", "appl", "apple", "apple ", "apple c", "apple co"];
        let mut prev = Query::default();
        let mut prev_hits: Vec<u32> = (0..e.len() as u32).collect();
        for s in steps {
            let cur = q(s);
            assert!(narrows(&prev, &cur), "{s}");
            let mut cand = prev_hits.clone();
            cand.sort_unstable();
            let fast = filter_in(&e, &cur, Some(&cand));
            assert_eq!(fast, filter(&e, &cur), "{s}");
            prev = cur;
            prev_hits = fast;
        }
        assert!(!narrows(&q("apple"), &q("appl"))); // 削除は絞り込みではない
        assert!(!narrows(&q("apple"), &q("apricot")));
        assert!(!narrows(&Query { dicts: Some(vec![true]), ..Default::default() }, &Query::default()));
        assert!(narrows(&q("a"), &Query { all: "a".into(), tag: "x".into(), ..Default::default() }));
    }

    #[test]
    fn dict_filter() {
        let mut e = sample();
        e[1] = e[1].clone().with_dict(1);
        e[2] = e[2].clone().with_dict(2);
        let r = filter(&e, &Query { dicts: Some(vec![false, true, false]), ..Default::default() });
        assert_eq!(r, vec![1]);
        // 複数の辞書を選ぶ
        let r = filter(&e, &Query { dicts: Some(vec![false, true, true]), ..Default::default() });
        assert_eq!(r, vec![1, 2]);
        let r = filter(&e, &Query { all: "apple".into(), dicts: Some(vec![true, false, false]), ..Default::default() });
        assert_eq!(r, vec![0, 3]);
    }

    #[test]
    fn marks() {
        let t = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(mark_ranges("Apple pie, apple", &t(&["app"])), vec![(0, 3), (11, 14)]);
        // 重なり・隣接はまとめる
        assert_eq!(mark_ranges("abcd", &t(&["abc", "bcd"])), vec![(0, 4)]);
        // 日本語、サロゲートペア(😀 は UTF-16 で2単位)
        assert_eq!(mark_ranges("😀辞書の辞", &t(&["辞書"])), vec![(2, 4)]);
        assert!(mark_ranges("abc", &t(&[])).is_empty());
        assert!(mark_ranges("abc", &t(&["zzz", ""])).is_empty());
        let q = Query { all: "A b".into(), word: "W".into(), tag: "t1,t2".into(), ..Default::default() };
        let (w, m, tg) = q.mark_terms();
        assert_eq!(w, t(&["a", "b", "w"]));
        assert_eq!(m, t(&["a", "b"]));
        assert_eq!(tg, t(&["a", "b", "t1", "t2"]));
    }

    #[test]
    fn hash_search() {
        let e = vec![
            Entry::new(1, "apple".into(), "りんご".into(), "fruit red".into(), 0),
            Entry::new(2, "grapefruit".into(), "柑橘".into(), "citrus".into(), 0),
            Entry::new(3, "memo".into(), "#fruit と書いたメモ".into(), "".into(), 0),
        ];
        // # 付きはタグの先頭に一致(grapefruit の見出し語には一致しない)。本文に #fruit と書いてあれば一致する
        assert_eq!(filter(&e, &q("#fruit")), vec![0, 2]);
        assert_eq!(filter(&e, &q("#fr")), vec![0, 2]);
        assert_eq!(filter(&e, &q("#ed")), Vec::<u32>::new()); // タグの途中には一致しない
        assert_eq!(filter(&e, &q("ed")), vec![0]); // # 無しなら途中にも一致
        // # だけなら検索しない(全件)
        assert_eq!(filter(&e, &q("#")).len(), 3);
        assert_eq!(filter(&e, &q("apple #")), vec![0]);
        // タグ欄でも同じ
        assert_eq!(filter(&e, &Query { tag: "#cit".into(), ..Default::default() }), vec![1]);
        assert_eq!(filter(&e, &Query { tag: "#".into(), ..Default::default() }).len(), 3);
        // 入力途中の「#」→「#f」は絞り込みとして扱える
        assert!(narrows(&q("#"), &q("#f")));
        // 強調表示: 表示上の「#fruit #red」の中の位置
        let (_, _, tg) = q("#fr").mark_terms();
        assert_eq!(mark_ranges(&hash_tags("fruit red"), &tg), vec![(0, 3)]);
        assert!(q("#").mark_terms().2.is_empty());
    }

    #[test]
    fn reading_search_and_order() {
        let mk = |id, w: &str, r: &str, m: &str| Entry::new(id, w.into(), m.into(), "".into(), 0).with_reading(r.into());
        let mut e = vec![
            mk(1, "林檎", "リンゴ", "果物"),
            mk(2, "一期一会", "いちごいちえ", "一生に一度の出会い"),
            mk(3, "苺", "いちご", "赤い果物"),
            mk(4, "apple", "", "りんごのこと"),
            mk(5, "SQL", "エスキューエル", "問い合わせ言語"),
        ];
        // 見出し語順は、読みがあれば読みの順(かなの種類は区別しない)
        sort_entries(&mut e);
        assert_eq!(e.iter().map(|x| x.id).collect::<Vec<_>>(), vec![4, 5, 3, 2, 1]); // apple, SQL(英字は読みより見出し語), いちご, いちごいちえ, りんご
        let ids = |v: Vec<u32>| v.into_iter().map(|i| e[i as usize].id).collect::<Vec<_>>();
        // 読みで引ける。ひらがなでもカタカナでも同じ
        assert_eq!(ids(filter(&e, &q("りんご"))), vec![1, 4]); // 読みの完全一致が先、意味に含む apple が後
        assert_eq!(ids(filter(&e, &q("リンゴ"))), vec![1, 4]); // 意味の「りんご」にも、カタカナで一致する
        assert_eq!(ids(filter(&e, &q("いちご"))), vec![3, 2]); // 完全一致 > 前方一致
        assert_eq!(ids(filter(&e, &q("えすきゅー"))), vec![5]);
        // 見出し語欄(絞り込み)も読みに当たる。ワイルドカードも使える
        assert_eq!(ids(filter(&e, &Query { word: "いち*え".into(), ..Default::default() })), vec![2]);
        assert_eq!(ids(filter(&e, &Query { word: "りん".into(), ..Default::default() })), vec![1]);
        // 意味欄は読みを見ない
        assert!(filter(&e, &Query { meaning: "いちごいちえ".into(), ..Default::default() }).is_empty());
        assert_eq!(fold("リンゴ ヴァ ABC 漢字"), "りんご ゔぁ abc 漢字");

        // 並び全体: 数字 → 英字(読みがあっても見出し語で並べる)→ かな(読み、または読みの無いかな・カタカナ語)→ 読みの無い漢字
        let mut v = vec![
            mk(1, "辞書", "", ""),
            mk(2, "アジェンダ", "", ""),
            mk(3, "稟議", "りんぎ", ""),
            mk(4, "zebra", "", ""),
            mk(5, "API", "えーぴーあい", ""),
            mk(6, "5W1H", "", ""),
            mk(7, "いろは", "", ""),
            mk(8, "Apple", "", ""),
        ];
        sort_entries(&mut v);
        let words: Vec<&str> = v.iter().map(|x| x.word.as_str()).collect();
        assert_eq!(words, vec!["5W1H", "API", "Apple", "zebra", "アジェンダ", "いろは", "稟議", "辞書"]);
    }

    #[test]
    fn fold_rules() {
        // 大文字・小文字、全角・半角(英数字・記号・スペース)
        assert_eq!(fold("ＡＰＩ　Ｖ２＃＊｜－"), "api v2#*|-");
        // 半角カタカナ → 全角 → ひらがな。濁点・半濁点は前の字と合成する
        assert_eq!(fold("ｻｰﾊﾞ ﾊﾟｿｺﾝ ｳﾞｧ"), "さーば ぱそこん ゔぁ");
        assert_eq!(fold("ｶﾞｷﾞｸﾞｹﾞｺﾞ ﾊﾟﾋﾟﾌﾟﾍﾟﾎﾟ ﾂﾞ"), "がぎぐげご ぱぴぷぺぽ づ");
        // 濁点が結合文字(か + U+3099)になっている表記も合成する
        assert_eq!(fold("か\u{3099}き\u{3099} は\u{309A}"), "がぎ ぱ");
        // 合成できない位置の濁点は、そのまま残す
        assert_eq!(fold("ﾞa ｱﾞ"), "゛a あ゛");
        // カタカナ → ひらがな(長音はそのまま)
        assert_eq!(fold("サーバー ヽヾ"), "さーばー ゝゞ");
        // 長音や小さい字の違いはそろえない
        assert_ne!(fold("サーバ"), fold("サーバー"));
        assert_eq!(fold("Straße Ünïcode"), "straße ünïcode");
    }

    #[test]
    fn width_and_kana_insensitive_search() {
        let e = vec![
            Entry::new(1, "ＡＰＩ".into(), "外部から機能を呼び出す窓口".into(), "ＩＴ".into(), 0),
            Entry::new(2, "サーバ".into(), "ｻｰﾋﾞｽを提供するコンピュータ".into(), "it".into(), 0),
            Entry::new(3, "りんご".into(), "赤い果物".into(), "果物".into(), 0),
            Entry::new(4, "ｶﾞｲﾄﾞ".into(), "案内。ガイドブック".into(), "".into(), 0),
        ];
        // 全角で登録した語を半角で、半角で登録した語を全角で引ける
        assert_eq!(filter(&e, &q("api")), vec![0]);
        assert_eq!(filter(&e, &q("ＡＰＩ")), vec![0]);
        assert_eq!(filter(&e, &q("ｻｰﾊﾞ")), vec![1]);
        assert_eq!(filter(&e, &q("さーば")), vec![1]);
        assert_eq!(filter(&e, &q("サービス")), vec![1]); // 意味の半角カタカナ
        assert_eq!(filter(&e, &q("リンゴ")), vec![2]); // 見出し語のひらがな
        assert_eq!(filter(&e, &q("ガイド")), vec![3]);
        assert_eq!(filter(&e, &q("がいど")), vec![3]);
        assert_eq!(filter(&e, &q("カイド")), Vec::<u32>::new()); // 濁点の有無は区別する
        // タグと記号(全角の # と *)
        assert_eq!(filter(&e, &q("＃ｉｔ")), vec![0, 1]);
        assert_eq!(filter(&e, &Query { tag: "IT".into(), ..Default::default() }), vec![0, 1]);
        assert_eq!(filter(&e, &Query { word: "ａｐ＊".into(), ..Default::default() }), vec![0]);
        // 全角スペースも語の区切り
        assert_eq!(filter(&e, &q("ｻｰﾊﾞ　コンピュータ")), vec![1]);
        // 順位: そろえた後で完全一致する語が先
        let e2 = vec![
            Entry::new(1, "アップルパイ".into(), "".into(), "".into(), 0),
            Entry::new(2, "ｱｯﾌﾟﾙ".into(), "".into(), "".into(), 0),
        ];
        assert_eq!(filter(&e2, &q("あっぷる")), vec![1, 0]);

        // 強調表示の位置は、元の文字列の上で求める。「ｶﾞ」(2文字)は「が」1文字ぶんとして扱う
        let t = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(mark_ranges("ｶﾞｲﾄﾞ", &t(&["がい"])), vec![(0, 3)]);
        assert_eq!(mark_ranges("xｶﾞｲﾄﾞ", &t(&["いど"])), vec![(3, 6)]);
        assert_eq!(mark_ranges("ＡＰＩ api", &t(&["api"])), vec![(0, 3), (4, 7)]);
        assert_eq!(mark_ranges("りんごとリンゴ", &t(&["りんご"])), vec![(0, 3), (4, 7)]);
        let (w, _, _) = q("ＡＰＩ ｶﾞｲﾄﾞ").mark_terms();
        assert_eq!(w, t(&["api", "がいど"]));

        // 絞り込みの再利用は、そろえた後の文字で判定する
        assert!(narrows(&q("ａｐ"), &q("api")));
        assert!(narrows(&q("ｶ"), &q("ｶｲ")));
        assert!(!narrows(&q("ｶ"), &q("ｶﾞ"))); // 「か」に濁点が付いて「が」になった。「か」の結果の部分集合ではない
        assert_eq!(filter_in(&e, &q("ｶﾞ"), None), vec![3]);

        // 並び順: 全角の英数字は英数字として、半角カタカナはかなとして並ぶ
        let mut v = vec![
            Entry::new(1, "ｻｰﾊﾞ".into(), "".into(), "".into(), 0),
            Entry::new(2, "ＡＰＩ".into(), "".into(), "".into(), 0),
            Entry::new(3, "かぎ".into(), "".into(), "".into(), 0),
            Entry::new(4, "zip".into(), "".into(), "".into(), 0),
        ];
        sort_entries(&mut v);
        assert_eq!(v.iter().map(|x| x.id).collect::<Vec<_>>(), vec![2, 4, 3, 1]);
    }

    #[test]
    fn or_and_exclude_search() {
        let e = vec![
            Entry::new(1, "apple".into(), "りんご。赤い果物".into(), "fruit red".into(), 0),
            Entry::new(2, "banana".into(), "バナナ。黄色い果物".into(), "fruit yellow".into(), 0),
            Entry::new(3, "carrot".into(), "にんじん。赤い野菜".into(), "vegetable".into(), 0),
            Entry::new(4, "orange".into(), "みかんの仲間。果物".into(), "fruit".into(), 0),
        ];
        // OR: | でつなぐ、または大文字の OR
        assert_eq!(filter(&e, &q("apple|banana")), vec![0, 1]);
        assert_eq!(filter(&e, &q("apple OR banana")), vec![0, 1]);
        assert_eq!(filter(&e, &q("apple | banana")), vec![0, 1]);
        assert_eq!(filter(&e, &q("apple |banana")), vec![0, 1]);
        assert_eq!(filter(&e, &q("apple| banana")), vec![0, 1]);
        assert_eq!(filter(&e, &q("apple｜banana")), vec![0, 1]); // 全角の ｜
        assert_eq!(filter(&e, &q("apple ＯＲ banana OR carrot")), vec![0, 1, 2]);
        // 小文字の or は普通の検索語(orange の見出し語に含まれる)
        assert_eq!(filter(&e, &q("or")), vec![3]);
        assert_eq!(filter(&e, &q("apple or")), Vec::<u32>::new());
        // OR と AND の組み合わせ: (apple か carrot) かつ 赤い
        assert_eq!(filter(&e, &q("apple|carrot|banana 赤い")), vec![0, 2]);
        // 除外
        assert_eq!(filter(&e, &q("果物 -赤い")), vec![1, 3]);
        assert_eq!(filter(&e, &q("-果物")), vec![2]); // 除外だけでもよい
        assert_eq!(filter(&e, &q("果物 －赤い")), vec![1, 3]); // 全角の －
        assert_eq!(filter(&e, &q("-#fruit")), vec![2]); // タグの除外
        assert_eq!(filter(&e, &q("#fruit -#red -#yellow")), vec![3]);
        assert_eq!(filter(&e, &q("-a*")), vec![1, 2, 3]); // ワイルドカードの除外(見出し語が a で始まる語と、意味に a… を含む語)
        assert_eq!(filter(&e, &q("-apple|banana")), vec![2, 3]); // どちらでもない
        // 入力の途中の形は、その部分を無視する
        assert_eq!(filter(&e, &q("apple -")), vec![0]);
        assert_eq!(filter(&e, &q("apple |")), vec![0]);
        assert_eq!(filter(&e, &q("apple|")), vec![0]);
        assert_eq!(filter(&e, &q("apple OR")), vec![0]);
        assert_eq!(filter(&e, &q("| apple")), vec![0]);
        assert_eq!(filter(&e, &q("OR apple")), vec![0]);
        assert_eq!(filter(&e, &q("-")).len(), 4);
        assert_eq!(filter(&e, &q("|")).len(), 4);
        assert_eq!(filter(&e, &q("-#")).len(), 4);
        // 見出し語から検索欄の文字列を作る(記法として解釈されないようにする)
        for w in ["-fno-rtti", "OR", "a OR b", "e-mail", "－全角", "x|y"] {
            let v = vec![Entry::new(1, w.into(), "意味".into(), "".into(), 0), Entry::new(2, "ほかの語".into(), "意味".into(), "".into(), 0)];
            let hits = filter(&v, &q(&search_text_for(w)));
            assert!(hits.contains(&0), "{w} → {}", search_text_for(w));
        }
        assert_eq!(search_text_for("-fno-rtti"), "fno-rtti");
        assert_eq!(search_text_for("a OR b"), "a or b");
        // 語の途中の - は、ただの文字
        let h = vec![Entry::new(1, "e-mail".into(), "電子メール".into(), "".into(), 0), Entry::new(2, "-fno-rtti".into(), "コンパイラの指定".into(), "".into(), 0)];
        assert_eq!(filter(&h, &q("e-mail")), vec![0]);
        assert_eq!(filter(&h, &q("fno-rtti")), vec![1]); // - で始まる語は、- を外して引く
        // 絞り込みの各欄でも同じ
        assert_eq!(filter(&e, &Query { word: "apple|orange".into(), ..Default::default() }), vec![0, 3]);
        assert_eq!(filter(&e, &Query { meaning: "果物 -バナナ".into(), ..Default::default() }), vec![0, 3]);
        assert_eq!(filter(&e, &Query { tag: "fruit,-red".into(), ..Default::default() }), vec![1, 3]);
        assert_eq!(filter(&e, &Query { tag: "red|yellow".into(), ..Default::default() }), vec![0, 1]);
        // 順位: 選択肢のどれかに完全一致する語が先。除外の語は順位に使わない
        let r = vec![
            Entry::new(1, "pineapple".into(), "".into(), "".into(), 0),
            Entry::new(2, "banana".into(), "".into(), "".into(), 0),
            Entry::new(3, "apple".into(), "".into(), "".into(), 0),
        ];
        assert_eq!(filter(&r, &q("apple|banana")), vec![1, 2, 0]);
        assert_eq!(filter(&r, &q("-banana apple")), vec![2, 0]);
        // 強調表示: 選択肢はすべて、除外の語は対象外
        let (w, m, _) = q("apple|banana -carrot").mark_terms();
        assert_eq!(w, vec!["apple".to_string(), "banana".to_string()]);
        assert_eq!(m, w);
        // 絞り込みの再利用はしない(結果が広がることがあるため)
        assert!(!narrows(&q("果物 -赤"), &q("果物 -赤い"))); // 除外が減って、結果が増える
        assert!(!narrows(&q("apple"), &q("apple|")));
        assert!(!narrows(&q("apple|b"), &q("apple|ba")));
        assert!(!narrows(&q("apple O"), &q("apple OR"))); // 「o」を含む語だけ → apple すべて
        assert!(narrows(&q("apple OR"), &q("apple ORa"))); // OR が普通の語に変わると、絞り込みになる
        assert!(!narrows(&q("a"), &Query { all: "a".into(), tag: "x,-y".into(), ..Default::default() }));
        for (a, b) in [("果物 -赤", "果物 -赤い"), ("apple O", "apple OR"), ("apple OR", "apple ORa"), ("apple", "apple|")] {
            // narrows が真のときは、前の結果の中だけを調べても全体を調べたのと同じになる
            if narrows(&q(a), &q(b)) {
                let mut cand = filter(&e, &q(a));
                cand.sort_unstable();
                assert_eq!(filter_in(&e, &q(b), Some(&cand)), filter(&e, &q(b)), "{a} → {b}");
            }
        }
    }

    #[test]
    fn exact_tag_search_and_tag_list() {
        let e = vec![
            Entry::new(1, "a".into(), "x".into(), "IT 略語".into(), 0),
            Entry::new(2, "b".into(), "x".into(), "item".into(), 0),
            Entry::new(3, "c".into(), "x".into(), "it".into(), 0),
            Entry::new(4, "d".into(), "#\"it\" と書いた意味".into(), "".into(), 0),
        ];
        // #it は「it で始まるタグ」。#"it" は、タグが it そのもの(大文字・小文字などは区別しない)
        assert_eq!(filter(&e, &q("#it")), vec![0, 1, 2]);
        assert_eq!(filter(&e, &q("#\"it\"")), vec![0, 2]); // 意味に同じ文字があっても、タグだけを見る
        assert_eq!(filter(&e, &q("＃＂ＩＴ＂")), vec![0, 2]); // 全角でもよい
        assert_eq!(filter(&e, &q(&tag_query("IT"))), vec![0, 2]);
        assert_eq!(tag_query("#略語"), "#\"略語\"");
        assert_eq!(filter(&e, &q("#\"it\" #\"略語\"")), vec![0]);
        assert_eq!(filter(&e, &q("-#\"it\"")), vec![1, 3]);
        assert_eq!(filter(&e, &q("#\"it\"|#\"item\"")), vec![0, 1, 2]);
        // タグ欄では、# が無くてもよい
        assert_eq!(filter(&e, &Query { tag: "\"it\"".into(), ..Default::default() }), vec![0, 2]);
        assert_eq!(filter(&e, &Query { tag: "#\"it\" \"略語\"".into(), ..Default::default() }), vec![0]);
        assert_eq!(filter(&e, &Query { tag: "it".into(), ..Default::default() }), vec![0, 1, 2]); // 引用符なしは、これまでどおり部分一致
        // 入力の途中(閉じる前)は、引用符の無い語と同じ。1文字ずつ足しても、全件を調べた場合と同じ結果になる
        assert_eq!(filter(&e, &q("#\"it")), vec![0, 1, 2]);
        assert_eq!(filter(&e, &q("#\"")).len(), 4);
        let steps = ["#", "#\"", "#\"i", "#\"it", "#\"it\"", "#\"it\" ", "#\"it\" #\"略"];
        let mut prev = Query::default();
        let mut hits: Vec<u32> = (0..e.len() as u32).collect();
        for s in steps {
            let cur = q(s);
            if narrows(&prev, &cur) {
                let mut cand = hits.clone();
                cand.sort_unstable();
                assert_eq!(filter_in(&e, &cur, Some(&cand)), filter(&e, &cur), "{s}");
            }
            hits = filter(&e, &cur);
            prev = cur;
        }
        // 強調表示: タグの表示(#IT #略語)の中の、# を含む範囲
        let (_, _, tg) = q("#\"it\"").mark_terms();
        assert_eq!(mark_ranges(&hash_tags("IT 略語"), &tg), vec![(0, 3)]);

        // タグの一覧: 書き方の違うタグは別々に数え、隣り合うように並べる
        assert_eq!(tag_counts(e.iter()), vec![("IT".to_string(), 1), ("it".to_string(), 1), ("item".to_string(), 1), ("略語".to_string(), 1)]);
        assert!(tag_counts(std::iter::empty()).is_empty());
    }

    #[test]
    fn version_stamp() {
        assert_eq!(version_num("2.0.3"), 203);
        assert_eq!(version_num("2.1.0"), 210);
        assert_eq!(version_num("3.0"), 300);
        assert_eq!(version_label(203), "2.03");
        assert_eq!(version_label(210), "2.10");

        let dir = std::env::temp_dir().join(format!("MyDictionary-ver-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("d.db");
        let c = open(&path).unwrap();
        assert_eq!(stored_version(&c).unwrap(), 0); // 記録の無い辞書
        // 書き込みの前の移行で、自分の版が記録される
        migrate(&c).unwrap();
        assert_eq!(stored_version(&c).unwrap(), app_version_num());
        // 新しい版が書いた辞書を、古い版が書いても、記録は下がらない
        stamp_version(&c, 999).unwrap();
        migrate(&c).unwrap();
        assert_eq!(stored_version(&c).unwrap(), 999);
        stamp_version(&c, 150).unwrap();
        assert_eq!(stored_version(&c).unwrap(), 999);
        drop(c);
        // 参照専用で開いても読める
        assert_eq!(stored_version(&open_ro(&path).unwrap()).unwrap(), 999);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn wildcard_search() {
        let e = vec![
            Entry::new(1, "apple".into(), "りんご。赤い果物".into(), "fruit red".into(), 0),
            Entry::new(2, "pineapple".into(), "パイナップル。熱帯の果物".into(), "fruit".into(), 0),
            Entry::new(3, "Apple Inc".into(), "アップル社".into(), "company it".into(), 0),
            Entry::new(4, "running".into(), "走ること。apple は関係ない".into(), "verb".into(), 0),
        ];
        let w = |s: &str| filter(&e, &Query { word: s.into(), ..Default::default() });
        assert_eq!(w("app*"), vec![0, 2]); // app で始まる
        assert_eq!(w("*apple"), vec![0, 1]); // apple で終わる
        assert_eq!(w("*ing"), vec![3]);
        assert_eq!(w("a*e"), vec![0]); // a で始まり e で終わる
        assert_eq!(w("a*c"), vec![2]);
        assert_eq!(w("*pp*"), vec![0, 1, 2]); // 両側 * は部分一致と同じ
        assert_eq!(w("p*a*e"), vec![1]);
        assert_eq!(w("apple*"), vec![0, 2]); // * は空文字にも一致
        assert_eq!(w("a*a"), Vec::<u32>::new()); // 断片は重ならない
        // 意味: 断片がこの順に現れる
        let m = |s: &str| filter(&e, &Query { meaning: s.into(), ..Default::default() });
        assert_eq!(m("りんご*果物"), vec![0]);
        assert_eq!(m("果物*りんご"), Vec::<u32>::new());
        assert_eq!(m("*果物"), vec![0, 1]);
        // タグ: タグ1つずつに照合。# 付きでも同じ
        let t = |s: &str| filter(&e, &Query { tag: s.into(), ..Default::default() });
        assert_eq!(t("f*"), vec![0, 1]);
        assert_eq!(t("#f*"), vec![0, 1]);
        assert_eq!(t("*d"), vec![0]); // red(fruit red を1つの文字列としては見ない)
        assert_eq!(t("f*d"), Vec::<u32>::new());
        // 全体検索: 見出し語・意味・タグのどれかに一致。見出し語に一致したものが先
        assert_eq!(filter(&e, &q("app*")), vec![0, 2, 3]); // running は意味に「apple は…」を含む
        assert_eq!(filter(&e, &q("#i*")), vec![2]);
        assert_eq!(filter(&e, &q("app* fruit")), vec![0]); // 他の語との AND
        // * だけなら検索しない
        assert_eq!(filter(&e, &q("*")).len(), 4);
        assert_eq!(filter(&e, &q("**")).len(), 4);
        // 絞り込みの再利用はしない(結果が広がることがあるため)
        assert!(!narrows(&q("a*b"), &q("a*bc")));
        assert!(!narrows(&q("ap"), &q("ap*")));
        // 強調は断片ごと
        let (wm, _, tg) = Query { all: "a*e".into(), tag: "#f*t".into(), ..Default::default() }.mark_terms();
        assert!(wm.is_empty()); // 英数字1文字の断片は強調しない
        assert_eq!(tg, vec!["#f".to_string()]);
        let (wm, _, _) = q("app*ng 果*物 a").mark_terms();
        assert_eq!(wm, vec!["app".to_string(), "ng".to_string(), "果".to_string(), "物".to_string(), "a".to_string()]);
    }

    #[test]
    fn links() {
        let v = parse_links("https://a.example/\n\n 公式サイト | https://b.example/ \n[仕様書](C:\\docs\\spec.pdf)\n| x\n");
        assert_eq!(
            v,
            vec![
                ("https://a.example/".to_string(), "https://a.example/".to_string()),
                ("公式サイト".to_string(), "https://b.example/".to_string()),
                ("仕様書".to_string(), "C:\\docs\\spec.pdf".to_string()),
                ("| x".to_string(), "| x".to_string()),
            ]
        );
        assert!(parse_links("  \n").is_empty());
    }

    #[test]
    fn word_and_body_links() {
        assert_eq!(link_kind("word:apple"), LinkKind::Word);
        assert_eq!(parse_links("[[りんご]]\n類義語 | word:apple"), vec![
            ("りんご".to_string(), "word:りんご".to_string()),
            ("類義語".to_string(), "word:apple".to_string()),
        ]);
        let (disp, links) = body_links("😀[[apple]] の一種。詳しくは https://example.com/a?b=1。別名[[ pine ]]、[[未完");
        assert_eq!(disp, "😀apple の一種。詳しくは https://example.com/a?b=1。別名pine、[[未完");
        assert_eq!(
            links,
            vec![
                (2, 7, "word:apple".to_string()), // 😀 は UTF-16 で2単位
                (17, 42, "https://example.com/a?b=1".to_string()),
                (45, 49, "word:pine".to_string()),
            ]
        );
        // リンクが無ければそのまま。末尾の句読点や閉じ括弧は URL に含めない
        assert_eq!(body_links("plain [text]"), ("plain [text]".to_string(), vec![]));
        let (d, l) = body_links("(see http://a.example/x).");
        assert_eq!(d, "(see http://a.example/x).");
        assert_eq!(l, vec![(5, 23, "http://a.example/x".to_string())]);
        // 空の [[ ]] や、行をまたぐものはリンクにしない
        assert_eq!(body_links("[[]] [[a\nb]]").1, vec![]);
    }

    #[test]
    fn link_kinds() {
        assert_eq!(link_kind("https://example.com/a.exe"), LinkKind::Web); // Web 上のファイルはブラウザが扱う
        assert_eq!(link_kind(" HTTP://example.com "), LinkKind::Web);
        assert_eq!(link_kind("C:\\docs\\spec.pdf"), LinkKind::Other);
        assert_eq!(link_kind("\\\\server\\share\\folder"), LinkKind::Other);
        assert_eq!(link_kind("mailto:a@example.com"), LinkKind::Other);
        assert_eq!(link_kind("\\\\server\\share\\tool.EXE"), LinkKind::Executable);
        assert_eq!(link_kind("\"C:\\x\\run.bat\""), LinkKind::Executable);
        assert_eq!(link_kind("file:///C:/x/a.lnk"), LinkKind::Executable);
        assert_eq!(link_kind("C:\\my.folder\\readme"), LinkKind::Other);
    }

    #[test]
    fn old_schema_without_link_column() {
        let dir = std::env::temp_dir().join(format!("MyDictionary-old-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("old.db");
        let _ = std::fs::remove_file(&path);
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE words (id INTEGER PRIMARY KEY AUTOINCREMENT, word TEXT NOT NULL, meaning TEXT NOT NULL,
                 tags TEXT NOT NULL DEFAULT '', updated_at INTEGER NOT NULL);
                 INSERT INTO words (word, meaning, tags, updated_at) VALUES ('old', 'entry', '', 1);",
            )
            .unwrap();
        }
        // 参照専用でも読める(link は空)
        let ro = open_ro(&path).unwrap();
        assert!(!has_column(&ro, "link").unwrap() && !has_column(&ro, "reading").unwrap());
        let v = load_all(&ro).unwrap();
        assert_eq!((v[0].word.as_str(), v[0].link.as_str()), ("old", ""));
        drop(ro);
        // 書き込み前の移行で列が増え、既存データは残る
        let c = open(&path).unwrap();
        migrate(&c).unwrap();
        migrate(&c).unwrap(); // 2回呼んでも問題ない
        insert(&c, "new", "entry", "", "https://example.com/").unwrap();
        let mut v = load_all(&c).unwrap();
        sort_entries(&mut v);
        assert_eq!(v.iter().map(|e| e.link.as_str()).collect::<Vec<_>>(), vec!["https://example.com/", ""]);
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sample_data() {
        let (rows, skipped) = crate::import::to_rows(crate::import::parse_table(SAMPLE_TSV, '\t'));
        assert_eq!((rows.len(), skipped), (300, 0));
        assert!(rows.iter().all(|r| !r.word.is_empty() && !r.meaning.is_empty() && !r.tags.is_empty()));
        // 四字熟語とことわざには、すべて読みが付いている
        assert!(rows.iter().filter(|r| r.tags.contains("四字熟語") || r.tags.contains("ことわざ")).all(|r| !r.reading.is_empty()));
        assert!(rows.iter().filter(|r| !r.reading.is_empty()).count() >= 150);
        assert!(rows.iter().filter(|r| !r.link.is_empty()).all(|r| link_kind(&r.link) == LinkKind::Web));
        let mut words: Vec<&str> = rows.iter().map(|r| r.word.as_str()).collect();
        words.sort_unstable();
        words.dedup();
        assert_eq!(words.len(), 300); // 見出し語の重複なし

        // 取り込みの手順で辞書に入れられる。取り込み直しても増えない
        let dir = std::env::temp_dir().join(format!("MyDictionary-sample-{}", std::process::id()));
        let path = dir.join("sample.db");
        let c = open(&path).unwrap();
        migrate(&c).unwrap();
        assert_eq!(crate::import::insert_rows(&c, &rows).unwrap(), (300, 0));
        assert_eq!(crate::import::insert_rows(&c, &rows).unwrap(), (0, 300));
        assert_eq!(load_all(&c).unwrap().len(), 300);
        drop(c);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sample2_long_texts() {
        let (rows, skipped) = crate::import::to_rows(crate::import::parse_table(SAMPLE2_TSV, '\t'));
        assert_eq!((rows.len(), skipped), (20, 0));
        // すべて読み付きで、複数段落の長文
        assert!(rows.iter().all(|r| !r.reading.is_empty() && r.meaning.chars().count() >= 300 && r.meaning.contains("\n\n")));
        // 文中と参照先の [[語]] は、サンプル1 か サンプル2 のどちらかに実在する語を指している
        let (s1, _) = crate::import::to_rows(crate::import::parse_table(SAMPLE_TSV, '\t'));
        let known: std::collections::HashSet<&str> = s1.iter().chain(rows.iter()).map(|r| r.word.as_str()).collect();
        for r in &rows {
            let targets = body_links(&r.meaning).1.into_iter().map(|l| l.2).chain(parse_links(&r.link).into_iter().map(|l| l.1));
            for t in targets {
                if let Some(w) = t.strip_prefix(WORD_SCHEME) {
                    assert!(known.contains(w), "{} → [[{}]] が見つからない", r.word, w);
                }
            }
        }
    }

    #[test]
    fn jst_format() {
        assert_eq!(format_jst(0), "1970/01/01 09:00:00");
        assert_eq!(format_jst(1_700_000_000), "2023/11/15 07:13:20");
    }

    #[test]
    fn tags_normalize_and_sort() {
        assert_eq!(normalize_tags("  a　b\tb  c "), "a b c");
        assert_eq!(normalize_tags("x,y、z，x"), "x y z");
        assert_eq!(normalize_tags("#a ##b # c"), "a b c"); // 入力された # は外す
        assert_eq!(normalize_tags(""), "");
        let mut e = vec![
            Entry::new(2, "b".into(), "".into(), "".into(), 5),
            Entry::new(1, "B".into(), "".into(), "".into(), 9),
            Entry::new(3, "a".into(), "".into(), "".into(), 1),
        ];
        sort_entries(&mut e);
        assert_eq!(e.iter().map(|x| x.id).collect::<Vec<_>>(), vec![3, 1, 2]);
    }

    #[test]
    fn db_roundtrip() {
        let dir = std::env::temp_dir().join(format!("MyDictionary-test-{}", std::process::id()));
        let path = dir.join("d.db");
        let conn = open(&path).unwrap();
        let a = insert(&conn, "b", "second", "t", "https://example.com/").unwrap();
        insert(&conn, "A", "first", "", "").unwrap();
        let mut all = load_all(&conn).unwrap();
        sort_entries(&mut all);
        assert_eq!(all.iter().map(|e| e.word.as_str()).collect::<Vec<_>>(), vec!["A", "b"]);
        let f = |w: &str, m: &str, t: &str, l: &str, r: &str| Fields { word: w.into(), meaning: m.into(), tags: t.into(), link: l.into(), reading: r.into() };
        let ts = update(&conn, a.id, a.updated_at, &f("b2", "second!", "t1 t2", "C:\\doc\\a.pdf", "ビーツー")).unwrap();
        assert!(ts > a.updated_at);
        assert!(update(&conn, 9999, 0, &f("x", "y", "", "", "")).is_err());
        // 他の人が先に変更していた(更新時刻が違う)場合は上書きしない
        assert!(update(&conn, a.id, a.updated_at, &f("stale", "stale", "", "", "")).is_err());
        let all = load_all(&conn).unwrap();
        let b = all.iter().find(|e| e.id == a.id).unwrap();
        assert_eq!((b.word.as_str(), b.meaning.as_str(), b.tags.as_str()), ("b2", "second!", "t1 t2"));
        assert_eq!(b.link, "C:\\doc\\a.pdf");
        assert_eq!(b.reading, "ビーツー");
        assert_eq!(delete_ids(&conn, &[a.id, 12345]).unwrap(), 1);
        assert_eq!(load_all(&conn).unwrap().len(), 1);
        let ro = open_ro(&path).unwrap();
        assert_eq!(load_all(&ro).unwrap().len(), 1);
        assert!(ro.execute("DELETE FROM words", []).is_err()); // 参照専用は書けない
        assert!(open_ro(&dir.join("none.db")).is_err());
        drop(conn);
        drop(ro);
        let _ = std::fs::remove_dir_all(dir);
    }
}
