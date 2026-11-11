//! GUI に依存しない中核部分: SQLite の読み書きとメモリ上の絞り込み検索。

use rusqlite::{params, Connection};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: i64,
    pub word: String,
    pub meaning: String,
    /// スペース区切りのタグ
    pub tags: String,
    /// UNIX秒(UTC)
    pub updated_at: i64,
    /// 検索用の小文字化済み文字列(word / meaning / tags)
    word_lc: String,
    haystack: String,
}

impl Entry {
    pub fn new(id: i64, word: String, meaning: String, tags: String, updated_at: i64) -> Self {
        let word_lc = word.to_lowercase();
        let haystack = format!("{}\n{}\n{}", word_lc, meaning.to_lowercase(), tags.to_lowercase());
        Entry { id, word, meaning, tags, updated_at, word_lc, haystack }
    }
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

pub fn load_all(conn: &Connection) -> rusqlite::Result<Vec<Entry>> {
    let mut stmt =
        conn.prepare("SELECT id, word, meaning, tags, updated_at FROM words ORDER BY word COLLATE NOCASE, id")?;
    let rows = stmt.query_map([], |r| {
        Ok(Entry::new(r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })?;
    rows.collect()
}

pub fn now_unix() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[allow(dead_code)]
pub fn insert(conn: &Connection, word: &str, meaning: &str, tags: &str) -> rusqlite::Result<Entry> {
    let ts = now_unix();
    conn.execute(
        "INSERT INTO words (word, meaning, tags, updated_at) VALUES (?1, ?2, ?3, ?4)",
        params![word, meaning, tags, ts],
    )?;
    Ok(Entry::new(conn.last_insert_rowid(), word.into(), meaning.into(), tags.into(), ts))
}

#[allow(dead_code)]
pub fn delete(conn: &Connection, id: i64) -> rusqlite::Result<usize> {
    conn.execute("DELETE FROM words WHERE id = ?1", params![id])
}

/// 空白区切りの全語を含む(AND・大小文字無視・部分一致)エントリの添字を返す。
/// 見出し語の完全一致 > 前方一致 > 見出し語に含む > その他、の順に並べる(同順位は元の順)。
pub fn filter(entries: &[Entry], query: &str) -> Vec<u32> {
    let q = query.to_lowercase();
    let terms: Vec<&str> = q.split_whitespace().collect();
    if terms.is_empty() {
        return (0..entries.len() as u32).collect();
    }
    let mut hits: Vec<(u8, u32)> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        if terms.iter().all(|t| e.haystack.contains(t)) {
            let rank = if e.word_lc == q.trim() {
                0
            } else if e.word_lc.starts_with(terms[0]) {
                1
            } else if e.word_lc.contains(terms[0]) {
                2
            } else {
                3
            };
            hits.push((rank, i as u32));
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

    #[test]
    fn empty_query_returns_all() {
        assert_eq!(filter(&sample(), "").len(), 4);
        assert_eq!(filter(&sample(), "   ").len(), 4);
    }

    #[test]
    fn partial_match_and_case_insensitive() {
        let e = sample();
        let r = filter(&e, "APP");
        assert_eq!(r.len(), 4); // 辞書は意味に apple を含む
        // 見出し語の前方一致(apple, Apple Inc)が先、pineapple、意味のみの辞書が最後
        assert_eq!(r, vec![0, 2, 1, 3]);
    }

    #[test]
    fn exact_word_ranks_first() {
        let e = sample();
        assert_eq!(filter(&e, "apple")[0], 0);
    }

    #[test]
    fn and_search_and_tags() {
        let e = sample();
        assert_eq!(filter(&e, "apple company"), vec![2]);
        assert_eq!(filter(&e, "fruit"), vec![0, 1]);
        assert_eq!(filter(&e, "存在しない"), Vec::<u32>::new());
    }

    #[test]
    fn jst_format() {
        assert_eq!(format_jst(0), "1970/01/01 09:00:00");
        assert_eq!(format_jst(1_700_000_000), "2023/11/15 07:13:20");
    }

    #[test]
    fn db_roundtrip() {
        let dir = std::env::temp_dir().join(format!("fastdict-test-{}", std::process::id()));
        let path = dir.join("d.db");
        let conn = open(&path).unwrap();
        let a = insert(&conn, "b", "second", "t").unwrap();
        insert(&conn, "A", "first", "").unwrap();
        let all = load_all(&conn).unwrap();
        assert_eq!(all.iter().map(|e| e.word.as_str()).collect::<Vec<_>>(), vec!["A", "b"]);
        assert_eq!(delete(&conn, a.id).unwrap(), 1);
        assert_eq!(load_all(&conn).unwrap().len(), 1);
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }
}
