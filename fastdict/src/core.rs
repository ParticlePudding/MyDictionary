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
    pub meaning: String,
    /// スペース区切りのタグ
    pub tags: String,
    /// UNIX秒(UTC)
    pub updated_at: i64,
    // 検索用の小文字化済み文字列
    word_lc: String,
    meaning_lc: String,
    tags_lc: String,
}

impl Entry {
    pub fn new(id: i64, word: String, meaning: String, tags: String, updated_at: i64) -> Self {
        let (word_lc, meaning_lc, tags_lc) = (word.to_lowercase(), meaning.to_lowercase(), tags.to_lowercase());
        Entry { dict: 0, id, word, meaning, tags, updated_at, word_lc, meaning_lc, tags_lc }
    }

    pub fn with_dict(mut self, dict: usize) -> Self {
        self.dict = dict;
        self
    }
}

/// 検索条件。全体検索(all)は見出し語・意味・タグのどれかに含まれればよい。
/// word / meaning / tag は列ごとのカスタム検索で、すべてAND。
#[derive(Clone, Debug, Default)]
pub struct Query {
    pub all: String,
    pub word: String,
    pub meaning: String,
    pub tag: String,
    /// None なら全辞書
    pub dict: Option<usize>,
}

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS words (
            id         INTEGER PRIMARY KEY AUTOINCREMENT,
            word       TEXT NOT NULL,
            meaning    TEXT NOT NULL,
            tags       TEXT NOT NULL DEFAULT '',
            updated_at INTEGER NOT NULL
         );",
    )?;
    Ok(conn)
}

/// 参照専用で開く(ファイルが無ければエラー)
pub fn open_ro(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
}

/// 全件を読み込む。並べ替えは呼び出し側(sort_entries)で行う。
/// 件数が多いときは、検索用の小文字化を複数スレッドで行う。
pub fn load_all(conn: &Connection) -> rusqlite::Result<Vec<Entry>> {
    type Raw = (i64, String, String, String, i64);
    let mut stmt = conn.prepare("SELECT id, word, meaning, tags, updated_at FROM words")?;
    let raw: Vec<Raw> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let build = |(id, w, m, t, u): Raw| Entry::new(id, w, m, t, u);
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
            .map(|p| sc.spawn(move || p.into_iter().map(|r| Entry::new(r.0, r.1, r.2, r.3, r.4)).collect::<Vec<_>>()))
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

pub fn insert(conn: &Connection, word: &str, meaning: &str, tags: &str) -> rusqlite::Result<Entry> {
    let ts = now_unix();
    conn.execute(
        "INSERT INTO words (word, meaning, tags, updated_at) VALUES (?1, ?2, ?3, ?4)",
        params![word, meaning, tags, ts],
    )?;
    Ok(Entry::new(conn.last_insert_rowid(), word.into(), meaning.into(), tags.into(), ts))
}

/// 既存レコードを更新し、更新時刻(UNIX秒)を返す
pub fn update(conn: &Connection, id: i64, word: &str, meaning: &str, tags: &str) -> rusqlite::Result<i64> {
    let ts = now_unix();
    let n = conn.execute(
        "UPDATE words SET word = ?1, meaning = ?2, tags = ?3, updated_at = ?4 WHERE id = ?5",
        params![word, meaning, tags, ts, id],
    )?;
    if n == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(ts)
}

/// 複数IDを1トランザクションで削除する(途中で失敗したら何も消さない)
pub fn delete_many(conn: &mut Connection, ids: &[i64]) -> rusqlite::Result<usize> {
    let tx = conn.transaction()?;
    let mut n = 0;
    for id in ids {
        n += tx.execute("DELETE FROM words WHERE id = ?1", params![id])?;
    }
    tx.commit()?;
    Ok(n)
}

/// 見出し語(大文字小文字無視)→ id 順に並べる(load_all と同じ順序)
pub fn sort_entries(entries: &mut [Entry]) {
    entries.sort_unstable_by(|a, b| a.word_lc.cmp(&b.word_lc).then(a.dict.cmp(&b.dict)).then(a.id.cmp(&b.id)));
}

/// 最終更新(UNIX秒)。空なら None
pub fn last_update(entries: &[Entry]) -> Option<i64> {
    entries.iter().map(|e| e.updated_at).max()
}

fn split_tags(input: &str) -> impl Iterator<Item = &str> {
    input.split(|c: char| c.is_whitespace() || matches!(c, ',' | '，' | '、')).filter(|t| !t.is_empty())
}

/// タグの区切り(空白・全角空白・カンマ・読点)を単一スペースに正規化し、重複を除く
pub fn normalize_tags(input: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for t in split_tags(input) {
        if !out.contains(&t) {
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
    let ext = |p: &str, n: &str| n.to_lowercase().starts_with(&p.to_lowercase());
    prev.dict == new.dict
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
    let all_t: Vec<&str> = all_l.split_whitespace().collect();
    let word_t: Vec<&str> = word_l.split_whitespace().collect();
    let meaning_t: Vec<&str> = meaning_l.split_whitespace().collect();
    let tag_t: Vec<&str> = split_tags(&tag_l).collect();

    let no_terms = all_t.is_empty() && word_t.is_empty() && meaning_t.is_empty() && tag_t.is_empty();
    let dict_ok = |e: &Entry| q.dict.map_or(true, |d| e.dict == d);
    if no_terms {
        return match candidates {
            Some(c) => c.iter().copied().filter(|&i| dict_ok(&entries[i as usize])).collect(),
            None => (0..entries.len() as u32).filter(|&i| dict_ok(&entries[i as usize])).collect(),
        };
    }

    // 順位付けの基準: 見出し語欄があればそれ、なければ全体検索
    let (key, first) = if !word_t.is_empty() {
        (word_t.join(" "), word_t[0])
    } else if !all_t.is_empty() {
        (all_t.join(" "), all_t[0])
    } else {
        (String::new(), "")
    };

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
        let ok = all_t.iter().all(|t| e.word_lc.contains(t) || e.meaning_lc.contains(t) || e.tags_lc.contains(t))
            && word_t.iter().all(|t| e.word_lc.contains(t))
            && meaning_t.iter().all(|t| e.meaning_lc.contains(t))
            && tag_t.iter().all(|t| e.tags_lc.contains(t));
        if ok {
            let rank = if first.is_empty() {
                3
            } else if e.word_lc == key {
                0
            } else if e.word_lc.starts_with(first) {
                1
            } else if e.word_lc.contains(first) {
                2
            } else {
                3
            };
            hits.push((rank, i));
        }
    }
    hits.sort_by_key(|h| h.0); // 安定ソート
    hits.into_iter().map(|h| h.1).collect()
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
        assert!(!narrows(&Query { dict: Some(0), ..Default::default() }, &Query::default()));
        assert!(narrows(&q("a"), &Query { all: "a".into(), tag: "x".into(), ..Default::default() }));
    }

    #[test]
    fn dict_filter() {
        let mut e = sample();
        e[1] = e[1].clone().with_dict(1);
        e[2] = e[2].clone().with_dict(1);
        let r = filter(&e, &Query { dict: Some(1), ..Default::default() });
        assert_eq!(r, vec![1, 2]);
        let r = filter(&e, &Query { all: "apple".into(), dict: Some(0), ..Default::default() });
        assert_eq!(r, vec![0, 3]);
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
        assert_eq!(normalize_tags(""), "");
        let mut e = vec![
            Entry::new(2, "b".into(), "".into(), "".into(), 5),
            Entry::new(1, "B".into(), "".into(), "".into(), 9),
            Entry::new(3, "a".into(), "".into(), "".into(), 1),
        ];
        sort_entries(&mut e);
        assert_eq!(e.iter().map(|x| x.id).collect::<Vec<_>>(), vec![3, 1, 2]);
        assert_eq!(last_update(&e), Some(9));
        assert_eq!(last_update(&[]), None);
    }

    #[test]
    fn db_roundtrip() {
        let dir = std::env::temp_dir().join(format!("fastdict-test-{}", std::process::id()));
        let path = dir.join("d.db");
        let conn = open(&path).unwrap();
        let a = insert(&conn, "b", "second", "t").unwrap();
        insert(&conn, "A", "first", "").unwrap();
        let mut all = load_all(&conn).unwrap();
        sort_entries(&mut all);
        assert_eq!(all.iter().map(|e| e.word.as_str()).collect::<Vec<_>>(), vec!["A", "b"]);
        let ts = update(&conn, a.id, "b2", "second!", "t1 t2").unwrap();
        assert!(ts > 0);
        assert!(update(&conn, 9999, "x", "y", "").is_err());
        let all = load_all(&conn).unwrap();
        let b = all.iter().find(|e| e.id == a.id).unwrap();
        assert_eq!((b.word.as_str(), b.meaning.as_str(), b.tags.as_str()), ("b2", "second!", "t1 t2"));
        let mut conn = conn;
        assert_eq!(delete_many(&mut conn, &[a.id, 12345]).unwrap(), 1);
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
