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
    // 検索用の小文字化済み文字列
    word_lc: String,
    /// 読みを小文字化し、カタカナをひらがなにそろえたもの
    reading_norm: String,
    meaning_lc: String,
    tags_lc: String,
}

impl Entry {
    pub fn new(id: i64, word: String, meaning: String, tags: String, updated_at: i64) -> Self {
        let (word_lc, meaning_lc) = (word.to_lowercase(), meaning.to_lowercase());
        // タグは「#a #b」の形で持つ。「#a」で検索するとタグの先頭に一致し、「a」ならタグの途中にも一致する
        let tags_lc = hash_tags(&tags.to_lowercase());
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
        self.reading_norm = to_hiragana(&reading.to_lowercase());
        self.reading = reading;
        self
    }
}

/// カタカナをひらがなにそろえる(読みの検索で、かなの種類を区別しないため)。文字数は変わらない
pub fn to_hiragana(s: &str) -> String {
    s.chars().map(|c| if ('\u{30A1}'..='\u{30F6}').contains(&c) { char::from_u32(c as u32 - 0x60).unwrap_or(c) } else { c }).collect()
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
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    for col in ["link", "reading"] {
        if !has_column(conn, col)? {
            conn.execute_batch(&format!("ALTER TABLE words ADD COLUMN {col} TEXT NOT NULL DEFAULT ''"))?;
        }
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
/// 件数が多いときは、検索用の小文字化を複数スレッドで行う。
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

/// 複数IDを削除する(トランザクションは呼び出し側で管理)
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
    fn key(e: &Entry) -> impl Iterator<Item = char> + '_ {
        let ascii_head = e.word_lc.starts_with(|c: char| c.is_ascii_alphanumeric());
        let k = if ascii_head || e.reading_norm.is_empty() { &e.word_lc } else { &e.reading_norm };
        k.chars().map(|c| if ('\u{30A1}'..='\u{30F6}').contains(&c) { char::from_u32(c as u32 - 0x60).unwrap_or(c) } else { c })
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

/// 検索語1つ。`*` を含む語はワイルドカード(`*` は任意の文字列)。
enum Term<'a> {
    /// 部分一致
    Plain(&'a str),
    /// `*` で区切った断片と、先頭・末尾が固定かどうか(`app*` は先頭固定、`*ing` は末尾固定)
    Glob { parts: Vec<&'a str>, start: bool, end: bool },
}

impl<'a> Term<'a> {
    fn new(t: &'a str) -> Term<'a> {
        if t.contains('*') {
            Term::Glob { parts: t.split('*').filter(|p| !p.is_empty()).collect(), start: !t.starts_with('*'), end: !t.ends_with('*') }
        } else {
            Term::Plain(t)
        }
    }

    /// 文字列全体に対して照合する(見出し語・タグ用)。Plain は部分一致
    fn matches_whole(&self, text: &str) -> bool {
        match self {
            Term::Plain(t) => text.contains(t),
            Term::Glob { parts, start, end } => glob(text, parts, *start, *end),
        }
    }

    /// 文章の中から探す(意味用)。ワイルドカードは「断片がこの順に現れる」
    fn matches_within(&self, text: &str) -> bool {
        match self {
            Term::Plain(t) => text.contains(t),
            Term::Glob { parts, .. } => glob(text, parts, false, false),
        }
    }

    /// タグ(「#a #b」形式)のどれか1つに一致するか
    fn matches_tags(&self, tags_lc: &str) -> bool {
        match self {
            Term::Plain(t) => tags_lc.contains(t),
            Term::Glob { parts, start, end } => {
                // 「#」から書かれていれば # 込みで、そうでなければ # を除いたタグ名と照合する
                let with_hash = *start && parts.first().map_or(false, |p| p.starts_with('#'));
                tags_lc.split(' ').any(|tag| glob(if with_hash { tag } else { tag.trim_start_matches('#') }, parts, *start, *end))
            }
        }
    }

    fn matches_any(&self, e: &Entry) -> bool {
        self.matches_whole(&e.word_lc) || self.matches_within(&e.meaning_lc) || self.matches_tags(&e.tags_lc)
    }
}

/// `parts` が `text` の中にこの順で(重ならずに)現れるか。start / end が真なら先頭 / 末尾に固定する
fn glob(text: &str, parts: &[&str], start: bool, end: bool) -> bool {
    let mut rest = text;
    let last = parts.len().saturating_sub(1);
    for (i, p) in parts.iter().enumerate() {
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

/// 条件に合うエントリの添字を返す(大文字小文字無視・部分一致)。
/// 並び: 見出し語の完全一致 > 前方一致 > 見出し語に含む > その他(同順位は元の順)。
pub fn filter(entries: &[Entry], q: &Query) -> Vec<u32> {
    filter_in(entries, q, None)
}

/// `new` が `prev` の条件を絞り込むだけ(= `prev` のヒットの部分集合になる)か。
/// 入力欄に文字を追加していくときに成り立つ。
pub fn narrows(prev: &Query, new: &Query) -> bool {
    // ワイルドカードは文字を足すと一致範囲が広がることがある(例: 「a*b」→「a*bc」)ので対象外
    if [&new.all, &new.word, &new.meaning, &new.tag].iter().any(|s| s.contains('*')) {
        return false;
    }
    let ext = |p: &str, n: &str| n.to_lowercase().starts_with(&p.to_lowercase());
    prev.dicts == new.dicts
        && ext(&prev.all, &new.all)
        && ext(&prev.word, &new.word)
        && ext(&prev.meaning, &new.meaning)
        && ext(&prev.tag, &new.tag)
}

/// `filter` と同じ。`candidates`(昇順の添字)を渡すと、その中だけを調べる。
pub fn filter_in(entries: &[Entry], q: &Query, candidates: Option<&[u32]>) -> Vec<u32> {
    let all_l = q.all.to_lowercase();
    let word_l = q.word.to_lowercase();
    let meaning_l = q.meaning.to_lowercase();
    let tag_l = q.tag.to_lowercase();
    let all_t: Vec<Term> = all_l.split_whitespace().filter(|t| is_term(t)).map(Term::new).collect();
    let word_t: Vec<Term> = word_l.split_whitespace().filter(|t| is_term(t)).map(Term::new).collect();
    let meaning_t: Vec<Term> = meaning_l.split_whitespace().filter(|t| is_term(t)).map(Term::new).collect();
    let tag_t: Vec<Term> = split_tags(&tag_l).filter(|t| is_term(t)).map(Term::new).collect();

    let no_terms = all_t.is_empty() && word_t.is_empty() && meaning_t.is_empty() && tag_t.is_empty();
    let dict_ok = |e: &Entry| q.dicts.as_ref().map_or(true, |m| m.get(e.dict).copied().unwrap_or(false));
    if no_terms {
        return match candidates {
            Some(c) => c.iter().copied().filter(|&i| dict_ok(&entries[i as usize])).collect(),
            None => (0..entries.len() as u32).filter(|&i| dict_ok(&entries[i as usize])).collect(),
        };
    }

    // 順位付けの基準: 見出し語欄があればその先頭の語、なければ全体検索の先頭の語
    // 読みとの照合用: 検索語のカタカナをひらがなにそろえたもの(all_t / word_t と同じ並び)
    let (all_h, word_h) = (to_hiragana(&all_l), to_hiragana(&word_l));
    let all_r: Vec<Term> = all_h.split_whitespace().filter(|t| is_term(t)).map(Term::new).collect();
    let word_r: Vec<Term> = word_h.split_whitespace().filter(|t| is_term(t)).map(Term::new).collect();
    let first: Option<&Term> = word_t.first().or(all_t.first());
    let first_r: Option<&Term> = word_r.first().or(all_r.first());
    let key: String = if !word_t.is_empty() { word_l.split_whitespace().collect::<Vec<_>>().join(" ") } else { all_l.split_whitespace().collect::<Vec<_>>().join(" ") };

    let key_h = to_hiragana(&key);

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
        let ok = all_t.iter().zip(&all_r).all(|(t, r)| t.matches_any(e) || r.matches_whole(&e.reading_norm))
            && word_t.iter().zip(&word_r).all(|(t, r)| t.matches_whole(&e.word_lc) || r.matches_whole(&e.reading_norm))
            && meaning_t.iter().all(|t| t.matches_within(&e.meaning_lc))
            && tag_t.iter().all(|t| t.matches_tags(&e.tags_lc));
        if ok {
            // 見出し語/読みの完全一致 > 前方一致(ワイルドカードは一致) > 含む > その他
            let rank = match (first, first_r) {
                (Some(Term::Plain(f)), Some(Term::Plain(fr))) => {
                    let rd = &e.reading_norm;
                    if e.word_lc == key || (!rd.is_empty() && *rd == key_h) {
                        0
                    } else if e.word_lc.starts_with(f) || rd.starts_with(fr) {
                        1
                    } else if e.word_lc.contains(f) || rd.contains(fr) {
                        2
                    } else {
                        3
                    }
                }
                (Some(g), Some(gr)) => {
                    if g.matches_whole(&e.word_lc) || gr.matches_whole(&e.reading_norm) {
                        1
                    } else {
                        3
                    }
                }
                _ => 3,
            };
            hits.push((rank, i));
        }
    }
    hits.sort_by_key(|h| h.0); // 安定ソート
    hits.into_iter().map(|h| h.1).collect()
}

impl Query {
    /// 強調表示する語(小文字)。(見出し語用, 意味用, タグ用)
    pub fn mark_terms(&self) -> (Vec<String>, Vec<String>, Vec<String>) {
        // ワイルドカードの語は、`*` で区切った断片をそれぞれ強調する
        // (英数字1文字だけの断片は、あちこちが赤くなって読みにくいので強調しない)
        let pieces = |t: &str| {
            let glob = t.contains('*');
            t.split('*')
                .filter(|p| is_term(p))
                .filter(|p| !(glob && p.chars().count() == 1 && p.chars().all(|c| c.is_ascii_alphanumeric())))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        let ws = |s: &str| s.to_lowercase().split_whitespace().filter(|t| is_term(t)).flat_map(pieces).collect::<Vec<_>>();
        let all = ws(&self.all);
        let mut word = all.clone();
        word.extend(ws(&self.word));
        let mut meaning = all.clone();
        meaning.extend(ws(&self.meaning));
        let mut tag = all;
        tag.extend(split_tags(&self.tag.to_lowercase()).filter(|t| is_term(t)).flat_map(pieces));
        (word, meaning, tag)
    }
}

/// `text` の中で `terms`(小文字)に一致する範囲を、UTF-16 単位の (開始, 終了) で返す。
/// 大文字小文字は無視。重なる範囲はまとめる。
pub fn mark_ranges(text: &str, terms: &[String]) -> Vec<(usize, usize)> {
    if terms.is_empty() || text.is_empty() {
        return Vec::new();
    }
    let low = |c: char| c.to_lowercase().next().unwrap_or(c);
    let chars: Vec<char> = text.chars().map(low).collect();
    // 各文字の UTF-16 上の開始位置
    let mut pos16 = Vec::with_capacity(chars.len() + 1);
    let mut p = 0;
    for c in text.chars() {
        pos16.push(p);
        p += c.len_utf16();
    }
    pos16.push(p);

    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for t in terms {
        let tc: Vec<char> = t.chars().map(low).collect();
        if tc.is_empty() || tc.len() > chars.len() {
            continue;
        }
        let mut i = 0;
        while i + tc.len() <= chars.len() {
            if chars[i..i + tc.len()] == tc[..] {
                ranges.push((pos16[i], pos16[i + tc.len()]));
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
        assert_eq!(ids(filter(&e, &q("リンゴ"))), vec![1]); // 意味の「りんご」は表記どおりなので一致しない
        assert_eq!(ids(filter(&e, &q("いちご"))), vec![3, 2]); // 完全一致 > 前方一致
        assert_eq!(ids(filter(&e, &q("えすきゅー"))), vec![5]);
        // 見出し語欄(絞り込み)も読みに当たる。ワイルドカードも使える
        assert_eq!(ids(filter(&e, &Query { word: "いち*え".into(), ..Default::default() })), vec![2]);
        assert_eq!(ids(filter(&e, &Query { word: "りん".into(), ..Default::default() })), vec![1]);
        // 意味欄は読みを見ない
        assert!(filter(&e, &Query { meaning: "いちごいちえ".into(), ..Default::default() }).is_empty());
        assert_eq!(to_hiragana("リンゴ ヴァ ABC 漢字"), "りんご ゔぁ ABC 漢字");

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
