//! JSON でのエクスポート / インポート(画面に依存しない部分)。
//!
//! ```json
//! {
//!   "format": "MyDictionary",
//!   "version": 1,
//!   "entries": [
//!     {
//!       "word": "一期一会",
//!       "reading": "いちごいちえ",
//!       "meaning": "一生に一度の出会い。\nその機会を大切にすべきだという教え。",
//!       "tags": ["四字熟語"],
//!       "links": ["公式サイト | https://example.com/", "[[温故知新]]"],
//!       "id": 12,
//!       "updated_at": 1730000000,
//!       "updated": "2024/10/27 12:33:20"
//!     }
//!   ]
//! }
//! ```
//! 取り込み時に必須なのは `word` と `meaning` だけ。`entries` を省いて配列をそのまま書いてもよい。
//! `updated_at` は UNIX 秒(`updated` は人が読むための日本時間で、取り込み時には使わない)。
//! `id` は語の番号。`updated_at` と合わせて、上書きの取り込みで「どの語か」を照合するのに使う
//! (CSV / TSV の「管理用」の列と同じ役割。`import::import_rows` を参照)。

use crate::core::{self, Entry};
use crate::import::{Given, Row};
use serde_json::Value;

fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// 辞書の内容を JSON にする(項目の順番を読みやすく保つため、手で組み立てる)
pub fn export(entries: &[Entry]) -> String {
    let mut out = String::from("{\n  \"format\": \"MyDictionary\",\n  \"version\": 1,\n  \"entries\": [");
    for (i, e) in entries.iter().enumerate() {
        out.push_str(if i == 0 { "\n" } else { ",\n" });
        let list = |items: Vec<&str>| items.iter().map(|t| quote(t)).collect::<Vec<_>>().join(", ");
        let tags = list(e.tags.split(' ').filter(|t| !t.is_empty()).collect());
        let links = list(e.link.lines().map(str::trim).filter(|l| !l.is_empty()).collect());
        out.push_str(&format!(
            "    {{\n      \"word\": {},\n      \"reading\": {},\n      \"meaning\": {},\n      \"tags\": [{}],\n      \"links\": [{}],\n      \"id\": {},\n      \"updated_at\": {},\n      \"updated\": {}\n    }}",
            quote(&e.word),
            quote(&e.reading),
            quote(&e.meaning.replace("\r\n", "\n")),
            tags,
            links,
            e.id,
            e.updated_at,
            quote(&core::format_jst(e.updated_at)),
        ));
    }
    out.push_str(if entries.is_empty() { "]\n}\n" } else { "\n  ]\n}\n" });
    out
}

fn text_of(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or("").trim().to_string()
}

/// 文字列、または文字列の配列を、指定の区切りでつないだ文字列にする
fn joined(v: Option<&Value>, sep: &str) -> String {
    match v {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(sep),
        _ => String::new(),
    }
}

/// JSON を登録用の行にする。戻り値は (行, word か meaning が無くて読み飛ばした件数)
pub fn parse(text: &str) -> Result<(Vec<Row>, usize), String> {
    let root: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| format!("JSON として読めません: {e}"))?;
    let items = match &root {
        Value::Array(a) => a,
        Value::Object(o) => match o.get("entries") {
            Some(Value::Array(a)) => a,
            _ => return Err("\"entries\" の配列がありません".into()),
        },
        _ => return Err("配列、または \"entries\" を持つオブジェクトである必要があります".into()),
    };
    let mut rows = Vec::new();
    let mut skipped = 0;
    for it in items {
        let Some(o) = it.as_object() else {
            skipped += 1;
            continue;
        };
        let word = text_of(o.get("word"));
        let meaning = o.get("meaning").and_then(Value::as_str).unwrap_or("").replace("\r\n", "\n").trim().to_string();
        if word.is_empty() || meaning.is_empty() {
            skipped += 1;
            continue;
        }
        // links(配列)が無ければ link(文字列。改行区切り)を見る
        let links = if o.contains_key("links") { joined(o.get("links"), "\n") } else { joined(o.get("link"), "\n") };
        let updated_at = o.get("updated_at").and_then(Value::as_i64).filter(|&t| t > 0);
        let id = o.get("id").and_then(Value::as_i64).filter(|&i| i > 0);
        rows.push(Row {
            word,
            meaning,
            tags: core::normalize_tags(&joined(o.get("tags"), " ")),
            link: links.replace("\r\n", "\n"),
            reading: text_of(o.get("reading")),
            updated_at,
            // キーがある項目だけ「指定あり」とする(上書きのとき、無い項目は今の値を残す)
            given: Given { tags: o.contains_key("tags"), link: o.contains_key("links") || o.contains_key("link"), reading: o.contains_key("reading") },
            key: id.zip(updated_at),
        });
    }
    Ok((rows, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_keeps_everything() {
        let e = vec![
            Entry::new(1, "一期\"一会\"".into(), "1行目\n2行目\t\\ と「引用」".into(), "四字熟語 名言".into(), 1_730_000_000)
                .with_reading("いちごいちえ".into())
                .with_link("公式 | https://example.com/\n[[温故知新]]".into()),
            Entry::new(2, "apple".into(), "りんご".into(), "".into(), 5),
        ];
        let json = export(&e);
        assert!(json.contains("\"updated\": \"2024/10/27 12:33:20\""));
        let (rows, skipped) = parse(&json).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].word, e[0].word);
        assert_eq!(rows[0].reading, "いちごいちえ");
        assert_eq!(rows[0].meaning, e[0].meaning);
        assert_eq!(rows[0].tags, "四字熟語 名言");
        assert_eq!(rows[0].link, e[0].link);
        assert_eq!(rows[0].updated_at, Some(1_730_000_000));
        // 語の番号と更新日時(上書きの取り込みで照合に使う)、キーのある項目
        assert_eq!(rows[0].key, Some((1, 1_730_000_000)));
        assert_eq!(rows[0].given, Given { tags: true, link: true, reading: true });
        assert_eq!((rows[1].tags.as_str(), rows[1].link.as_str(), rows[1].reading.as_str()), ("", "", ""));
        assert_eq!(parse(&export(&[])).unwrap().0.len(), 0);
    }

    #[test]
    fn lenient_input() {
        // 配列だけ、tags が文字列、link が文字列、余分なキー、欠けた項目
        let text = r##"[
            {"word": "a", "meaning": "x", "tags": "#t1, t2", "link": "https://a.example/", "extra": 1},
            {"word": "b"},
            "not an object",
            {"word": " c ", "meaning": " y ", "updated_at": "bad"}
        ]"##;
        let (rows, skipped) = parse(text).unwrap();
        assert_eq!(skipped, 2);
        assert_eq!((rows[0].tags.as_str(), rows[0].link.as_str()), ("t1 t2", "https://a.example/"));
        assert_eq!((rows[1].word.as_str(), rows[1].meaning.as_str(), rows[1].updated_at), ("c", "y", None));
        // 自分で書いた JSON: 番号は無い。キーのある項目だけ「指定あり」
        assert_eq!((rows[0].key, rows[1].key), (None, None));
        assert_eq!(rows[0].given, Given { tags: true, link: true, reading: false });
        assert_eq!(rows[1].given, Given::default());
        assert!(parse("{\"x\": 1}").is_err());
        assert!(parse("not json").is_err());
        assert!(parse("42").is_err());
    }

    #[test]
    fn import_keeps_update_time() {
        let dir = std::env::temp_dir().join(format!("MyDictionary-json-{}", std::process::id()));
        let path = dir.join("d.db");
        let conn = core::open(&path).unwrap();
        core::migrate(&conn).unwrap();
        let src = vec![Entry::new(1, "語".into(), "意味".into(), "t".into(), 1234).with_reading("ご".into())];
        let (rows, _) = parse(&export(&src)).unwrap();
        assert_eq!(crate::import::insert_rows(&conn, &rows).unwrap(), (1, 0));
        let got = core::load_all(&conn).unwrap();
        assert_eq!((got[0].updated_at, got[0].reading.as_str()), (1234, "ご"));
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }
}
