//! CSV / TSV のインポート(画面に依存しない部分)。
//!
//! - 1行目が見出し行なら、列名で対応づける(見出し語 / 読み / 意味 / タグ / 参照先。英語名も可、順不同)
//! - 見出し行が無ければ、左から 見出し語, 意味, タグ, 参照先, 読み の順とみなす
//! - 値は "..." で囲める(区切り文字・改行・"" を含められる。Excel の出力と同じ)
//! - TSV で改行を表したいときは `\n` と書ける

use crate::core;
use rusqlite::Connection;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub word: String,
    pub meaning: String,
    pub tags: String,
    pub link: String,
    pub reading: String,
    /// 更新日時(UNIX秒)。None なら取り込んだ時刻
    pub updated_at: Option<i64>,
}

/// UTF-8(BOM 有無どちらも)または UTF-16(BOM 付き)として読む。どちらでもなければ None
/// (その場合、呼び出し側が Shift_JIS として読み直す)。
pub fn decode_unicode(bytes: &[u8]) -> Option<String> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(rest.to_vec()).ok();
    }
    for (bom, big) in [([0xFF, 0xFE], false), ([0xFE, 0xFF], true)] {
        if let Some(rest) = bytes.strip_prefix(&bom) {
            let units: Vec<u16> = rest
                .chunks_exact(2)
                .map(|c| if big { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) })
                .collect();
            return String::from_utf16(&units).ok();
        }
    }
    String::from_utf8(bytes.to_vec()).ok()
}

/// 区切り文字を決める: 拡張子が .tsv / .tab ならタブ、.csv ならカンマ。それ以外は1行目にタブがあればタブ
pub fn sniff_delim(text: &str, file_name: &str) -> char {
    let name = file_name.to_lowercase();
    if name.ends_with(".tsv") || name.ends_with(".tab") {
        '\t'
    } else if name.ends_with(".csv") {
        ','
    } else if text.lines().next().map_or(false, |l| l.contains('\t')) {
        '\t'
    } else {
        ','
    }
}

/// 区切りテキストを行×列に分解する。空行は捨てる
pub fn parse_table(text: &str, delim: char) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut quoted = false; // 引用符の中
    let mut at_start = true; // セルの先頭
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if quoted {
            if c == '"' {
                if it.peek() == Some(&'"') {
                    cell.push('"');
                    it.next();
                } else {
                    quoted = false;
                }
            } else {
                cell.push(c);
            }
            continue;
        }
        if c == '"' && at_start {
            quoted = true;
            at_start = false;
        } else if c == delim {
            row.push(std::mem::take(&mut cell));
            at_start = true;
        } else if c == '\n' || c == '\r' {
            if c == '\r' && it.peek() == Some(&'\n') {
                it.next();
            }
            row.push(std::mem::take(&mut cell));
            if row.iter().any(|x| !x.trim().is_empty()) {
                rows.push(std::mem::take(&mut row));
            } else {
                row.clear();
            }
            at_start = true;
        } else {
            cell.push(c);
            at_start = false;
        }
    }
    row.push(cell);
    if row.iter().any(|x| !x.trim().is_empty()) {
        rows.push(row);
    }
    rows
}

fn header_kind(name: &str) -> Option<usize> {
    let n = name.trim().trim_start_matches('\u{feff}').to_lowercase();
    const NAMES: [&[&str]; 5] = [
        &["見出し語", "見出し", "単語", "語", "word", "term", "headword"],
        &["意味", "説明", "meaning", "definition", "description"],
        &["タグ", "tag", "tags"],
        &["参照先", "リンク", "link", "links", "url"],
        &["読み", "よみ", "読み方", "ふりがな", "reading", "yomi", "kana"],
    ];
    NAMES.iter().position(|set| set.contains(&n.as_str()))
}

/// 表を登録用の行に変換する。戻り値は (行, 見出し語か意味が空で読み飛ばした件数)
pub fn to_rows(table: Vec<Vec<String>>) -> (Vec<Row>, usize) {
    let mut it = table.into_iter().peekable();
    // 列の対応: [見出し語, 意味, タグ, 参照先, 読み] がそれぞれ何列目か
    let mut cols: [Option<usize>; 5] = [Some(0), Some(1), Some(2), Some(3), Some(4)];
    if let Some(first) = it.peek() {
        let kinds: Vec<Option<usize>> = first.iter().map(|c| header_kind(c)).collect();
        if kinds.contains(&Some(0)) && kinds.contains(&Some(1)) {
            cols = [None; 5];
            for (i, k) in kinds.iter().enumerate() {
                if let Some(k) = k {
                    cols[*k].get_or_insert(i);
                }
            }
            it.next(); // 見出し行は登録しない
        }
    }
    let mut rows = Vec::new();
    let mut skipped = 0;
    for r in it {
        let get = |k: usize| cols[k].and_then(|i| r.get(i)).map(|s| s.trim().to_string()).unwrap_or_default();
        let unescape = |s: String| s.replace("\\r\\n", "\n").replace("\\n", "\n").replace("\r\n", "\n");
        let row = Row { word: get(0), meaning: unescape(get(1)), tags: core::normalize_tags(&get(2)), link: unescape(get(3)), reading: get(4), updated_at: None };
        if row.word.is_empty() || row.meaning.is_empty() {
            skipped += 1;
        } else {
            rows.push(row);
        }
    }
    (rows, skipped)
}

/// 行を登録する。見出し語と意味が両方同じものは重複として飛ばす(同じファイルを2回取り込んでも増えない)。
/// 戻り値は (追加した件数, 重複で飛ばした件数)。トランザクションは呼び出し側で管理する。
pub fn insert_rows(conn: &Connection, rows: &[Row]) -> rusqlite::Result<(usize, usize)> {
    let mut seen: HashSet<(String, String)> = {
        let mut st = conn.prepare("SELECT word, meaning FROM words")?;
        let v = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        v
    };
    let (mut added, mut dup) = (0, 0);
    for r in rows {
        if seen.insert((r.word.clone(), r.meaning.clone())) {
            let f = core::Fields { word: r.word.clone(), reading: r.reading.clone(), meaning: r.meaning.clone(), tags: r.tags.clone(), link: r.link.clone() };
            core::insert_entry(conn, &f, r.updated_at)?;
            added += 1;
        } else {
            dup += 1;
        }
    }
    Ok((added, dup))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_with_quotes_and_header() {
        let text = "タグ,見出し語,意味,URL\r\n\"fruit, red\",apple,\"りんご。\n\"\"赤い\"\"果物\",https://example.com/\r\n,,空の行は飛ばす,\r\n\r\nx,banana,バナナ,\r\n";
        let (rows, skipped) = to_rows(parse_table(text, ','));
        assert_eq!(skipped, 1);
        assert_eq!(
            rows,
            vec![
                Row { word: "apple".into(), meaning: "りんご。\n\"赤い\"果物".into(), tags: "fruit red".into(), link: "https://example.com/".into(), reading: "".into(), updated_at: None },
                Row { word: "banana".into(), meaning: "バナナ".into(), tags: "x".into(), link: "".into(), reading: "".into(), updated_at: None },
            ]
        );
    }

    #[test]
    fn tsv_without_header_is_positional() {
        let text = "apple\tりんご\\n果物\t#fruit\nbanana\tバナナ\n";
        let (rows, skipped) = to_rows(parse_table(text, '\t'));
        assert_eq!(skipped, 0);
        assert_eq!(rows[0], Row { word: "apple".into(), meaning: "りんご\n果物".into(), tags: "fruit".into(), link: "".into(), reading: "".into(), updated_at: None });
        // 読みの列(列名で指定)
        let (named, _) = to_rows(parse_table("読み\t見出し語\t意味\nりんご\t林檎\t果物\n", '\t'));
        assert_eq!((named[0].word.as_str(), named[0].reading.as_str()), ("林檎", "りんご"));
        assert_eq!(rows[1].word, "banana");
        assert_eq!(rows[1].tags, "");
    }

    #[test]
    fn delimiter_and_encoding() {
        assert_eq!(sniff_delim("a,b", "x.CSV"), ',');
        assert_eq!(sniff_delim("a,b", "x.tsv"), '\t');
        assert_eq!(sniff_delim("a\tb\nc", "x.txt"), '\t');
        assert_eq!(sniff_delim("a,b", "x.txt"), ',');
        assert_eq!(decode_unicode("\u{feff}語,意味".as_bytes()).unwrap(), "語,意味");
        let utf16: Vec<u8> = [0xFFu8, 0xFE].into_iter().chain("語\t意味".encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
        assert_eq!(decode_unicode(&utf16).unwrap(), "語\t意味");
        // Shift_JIS の「語」(8C EA) は UTF-8 として不正 → None(呼び出し側が読み直す)
        assert!(decode_unicode(&[0x8C, 0xEA, b',', b'a']).is_none());
    }

    #[test]
    fn insert_skips_duplicates() {
        let dir = std::env::temp_dir().join(format!("fastdict-import-{}", std::process::id()));
        let path = dir.join("d.db");
        let conn = core::open(&path).unwrap();
        core::migrate(&conn).unwrap();
        let (rows, _) = to_rows(parse_table("a,1\nb,2\na,1\na,別の意味\n", ','));
        assert_eq!(insert_rows(&conn, &rows).unwrap(), (3, 1)); // 同じ「a,1」は1回だけ
        assert_eq!(insert_rows(&conn, &rows).unwrap(), (0, 4)); // 取り込み直しても増えない
        assert_eq!(core::load_all(&conn).unwrap().len(), 3);
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }
}
