//! CSV / TSV の取り込みと書き出し、取り込みの手順(画面に依存しない部分)。
//!
//! 取り込み:
//! - 1行目が見出し行なら、列名で対応づける(見出し語 / 読み / 意味 / タグ / 参照先 / 管理用。英語名も可、順不同)
//! - 見出し行が無ければ、左から 見出し語, 意味, タグ, 参照先, 読み の順とみなす
//! - 値は "..." で囲める(区切り文字・改行・"" を含められる。Excel の出力と同じ)
//! - TSV で改行を表したいときは `\n` と書ける("..." で囲んだ値と、CSV では、書いたとおりに扱う)
//!
//! 書き出し(`export_table`)は、取り込みがそのまま読める形にする。「管理用」の列に、語の番号と
//! 書き出した時点の更新日時を入れておき、上書きの取り込みで「どの語か」を見出し語ではなく番号で照合する。

use crate::core::{self, Entry};
use crate::undo::{Snapshot, Undo};
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};

/// 行のうち、ファイルに列(JSON ならキー)があった項目。上書きのとき、列の無い項目は今の値を残す
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Given {
    pub tags: bool,
    pub link: bool,
    pub reading: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Row {
    pub word: String,
    pub meaning: String,
    pub tags: String,
    pub link: String,
    pub reading: String,
    /// 更新日時(UNIX秒)。None なら取り込んだ時刻
    pub updated_at: Option<i64>,
    /// ファイルに列があった項目
    pub given: Given,
    /// 管理用の列: (語の番号, 書き出した時点の更新日時)。この辞書から書き出したファイルの行に付いている
    pub key: Option<(i64, i64)>,
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

/// 区切りテキストを、行×列に分解したもの
pub struct Table {
    pub rows: Vec<Vec<String>>,
    /// `rows` と同じ形。そのセルで `\n` の書き方を改行として扱うか
    /// (タブ区切りで、"..." で囲まれていないセルだけ)
    escapes: Vec<Vec<bool>>,
    /// `rows` と同じ形。そのセルが "..." で囲まれていたか(囲まれた値は、前後の空白・改行を削らない)
    quoted: Vec<Vec<bool>>,
    /// 引用符(")が閉じないまま、ファイルが終わった。その引用符があった行(1 始まり、ファイルの行)
    pub unclosed_quote: Option<usize>,
}

/// 区切りテキストを行×列に分解する。空行は捨てる
pub fn parse_table(text: &str, delim: char) -> Table {
    let tsv = delim == '\t';
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut escapes: Vec<Vec<bool>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut esc: Vec<bool> = Vec::new();
    let mut quoted_rows: Vec<Vec<bool>> = Vec::new();
    let mut qrow: Vec<bool> = Vec::new();
    let mut cell = String::new();
    let mut quoted = false; // 引用符の中
    let mut was_quoted = false; // このセルは引用符で始まった
    let mut at_start = true; // セルの先頭
    let mut line = 1usize; // ファイルの行(引用符の中の改行も数える)
    let mut quote_line = 0usize; // 開いている引用符の行
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\n' {
            line += 1;
        }
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
            quote_line = line;
            quoted = true;
            was_quoted = true;
            at_start = false;
        } else if c == delim {
            row.push(std::mem::take(&mut cell));
            esc.push(tsv && !was_quoted);
            qrow.push(was_quoted);
            was_quoted = false;
            at_start = true;
        } else if c == '\n' || c == '\r' {
            if c == '\r' {
                // CR LF と CR だけの改行も、1行と数える(LF は、ループの先頭で数えている)
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                line += 1;
            }
            row.push(std::mem::take(&mut cell));
            esc.push(tsv && !was_quoted);
            qrow.push(was_quoted);
            was_quoted = false;
            if row.iter().any(|x| !x.trim().is_empty()) {
                rows.push(std::mem::take(&mut row));
                escapes.push(std::mem::take(&mut esc));
                quoted_rows.push(std::mem::take(&mut qrow));
            } else {
                row.clear();
                esc.clear();
                qrow.clear();
            }
            at_start = true;
        } else {
            cell.push(c);
            at_start = false;
        }
    }
    row.push(cell);
    esc.push(tsv && !was_quoted);
    qrow.push(was_quoted);
    if row.iter().any(|x| !x.trim().is_empty()) {
        rows.push(row);
        escapes.push(esc);
        quoted_rows.push(qrow);
    }
    Table { rows, escapes, quoted: quoted_rows, unclosed_quote: quoted.then_some(quote_line) }
}

/// 列名 → 列の種類(0: 見出し語, 1: 意味, 2: タグ, 3: 参照先, 4: 読み, 5: 管理用)。
/// 「管理用(変更しない)」のような、括弧の注意書きは無視する
fn header_kind(name: &str) -> Option<usize> {
    let n = name.trim().trim_start_matches('\u{feff}').to_lowercase();
    let n = n.split(['(', '(']).next().unwrap_or("").trim();
    const NAMES: [&[&str]; 6] = [
        &["見出し語", "見出し", "単語", "語", "用語", "原語", "項目", "名称", "英単語", "英語", "語句", "略語", "word", "term", "headword", "source", "entry"],
        &["意味", "説明", "訳語", "訳", "日本語訳", "和訳", "日本語", "定義", "解説", "内容", "回答", "meaning", "definition", "description", "translation", "target", "answer"],
        &["タグ", "分類", "分野", "カテゴリ", "カテゴリー", "tag", "tags", "category"],
        &["参照先", "リンク", "link", "links", "url"],
        &["読み", "よみ", "読み方", "ふりがな", "フリガナ", "reading", "yomi", "kana"],
        &["管理用", "管理番号", "key"],
    ];
    NAMES.iter().position(|set| set.contains(&n))
}

/// 書き出しの見出し行(この順で書き出す)。「更新日時」は見るための列で、取り込みでは使わない
const EXPORT_HEADER: [&str; 7] = ["見出し語", "読み", "意味", "タグ", "参照先", "更新日時", "管理用(変更しない)"];

/// 管理用の列の値: 「語の番号_更新日時」
fn format_key(id: i64, updated_at: i64) -> String {
    format!("{id}_{updated_at}")
}

fn parse_key(s: &str) -> Option<(i64, i64)> {
    let (id, ts) = s.trim().split_once('_')?;
    let (id, ts) = (id.trim().parse::<i64>().ok()?, ts.trim().parse::<i64>().ok()?);
    (id > 0).then_some((id, ts))
}

/// 表を登録用の行に変換した結果
pub struct Converted {
    pub rows: Vec<Row>,
    /// 見出し語か意味が空で、読み飛ばした行の数
    pub skipped: usize,
    /// 取り込む前の確認の画面に出す説明(列の対応、閉じていない引用符など)
    pub notes: Vec<String>,
}

const KIND_NAMES: [&str; 6] = ["見出し語", "意味", "タグ", "参照先", "読み", "管理用"];

/// 表を登録用の行に変換する。戻り値は (行, 見出し語か意味が空で読み飛ばした件数)
#[cfg_attr(not(test), allow(dead_code))]
pub fn to_rows(table: Table) -> (Vec<Row>, usize) {
    let c = to_rows_detail(table);
    (c.rows, c.skipped)
}

/// 表を登録用の行に変換する(列の対応などの説明も返す)
pub fn to_rows_detail(table: Table) -> Converted {
    let Table { rows: cells, escapes, quoted, unclosed_quote } = table;
    let mut notes = Vec::new();
    if let Some(line) = unclosed_quote {
        notes.push(format!(
            "{line} 行目の引用符(\")が閉じていません。そこから最後までが、1つの値として読まれています。ファイルを確かめてください"
        ));
    }
    // 列の対応: [見出し語, 意味, タグ, 参照先, 読み, 管理用] がそれぞれ何列目か
    let mut cols: [Option<usize>; 6] = [Some(0), Some(1), Some(2), Some(3), Some(4), None];
    let mut start = 0;
    let header: Option<Vec<Option<usize>>> = cells.first().map(|first| first.iter().map(|c| header_kind(c)).collect());
    let given = match header {
        // 1行目の、空でない値の半分以上が知っている列名なら、見出し行とみなす
        // (語の行に「訳」のような列名と同じ語が1つ混ざっただけで、見出し行と取り違えないように)
        Some(kinds) if kinds.iter().any(|k| k.is_some()) && kinds.iter().filter(|k| k.is_some()).count() * 2 >= cells[0].iter().filter(|c| !c.trim().is_empty()).count() => {
            let names = &cells[0];
            cols = [None; 6];
            for (i, k) in kinds.iter().enumerate() {
                if let Some(k) = k {
                    cols[*k].get_or_insert(i);
                }
            }
            // 見出し語・意味の列名が見つからなければ、使われていない列を左から当てる
            for k in [0, 1] {
                if cols[k].is_none() {
                    cols[k] = (0..names.len()).find(|i| kinds[*i].is_none() && !cols.contains(&Some(*i)));
                }
            }
            start = 1; // 見出し行は登録しない
            let mut used: Vec<String> = Vec::new();
            for (k, col) in cols.iter().enumerate() {
                if let Some(i) = col {
                    used.push(format!("「{}」→ {}", names[*i].trim().trim_start_matches('\u{feff}'), KIND_NAMES[k]));
                }
            }
            let unused: Vec<String> = (0..names.len()).filter(|i| !cols.contains(&Some(*i)) && !names[*i].trim().is_empty()).map(|i| format!("「{}」", names[i].trim())).collect();
            let mut note = format!("列の対応: {}", used.join("、"));
            if !unused.is_empty() {
                note.push_str(&format!("。使わない列: {}", unused.join("、")));
            }
            notes.push(note);
            Given { tags: cols[2].is_some(), link: cols[3].is_some(), reading: cols[4].is_some() }
        }
        _ => {
            let first: Vec<&str> = cells.first().map(|r| r.iter().map(|c| c.trim()).filter(|c| !c.is_empty()).take(3).collect()).unwrap_or_default();
            notes.push(format!(
                "1行目に列名が無いため、左から 見出し語・意味・タグ・参照先・読み の順として読みます。1行目(「{}」)も語として取り込みます。列名の行なら、列名を説明書の書き方に直してください",
                first.join("」「")
            ));
            // 見出し行が無い表: どこかの行にその列があれば、「列がある」とみなす
            let width = cells.iter().map(Vec::len).max().unwrap_or(0);
            Given { tags: width > 2, link: width > 3, reading: width > 4 }
        }
    };
    let mut rows = Vec::new();
    let mut skipped = 0;
    let (mut skipped_at, mut wide_at): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
    // 見出し行がある表は、見出し行の列の数まで。無い表は、5列(見出し語・意味・タグ・参照先・読み)まで
    let width = if start == 1 { cells[0].len() } else { 5 };
    for (n, ((r, esc), quo)) in cells.iter().zip(&escapes).zip(&quoted).enumerate().skip(start) {
        let get = |k: usize| cols[k].and_then(|i| r.get(i)).map(|s| s.trim().to_string()).unwrap_or_default();
        // 意味と参照先: "..." で囲まれた値は、前後の空白・改行も値のうち(書き出しは、それを守るために囲んでいる)
        let get_raw = |k: usize| match cols[k] {
            Some(i) if quo.get(i).copied().unwrap_or(false) => r.get(i).cloned().unwrap_or_default(),
            _ => get(k),
        };
        // 改行を含められる値。タブ区切りで "..." で囲んでいない値は、`\n` を改行として扱う。
        // 参照先(`is_link`)は、`\\fs01\netadmin` のような共有フォルダのパスを壊さないように、
        // 区切った後の各行が、どれも参照先の形をしているときだけ改行にする。
        // 意味は、パスの中(`![説明](…)` の括弧の中や、`\\` `C:\` で始まる語の中)の `\n` を改行にしない
        let multiline = |k: usize, is_link: bool| {
            let v = get_raw(k);
            let escaped = cols[k].and_then(|i| esc.get(i)).copied().unwrap_or(false);
            let v = if !escaped {
                v
            } else if !is_link {
                unescape_meaning(&v)
            } else {
                let split = v.replace("\\r\\n", "\n").replace("\\n", "\n");
                if split.lines().skip(1).all(looks_like_link_line) {
                    split
                } else {
                    v
                }
            };
            v.replace("\r\n", "\n").replace('\r', "\n")
        };
        let row = Row {
            word: get(0),
            meaning: multiline(1, false),
            tags: core::normalize_tags(&get(2)),
            link: multiline(3, true),
            reading: get(4),
            updated_at: None,
            given,
            key: parse_key(&get(5)),
        };
        if row.word.is_empty() || row.meaning.is_empty() {
            skipped += 1;
            skipped_at.push(n + 1);
        } else {
            rows.push(row);
        }
        if r.iter().skip(width).any(|c| !c.trim().is_empty()) {
            wide_at.push(n + 1);
        }
    }
    let list = |v: &[usize]| v.iter().take(5).map(|n| n.to_string()).collect::<Vec<_>>().join("、") + if v.len() > 5 { "…" } else { "" };
    if !skipped_at.is_empty() {
        notes.push(format!("見出し語か意味が空のため取り込まない行: 表の {} 行目", list(&skipped_at)));
    }
    if !wide_at.is_empty() {
        notes.push(format!(
            "列名より後ろの列にも値がある行が {} 行あります(表の {} 行目)。その値は取り込みません。値の中に区切りの文字が入って、列がずれていないか確かめてください",
            wide_at.len(),
            list(&wide_at)
        ));
    }
    Converted { rows, skipped, notes }
}

/// 参照先の1行らしいか(`[[語]]`、URL、パス、「表示名 | 開く先」)。空の行もよい
fn looks_like_link_line(line: &str) -> bool {
    let t = line.trim();
    t.is_empty() || t.starts_with("[[") || t.contains(" | ") || t.contains("://") || core::is_link_target(t) || has_scheme(t)
}

/// `ms-settings:display` `tel:0312345678` のような、`//` の無い「スキーム:」で始まるか(2文字以上。`C:` は除く)
fn has_scheme(t: &str) -> bool {
    match t.split_once(':') {
        Some((scheme, rest)) => {
            scheme.len() >= 2
                && !rest.is_empty()
                && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        }
        None => false,
    }
}

/// タブ区切りの意味の欄の `\n`(と `\r\n`)を改行にする。ただし、パスの中のものは書いたとおりに残す:
/// - `[表示名](…)` `![説明](…)` の括弧の中
/// - 空白から始まる語のうち、`\\`(共有フォルダ)や `C:\` で始まるもの
fn unescape_meaning(v: &str) -> String {
    let chars: Vec<char> = v.chars().collect();
    let mut out = String::with_capacity(v.len());
    let mut in_target = false; // `](` の後、`)` まで
    let mut token_is_path = false; // いまの語(空白の後から)がパスで始まった
    let mut token_start = true;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if token_start && !c.is_whitespace() {
            let rest: String = chars[i..chars.len().min(i + 3)].iter().collect();
            let b = rest.as_bytes();
            token_is_path = rest.starts_with("\\\\") || (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'));
            token_start = false;
        }
        if c == ']' && chars.get(i + 1) == Some(&'(') {
            in_target = true;
            out.push_str("](");
            i += 2;
            continue;
        }
        if in_target && c == ')' {
            in_target = false;
        }
        if c == '\\' && !in_target && !token_is_path {
            let next: String = chars[i + 1..chars.len().min(i + 4)].iter().collect();
            if next.starts_with("r\\n") {
                out.push('\n');
                i += 4;
                token_start = true;
                continue;
            }
            if next.starts_with('n') {
                out.push('\n');
                i += 2;
                token_start = true;
                continue;
            }
        }
        if c.is_whitespace() {
            token_start = true;
            token_is_path = false;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// 取り込む行を点検する(CSV / TSV / JSON 共通)。
/// - 制御文字と、文字の向きを強制する文字を取り除く(`core::clean_text`)
/// - 長すぎる欄のある行は、取り込まない(`core::too_long`。極端に長い語を表示すると、画面が固まるため)
///
/// 戻り値は (取り込む行, 長すぎて外した行の見出し語, 文字を取り除いた行の数)
pub fn screen(rows: Vec<Row>) -> (Vec<Row>, Vec<String>, usize) {
    let mut out = Vec::with_capacity(rows.len());
    let (mut long, mut cleaned) = (Vec::new(), 0);
    for mut r in rows {
        let mut changed = false;
        for f in [&mut r.word, &mut r.meaning, &mut r.tags, &mut r.link, &mut r.reading] {
            let (c, ch) = core::clean_text(f);
            if ch {
                *f = c;
                changed = true;
            }
        }
        if changed {
            cleaned += 1;
        }
        if core::too_long(&r.word, &r.reading, &r.meaning, &r.tags, &r.link).is_some() {
            long.push(r.word.chars().take(30).collect());
            continue;
        }
        out.push(r);
    }
    (out, long, cleaned)
}

/// 辞書の内容を CSV / TSV にする(`delim` が区切り文字)。UTF-8(BOM 付き)で、行末は CRLF。
/// 取り込み(`parse_table` → `to_rows`)でそのまま読め、内容が元に戻る。
pub fn export_table(entries: &[Entry], delim: char) -> String {
    // 区切り文字・引用符・改行を含む値は "..." で囲む。`\`(TSV の `\n` の書き方と区別するため)と、
    // 前後に空白のある値も囲む
    let cell = |v: &str| {
        let needs = v.contains(delim) || v.contains(['"', '\n', '\r', '\\']) || v != v.trim();
        if needs {
            format!("\"{}\"", v.replace('"', "\"\""))
        } else {
            v.to_string()
        }
    };
    let sep = delim.to_string();
    let mut out = String::from("\u{feff}");
    out.push_str(&EXPORT_HEADER.map(cell).join(&sep));
    out.push_str("\r\n");
    for e in entries {
        let fields = [
            cell(&e.word),
            cell(&e.reading),
            cell(&e.meaning.replace("\r\n", "\n")),
            cell(&e.tags),
            cell(&e.link.replace("\r\n", "\n")),
            cell(&core::format_jst(e.updated_at)),
            cell(&format_key(e.id, e.updated_at)),
        ];
        out.push_str(&fields.join(&sep));
        out.push_str("\r\n");
    }
    out
}

// ---- 取り込みの手順 ---------------------------------------------------------

/// 同じ見出し語の語がすでにあるときの扱い
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    /// 両方残す(これまでの動作): 見出し語と意味が両方同じ行だけ飛ばし、ほかは追加する
    KeepBoth,
    /// 上書きする: 同じ語を、ファイルの内容で置き換える
    Overwrite,
    /// 飛ばす: 見出し語がすでにある行は取り込まない
    Skip,
}

/// 取り込みの結果(見込みを出すときも、実行したときも同じ形)
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Outcome {
    /// 追加した語
    pub added: usize,
    /// 上書きした語
    pub overwritten: usize,
    /// 辞書の語と内容が同じで、何も変えなかった行(上書き)
    pub unchanged: usize,
    /// 見出し語と意味が同じ語があるので、追加しなかった行
    pub duplicate: usize,
    /// 見出し語がすでにあるので、取り込まなかった行(飛ばす)
    pub skipped: usize,
    /// 書き出した後に辞書の側が変更されていたので、上書きしなかった語(見出し語)
    pub conflicts: Vec<String>,
    /// 同じ見出し語が辞書かファイルに複数あり、どれを上書きするか決められないので、追加の規則で扱った見出し語
    pub ambiguous: Vec<String>,
    /// 上書きした語の (前の見出し語, 新しい見出し語)
    pub changes: Vec<(String, String)>,
    /// 上書きした語ごとの、変わる項目の説明(`changes` と同じ順)
    pub diffs: Vec<Vec<String>>,
    /// 表計算ソフトで崩れたように見える値(「#NAME?」、先頭の 0 が消えた番号など)。(見出し語, 説明)
    pub suspicious: Vec<(String, String)>,
}

impl Outcome {
    /// 「追加 3 件、上書き 2 件、…」の形(0 件の項目は省く)
    pub fn summary(&self) -> String {
        let parts: Vec<String> = [
            ("追加", self.added),
            ("上書き", self.overwritten),
            ("変更なし", self.unchanged),
            ("同じ内容のため追加しない", self.duplicate),
            ("見出し語があるため取り込まない", self.skipped),
            ("書き出した後に変更されたため上書きしない", self.conflicts.len()),
        ]
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(name, n)| format!("{name} {n} 件"))
        .collect();
        if parts.is_empty() {
            "取り込む語はありません".to_string()
        } else {
            parts.join("、")
        }
    }

    /// 辞書を変更する件数
    pub fn writes(&self) -> usize {
        self.added + self.overwritten
    }
}

/// 上書きの見込みの詳細(確認画面の「詳細…」と、結果の画面に出す)。すべての語と、項目ごとの変更を書く。何も無ければ空
pub fn overwrite_details(plan: &Outcome) -> String {
    let mut out = String::new();
    let mut section = |title: &str, items: Vec<String>| {
        if items.is_empty() {
            return;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("■ {title}({} 件)\n", items.len()));
        for it in items {
            out.push_str(&format!("・{it}\n"));
        }
    };
    section(
        "表計算ソフトで崩れた可能性のある値(確かめてから取り込んでください)",
        plan.suspicious.iter().map(|(w, d)| format!("{w}: {d}")).collect(),
    );
    section(
        "上書きする語",
        plan.changes
            .iter()
            .zip(plan.diffs.iter().map(Some).chain(std::iter::repeat(None)))
            .map(|((a, b), d)| {
                let name = if a == b { a.clone() } else { format!("{a} → {b}") };
                match d {
                    Some(d) if !d.is_empty() => format!("{name}\n    {}", d.join("\n    ")),
                    _ => name,
                }
            })
            .collect(),
    );
    section("書き出した後に変更されたため、上書きしない語", plan.conflicts.clone());
    section("同じ見出し語が複数あるため、追加として扱う語", plan.ambiguous.clone());
    out
}

/// 値を、詳細の画面に出す長さに縮める(改行は「↵」にする)
fn brief(v: &str) -> String {
    let one: String = v.replace("\r\n", "\n").chars().map(|c| if c == '\n' { '↵' } else { c }).collect();
    if one.chars().count() > 40 {
        format!("「{}…」", one.chars().take(40).collect::<String>())
    } else {
        format!("「{one}」")
    }
}

/// 上書きで変わる項目の説明
fn diff_lines(ex: &Snapshot, new: &Snapshot) -> Vec<String> {
    let mut v = Vec::new();
    for (name, a, b) in [
        ("見出し語", &ex.word, &new.word),
        ("読み", &ex.reading, &new.reading),
        ("意味", &ex.meaning, &new.meaning),
        ("タグ", &ex.tags, &new.tags),
        ("参照先", &ex.link, &new.link),
    ] {
        let (a2, b2) = (a.replace("\r\n", "\n"), b.replace("\r\n", "\n"));
        if a2 == b2 {
            continue;
        }
        if b2.trim().is_empty() {
            v.push(format!("{name}: {} → 空にする", brief(a)));
        } else if a2.trim().is_empty() {
            v.push(format!("{name}: (空) → {}", brief(b)));
        } else {
            v.push(format!("{name}: {}", brief_diff(&a2, &b2)));
        }
    }
    v
}

/// 2つの値の違いを、違う箇所の前後だけ見せる(長い意味の後ろの方が違っても見えるように)。
/// 違いが数文字だけなら、文字の番号も添える(〜 と ～ のような、見た目の似た文字の違いが分かるように)
fn brief_diff(a: &str, b: &str) -> String {
    let (ac, bc): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let pre = ac.iter().zip(&bc).take_while(|(x, y)| x == y).count();
    let suf = ac[pre..].iter().rev().zip(bc[pre..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (da, db) = (&ac[pre..ac.len() - suf], &bc[pre..bc.len() - suf]);
    let start = pre.saturating_sub(12);
    let show = |cs: &[char], diff_len: usize| -> String {
        let end = (pre + diff_len + 12).min(cs.len());
        let body: String = cs[start..end].iter().map(|&c| if c == '\n' { '↵' } else { c }).collect();
        let body: String = if body.chars().count() > 60 { body.chars().take(60).collect::<String>() + "…" } else { body };
        format!("「{}{}{}」", if start > 0 { "…" } else { "" }, body, if end < cs.len() && !body.ends_with('…') { "…" } else { "" })
    };
    let mut line = format!("{} → {}", show(&ac, da.len()), show(&bc, db.len()));
    if !da.is_empty() && !db.is_empty() && da.len() <= 3 && db.len() <= 3 {
        let code = |cs: &[char]| cs.iter().map(|c| format!("U+{:04X}", *c as u32)).collect::<Vec<_>>().join(" ");
        line.push_str(&format!("(文字の違い: {} → {})", code(da), code(db)));
    }
    line
}

/// 表計算ソフト(Excel など)で開いて保存したときに、崩れたように見える値か。崩れていそうなら説明を返す。
/// `old` は辞書の今の値(分からなければ空)
pub fn excel_damage(old: &str, new: &str) -> Option<String> {
    let n = new.trim();
    const ERRORS: [&str; 9] = ["#NAME?", "#VALUE!", "#REF!", "#DIV/0!", "#N/A", "#NUM!", "#NULL!", "#SPILL!", "#CALC!"];
    if ERRORS.iter().any(|e| n.eq_ignore_ascii_case(e)) {
        return Some(format!("{n}(数式のエラー。元の値が数式として扱われた可能性)"));
    }
    if !n.is_empty() && n.chars().all(|c| c == '#') {
        return Some("「####」(列の幅が足りないときの表示が、そのまま保存された可能性)".into());
    }
    let o = old.trim();
    // 先頭の 0 が消えた(0123 → 123)
    if o.len() > 1 && o.starts_with('0') && o.chars().all(|c| c.is_ascii_digit()) && o.trim_start_matches('0') == n && !n.is_empty() {
        return Some(format!("{} → {}(先頭の 0 が消えた可能性)", brief(o), brief(n)));
    }
    // 長い数字が指数の形になった(123456789012 → 1.23457E+11)
    if !o.is_empty() && o.chars().all(|c| c.is_ascii_digit()) && o.len() >= 12 && n.contains(['E', 'e']) && n.contains('+') {
        return Some(format!("{} → {}(長い数字が指数の形になった可能性)", brief(o), brief(n)));
    }
    None
}

/// 取り込みの間、辞書の内容をメモリ上で追うための表
struct Book {
    by_id: HashMap<i64, Snapshot>,
    by_word: HashMap<String, Vec<i64>>,
    /// (見出し語, 意味) の組。同じ内容の行を二重に追加しないため
    seen: HashSet<(String, String)>,
    /// 見込みを出すときに、追加する語へ仮に振る番号(負の数)
    next_fake: i64,
}

impl Book {
    fn load(conn: &Connection) -> rusqlite::Result<Book> {
        let mut b = Book { by_id: HashMap::new(), by_word: HashMap::new(), seen: HashSet::new(), next_fake: -1 };
        for e in core::load_all(conn)? {
            b.put(Snapshot::of(&e));
        }
        Ok(b)
    }

    fn put(&mut self, s: Snapshot) {
        self.seen.insert(seen_key(&s.word, &s.meaning));
        self.by_word.entry(core::word_key(&s.word)).or_default().push(s.id);
        self.by_id.insert(s.id, s);
    }

    /// 語の内容を置き換える(見出し語が変わっていれば、見出し語の索引も直す)
    fn replace(&mut self, s: Snapshot) {
        if let Some(old) = self.by_id.get(&s.id) {
            if core::word_key(&old.word) != core::word_key(&s.word) {
                if let Some(v) = self.by_word.get_mut(&core::word_key(&old.word)) {
                    v.retain(|&i| i != s.id);
                }
                self.by_word.entry(core::word_key(&s.word)).or_default().push(s.id);
            }
        }
        self.seen.insert(seen_key(&s.word, &s.meaning));
        self.by_id.insert(s.id, s);
    }
}

/// 「同じ内容の語」の判定に使う組。見出し語は、検索・登録画面の重複の知らせと同じ規則でそろえる
/// (「ﾉﾙﾊﾞｽｸ」と「ノルバスク」は同じ見出し語)。意味は、改行の書き方だけそろえる
fn seen_key(word: &str, meaning: &str) -> (String, String) {
    (core::word_key(word), meaning.replace("\r\n", "\n").trim().to_string())
}

/// 行の内容が、辞書の語と同じか(ファイルに列の無い項目は比べない)
fn same_content(ex: &Snapshot, r: &Row) -> bool {
    ex.word == r.word
        && ex.meaning.replace("\r\n", "\n") == r.meaning
        && (!r.given.tags || ex.tags == r.tags)
        && (!r.given.link || ex.link.replace("\r\n", "\n") == r.link)
        && (!r.given.reading || ex.reading == r.reading)
}

/// 行を辞書に取り込む。`dry` が真なら何も書かず、結果の見込みだけを返す。
/// トランザクションは呼び出し側で管理する(実行するときは、列の移行 `core::migrate` が済んでいること)。
///
/// 上書き(`Mode::Overwrite`)での照合:
/// 1. 行に管理用の番号があり、その番号の語が辞書にある
///    - 内容が同じ → 変更なし
///    - 辞書の語の更新日時が、書き出した時点のまま → 上書き(見出し語を直していても、その語に当たる)
///    - 更新日時が変わっている → 書き出した後にだれかが変更した語。上書きしない
/// 2. 番号が無い、またはその番号の語が辞書に無い → 見出し語(完全一致)で照合する
///    - 辞書に無い → 追加
///    - 辞書にもファイルにも、その見出し語が1件だけ → 内容が同じなら変更なし、違えば上書き
///    - どちらかに複数ある → どれを上書きするか決められないので、「両方残す」の規則で扱う
///
/// 上書きした語は id を変えず、更新日時を取り込んだ時刻にする(編集の衝突を更新日時で検知しているため、
/// 内容を変えたら必ず更新日時も変える)。
pub fn import_rows(conn: &Connection, rows: &[Row], mode: Mode, dry: bool) -> rusqlite::Result<(Outcome, Undo)> {
    let mut book = Book::load(conn)?;
    let mut out = Outcome::default();
    let mut undo = Undo::default();
    let now = core::now_unix();

    // ファイルの中での、見出し語ごとの行数
    let mut in_file: HashMap<String, usize> = HashMap::new();
    for r in rows {
        *in_file.entry(core::word_key(&r.word)).or_default() += 1;
    }
    // 取り込む前から辞書にあった語
    let original: HashSet<i64> = book.by_id.keys().copied().collect();
    // 管理用の番号が指す語(取り込む前から辞書にあった語だけ)。番号も更新日時も見出し語も違う語は、
    // 別の辞書から書き出した行の番号が偶然一致しただけとみなし、番号では照合しない
    let owner = |book: &Book, r: &Row| -> Option<Snapshot> {
        let (id, ts) = r.key?;
        let ex = book.by_id.get(&id).filter(|_| original.contains(&id))?;
        (ex.updated_at == ts || core::word_key(&ex.word) == core::word_key(&r.word)).then(|| ex.clone())
    };
    let claimed: HashSet<i64> = rows.iter().filter_map(|r| owner(&book, r)).map(|ex| ex.id).collect();

    // 「両方残す」の規則: 見出し語と意味が両方同じ語が無ければ、追加する
    let add = |book: &mut Book, out: &mut Outcome, undo: &mut Undo, r: &Row| -> rusqlite::Result<()> {
        if book.seen.contains(&seen_key(&r.word, &r.meaning)) {
            out.duplicate += 1;
            return Ok(());
        }
        let ts = r.updated_at.unwrap_or(now);
        let id = if dry {
            book.next_fake -= 1;
            book.next_fake
        } else {
            let f = core::Fields { word: r.word.clone(), reading: r.reading.clone(), meaning: r.meaning.clone(), tags: r.tags.clone(), link: r.link.clone() };
            let e = core::insert_entry(conn, &f, Some(ts))?;
            undo.inserted.push((e.id, ts));
            e.id
        };
        book.put(Snapshot { id, word: r.word.clone(), reading: r.reading.clone(), meaning: r.meaning.clone(), tags: r.tags.clone(), link: r.link.clone(), updated_at: ts });
        out.added += 1;
        Ok(())
    };

    for r in rows {
        match mode {
            Mode::KeepBoth => add(&mut book, &mut out, &mut undo, r)?,
            Mode::Skip => {
                // 取り込む前から辞書にあった見出し語だけを見る(同じファイルの中の、同じ見出し語の2行目以降は、取り込む)
                let by_key = owner(&book, r).is_some();
                let by_word = book.by_word.get(&core::word_key(&r.word)).map_or(false, |v| v.iter().any(|id| original.contains(id)));
                if by_key || by_word {
                    out.skipped += 1;
                } else {
                    add(&mut book, &mut out, &mut undo, r)?;
                }
            }
            Mode::Overwrite => {
                // 上書きする相手を決める
                let mut target: Option<i64> = None;
                let mut by_word = true;
                // 番号で照合するのは、取り込む前から辞書にあった語だけ(この取り込みで追加した語に、
                // 番号が偶然一致して上書きしないように。owner を参照)
                if let Some(ex) = owner(&book, r) {
                    let ts = r.key.map_or(0, |k| k.1);
                    by_word = false;
                    if same_content(&ex, r) {
                        out.unchanged += 1;
                    } else if ex.updated_at == ts {
                        target = Some(ex.id);
                    } else {
                        out.conflicts.push(ex.word.clone());
                    }
                }
                if by_word {
                    // その見出し語の語のうち、取り込む前から辞書にあったもの
                    let olds: Vec<i64> = book.by_word.get(&core::word_key(&r.word)).map(|v| v.iter().copied().filter(|id| original.contains(id)).collect()).unwrap_or_default();
                    let single = olds.len() == 1 && !claimed.contains(&olds[0]) && in_file.get(&core::word_key(&r.word)) == Some(&1);
                    if olds.is_empty() {
                        // 辞書に無かった見出し語(この取り込みで追加した語と同じ見出し語の行も、追加する)
                        add(&mut book, &mut out, &mut undo, r)?;
                    } else if single {
                        if same_content(&book.by_id[&olds[0]], r) {
                            out.unchanged += 1;
                        } else {
                            target = Some(olds[0]);
                        }
                    } else {
                        if !out.ambiguous.contains(&r.word) {
                            out.ambiguous.push(r.word.clone());
                        }
                        add(&mut book, &mut out, &mut undo, r)?;
                    }
                }
                if let Some(id) = target {
                    let ex = book.by_id[&id].clone();
                    // ファイルに列の無い項目は、今の値を残す
                    let new = Snapshot {
                        id,
                        word: r.word.clone(),
                        meaning: r.meaning.clone(),
                        tags: if r.given.tags { r.tags.clone() } else { ex.tags.clone() },
                        link: if r.given.link { r.link.clone() } else { ex.link.clone() },
                        reading: if r.given.reading { r.reading.clone() } else { ex.reading.clone() },
                        updated_at: now.max(ex.updated_at + 1),
                    };
                    if !dry {
                        conn.execute(
                            "UPDATE words SET word = ?1, reading = ?2, meaning = ?3, tags = ?4, link = ?5, updated_at = ?6 WHERE id = ?7",
                            params![new.word, new.reading, new.meaning, new.tags, new.link, new.updated_at, id],
                        )?;
                        undo.updated.push((ex.clone(), new.updated_at));
                    }
                    for (name, a, b) in [("見出し語", &ex.word, &new.word), ("読み", &ex.reading, &new.reading), ("意味", &ex.meaning, &new.meaning), ("タグ", &ex.tags, &new.tags), ("参照先", &ex.link, &new.link)] {
                        if let Some(d) = excel_damage(a, b) {
                            out.suspicious.push((new.word.clone(), format!("{name} {d}")));
                        }
                    }
                    out.diffs.push(diff_lines(&ex, &new));
                    out.changes.push((ex.word.clone(), new.word.clone()));
                    out.overwritten += 1;
                    book.replace(new);
                }
            }
        }
    }
    Ok((out, undo))
}

/// 行を登録する(「両方残す」の規則)。見出し語と意味が両方同じものは重複として飛ばす(同じファイルを2回取り込んでも増えない)。
/// 戻り値は (追加した件数, 重複で飛ばした件数)。トランザクションは呼び出し側で管理する。
#[cfg_attr(not(test), allow(dead_code))]
pub fn insert_rows(conn: &Connection, rows: &[Row]) -> rusqlite::Result<(usize, usize)> {
    let (out, _) = import_rows(conn, rows, Mode::KeepBoth, false)?;
    Ok((out.added, out.duplicate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_and_skip_rules() {
        let (conn, dir) = open("fold");
        import_rows(&conn, &[row("ノルバスク錠5mg", "降圧薬")], Mode::KeepBoth, false).unwrap();
        // 半角カナ・全角英数で書いた同じ見出し語も、すでにあるとみなす
        let (o, _) = import_rows(&conn, &[row("ﾉﾙﾊﾞｽｸ錠５ｍｇ", "降圧薬"), row("アムロジピン", "降圧薬")], Mode::Skip, true).unwrap();
        assert_eq!((o.skipped, o.added), (1, 1));
        // 両方残す: 同じ見出し語(そろえて同じ)と同じ意味は、追加しない
        let (o, _) = import_rows(&conn, &[row("ﾉﾙﾊﾞｽｸ錠5mg", "降圧薬")], Mode::KeepBoth, true).unwrap();
        assert_eq!((o.duplicate, o.added), (1, 0));
        // 取り込まない: 同じファイルの中で、辞書に無い見出し語が2回出てきても、両方取り込む
        let (o, _) = import_rows(&conn, &[row("新薬", "説明1"), row("新薬", "説明2")], Mode::Skip, true).unwrap();
        assert_eq!((o.skipped, o.added), (0, 2));
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn column_names_and_notes() {
        // 訳語・原語の表。知らない列(備考)は使わず、そのことを知らせる
        let c = to_rows_detail(parse_table("原語,訳語,分野,備考\nliability,責任,法務,注意\n", ','));
        assert_eq!((c.rows[0].word.as_str(), c.rows[0].meaning.as_str(), c.rows[0].tags.as_str()), ("liability", "責任", "法務"));
        assert!(c.notes[0].contains("「備考」"), "{:?}", c.notes);
        // 見出し語の列名だけ分かる表: 意味は、残りの列の左から
        let c = to_rows_detail(parse_table("Term\tNotes\nAPI\tインターフェース\n", '\t'));
        assert_eq!((c.rows.len(), c.rows[0].meaning.as_str()), (1, "インターフェース"));
        // 列名が無い表は、そのことを知らせる
        let c = to_rows_detail(parse_table("a,b\n", ','));
        assert!(c.notes[0].contains("列名が無い"));
        // 閉じていない引用符
        let c = to_rows_detail(parse_table("見出し語,意味\na,\"説明\nb,続き\n", ','));
        assert_eq!(c.rows.len(), 1);
        assert!(c.notes.iter().any(|n| n.contains("2 行目の引用符")), "{:?}", c.notes);
        // TSV の参照先では、\n を改行にしない(共有フォルダのパスを壊さない)
        let c = to_rows_detail(parse_table("見出し語\t意味\t参照先\nx\ta\\nb\t\\\\fs01\\netadmin\n", '\t'));
        assert_eq!(c.rows[0].meaning, "a\nb");
        assert_eq!(c.rows[0].link, "\\\\fs01\\netadmin");
    }

    #[test]
    fn screening() {
        let long = "あ".repeat(core::MAX_MEANING + 1);
        let rows = vec![row("ok", "普通"), row("長い", &long), row("制御\u{202E}文字\u{0}", "a\u{7}b")];
        let (rows, long, cleaned) = screen(rows);
        assert_eq!(rows.len(), 2);
        assert_eq!(long, vec!["長い".to_string()]);
        assert_eq!(cleaned, 1);
        assert_eq!((rows[1].word.as_str(), rows[1].meaning.as_str()), ("制御文字", "ab"));
    }

    #[test]
    fn spreadsheet_damage() {
        assert!(excel_damage("=A1", "#NAME?").is_some());
        assert!(excel_damage("", "#VALUE!").is_some());
        assert!(excel_damage("abc", "#####").is_some());
        assert!(excel_damage("0123", "123").is_some());
        assert!(excel_damage("123456789012", "1.23457E+11").is_some());
        assert!(excel_damage("123", "124").is_none());
        assert!(excel_damage("#タグ", "#タグ2").is_none());
        // 上書きの見込みに出る
        let (conn, dir) = open("damage");
        import_rows(&conn, &[Row { reading: "0123".into(), given: Given { reading: true, ..Default::default() }, ..row("社員番号", "説明") }], Mode::KeepBoth, false).unwrap();
        let r = Row { reading: "123".into(), given: Given { reading: true, ..Default::default() }, ..row("社員番号", "説明") };
        let (o, _) = import_rows(&conn, &[r], Mode::Overwrite, true).unwrap();
        assert_eq!(o.overwritten, 1);
        assert_eq!(o.suspicious.len(), 1);
        assert!(overwrite_details(&o).contains("先頭の 0"));
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn move_json_between_dictionaries() {
        // 辞書 A を JSON に書き出し、空の辞書 B に「上書き」で取り込む: すべて追加され、消える語は無い
        let (a, dir_a) = open("move-a");
        let f = |w: &str| core::Fields { word: w.into(), meaning: format!("{w} の意味"), ..Default::default() };
        for (i, w) in ["one", "two", "three", "four"].iter().enumerate() {
            core::insert_entry(&a, &f(w), Some(1000 + i as i64)).unwrap();
        }
        let (rows, _) = crate::json_io::parse(&crate::json_io::export(&sorted(&a))).unwrap();
        let (b, dir_b) = open("move-b");
        let (plan, _) = import_rows(&b, &rows, Mode::Overwrite, true).unwrap();
        let (done, _) = import_rows(&b, &rows, Mode::Overwrite, false).unwrap();
        assert_eq!(done, plan); // 見込みと結果が同じ
        assert_eq!((done.added, done.overwritten), (4, 0));
        assert_eq!(sorted(&b).len(), 4);

        // 番号が同じでも、更新日時も見出し語も違う語(別の辞書の語)には当てない
        let (c, dir_c) = open("move-c");
        for w in ["alpha", "beta"] {
            core::insert_entry(&c, &f(w), Some(5)).unwrap();
        }
        let (o, _) = import_rows(&c, &rows, Mode::Overwrite, false).unwrap();
        assert_eq!((o.added, o.overwritten, o.conflicts.len()), (4, 0, 0));
        let (o, _) = import_rows(&c, &rows, Mode::Skip, true).unwrap();
        assert_eq!((o.skipped, o.added), (4, 0)); // いま追加したので、すべて取り込み済み
        let (d, dir_d) = open("move-d");
        core::insert_entry(&d, &f("alpha"), Some(5)).unwrap();
        let (o, _) = import_rows(&d, &rows, Mode::Skip, true).unwrap();
        assert_eq!((o.skipped, o.added), (0, 4));
        for (conn, dir) in [(a, dir_a), (b, dir_b), (c, dir_c), (d, dir_d)] {
            drop(conn);
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn letter_case_is_kept_when_matching() {
        // pH と PH は別の語(全角の ｐＨ は pH と同じ語)
        let (conn, dir) = open("case");
        let f = |w: &str, m: &str| core::Fields { word: w.into(), meaning: m.into(), ..Default::default() };
        core::insert_entry(&conn, &f("pH", "水素イオン指数"), Some(10)).unwrap();
        let (o, _) = import_rows(&conn, &[row("PH", "肺高血圧症")], Mode::Overwrite, true).unwrap();
        assert_eq!((o.added, o.overwritten), (1, 0));
        let (o, _) = import_rows(&conn, &[row("PH", "肺高血圧症")], Mode::Skip, true).unwrap();
        assert_eq!((o.added, o.skipped), (1, 0));
        let (o, _) = import_rows(&conn, &[row("ｐＨ", "水素イオン指数(血液ガス)")], Mode::Overwrite, true).unwrap();
        assert_eq!((o.added, o.overwritten), (0, 1));
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn row(word: &str, meaning: &str) -> Row {
        Row { word: word.into(), meaning: meaning.into(), ..Default::default() }
    }

    #[test]
    fn csv_with_quotes_and_header() {
        let text = "タグ,見出し語,意味,URL\r\n\"fruit, red\",apple,\"りんご。\n\"\"赤い\"\"果物\",https://example.com/\r\n,,空の行は飛ばす,\r\n\r\nx,banana,バナナ,\r\n";
        let (rows, skipped) = to_rows(parse_table(text, ','));
        assert_eq!(skipped, 1);
        // 見出し行にある列(タグ・参照先)は「列がある」、無い列(読み)は「無い」
        let given = Given { tags: true, link: true, reading: false };
        assert_eq!(
            rows,
            vec![
                Row { word: "apple".into(), meaning: "りんご。\n\"赤い\"果物".into(), tags: "fruit red".into(), link: "https://example.com/".into(), given, ..Default::default() },
                Row { word: "banana".into(), meaning: "バナナ".into(), tags: "x".into(), given, ..Default::default() },
            ]
        );
    }

    #[test]
    fn tsv_without_header_is_positional() {
        let text = "apple\tりんご\\n果物\t#fruit\nbanana\tバナナ\n";
        let (rows, skipped) = to_rows(parse_table(text, '\t'));
        assert_eq!(skipped, 0);
        // 見出し行が無い表は、どこかの行にある列までを「列がある」とみなす(ここでは3列目のタグまで)
        let given = Given { tags: true, link: false, reading: false };
        assert_eq!(rows[0], Row { word: "apple".into(), meaning: "りんご\n果物".into(), tags: "fruit".into(), given, ..Default::default() });
        // 読みの列(列名で指定)
        let (named, _) = to_rows(parse_table("読み\t見出し語\t意味\nりんご\t林檎\t果物\n", '\t'));
        assert_eq!((named[0].word.as_str(), named[0].reading.as_str()), ("林檎", "りんご"));
        assert_eq!(named[0].given, Given { tags: false, link: false, reading: true });
        assert_eq!(rows[1].word, "banana");
        assert_eq!(rows[1].tags, "");
    }

    #[test]
    fn backslash_n_is_newline_only_in_unquoted_tsv() {
        // タブ区切りで "..." で囲んでいない値だけ、`\n` を改行として扱う
        let (r, _) = to_rows(parse_table("a\t1行目\\n2行目\nb\t\"改行は \\n と書く\"\n", '\t'));
        assert_eq!(r[0].meaning, "1行目\n2行目");
        assert_eq!(r[1].meaning, "改行は \\n と書く");
        // CSV では、書いたとおりに扱う
        let (r, _) = to_rows(parse_table("a,改行は \\n と書く\n", ','));
        assert_eq!(r[0].meaning, "改行は \\n と書く");
        // パスの中の `\n` は改行にしない(画像の記法の括弧の中、共有フォルダ・ドライブで始まる語)
        let (r, _) = to_rows(parse_table("a\t写真\\n![海苔](dictionary_pic\\nori.png)\\n置き場は \\\\fs01\\netadmin です\n", '\t'));
        assert_eq!(r[0].meaning, "写真\n![海苔](dictionary_pic\\nori.png)\n置き場は \\\\fs01\\netadmin です");
        // 参照先の2行目が、`//` の無いスキーム(ms-settings:)でも改行にする
        let (r, _) = to_rows(parse_table("見出し語\t意味\t参照先\na\tb\thttps://example.com/\\nms-settings:display\n", '\t'));
        assert_eq!(r[0].link, "https://example.com/\nms-settings:display");
        // 引用符で囲んだ意味は、前後の空白・改行を削らない(書き出した表を取り込み直しても変わらない)
        let (r, _) = to_rows(parse_table("a,\"  前後に空白 \n\"\n", ','));
        assert_eq!(r[0].meaning, "  前後に空白 \n");
    }

    #[test]
    fn diff_shows_the_changed_part() {
        let long = "あ".repeat(80);
        let d = brief_diff(&format!("{long}10〜20"), &format!("{long}10～20"));
        assert!(d.contains("10〜20") && d.contains("10～20") && d.starts_with("「…"), "{d}");
        assert!(d.contains("U+301C → U+FF5E"), "{d}");
    }

    #[test]
    fn unclosed_quote_line_with_crlf() {
        let t = parse_table("a,1\r\nb,2\r\nc,\"閉じない\r\n", ',');
        assert_eq!(t.unclosed_quote, Some(3));
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

    fn open(name: &str) -> (Connection, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("MyDictionary-import-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let conn = core::open(&dir.join("d.db")).unwrap();
        core::migrate(&conn).unwrap();
        (conn, dir)
    }

    fn sorted(conn: &Connection) -> Vec<Entry> {
        let mut v = core::load_all(conn).unwrap();
        v.sort_by_key(|e| e.id);
        v
    }

    #[test]
    fn insert_skips_duplicates() {
        let (conn, dir) = open("dup");
        let (rows, _) = to_rows(parse_table("a,1\nb,2\na,1\na,別の意味\n", ','));
        assert_eq!(insert_rows(&conn, &rows).unwrap(), (3, 1)); // 同じ「a,1」は1回だけ
        assert_eq!(insert_rows(&conn, &rows).unwrap(), (0, 4)); // 取り込み直しても増えない
        assert_eq!(core::load_all(&conn).unwrap().len(), 3);
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 書き出して、そのまま取り込むと、内容が元に戻る(CSV・TSV とも)
    #[test]
    fn export_roundtrip() {
        let src = vec![
            Entry::new(1, "一期\"一会\"".into(), "1行目\n2行目\t, と \\n と「引用」".into(), "四字熟語 名言".into(), 1_730_000_000)
                .with_reading("いちごいちえ".into())
                .with_link("公式 | https://example.com/\n[[温故知新]]".into()),
            Entry::new(2, "apple".into(), "りんご".into(), "".into(), 5),
            Entry::new(3, "-fno-rtti".into(), "=1+1 のような値も、そのまま".into(), "IT".into(), 7),
        ];
        for delim in [',', '\t'] {
            let text = export_table(&src, delim);
            assert!(text.starts_with('\u{feff}'));
            assert!(text.contains("更新日時") && text.contains("2024/10/27 12:33:20"));
            let decoded = decode_unicode(text.as_bytes()).unwrap();
            let (rows, skipped) = to_rows(parse_table(&decoded, delim));
            assert_eq!((rows.len(), skipped), (3, 0), "{delim:?}");
            for (r, e) in rows.iter().zip(&src) {
                assert_eq!((&r.word, &r.reading, &r.meaning, &r.tags, &r.link), (&e.word, &e.reading, &e.meaning, &e.tags, &e.link), "{delim:?}");
                assert_eq!(r.key, Some((e.id, e.updated_at)));
                assert_eq!(r.given, Given { tags: true, link: true, reading: true });
            }
        }
        // 管理用の列の読み取り
        assert_eq!(parse_key(" 12_1730000000 "), Some((12, 1_730_000_000)));
        assert_eq!(parse_key("12"), None);
        assert_eq!(parse_key("x_1"), None);
        assert_eq!(parse_key("0_1"), None);
        assert_eq!(header_kind("管理用(変更しない)"), Some(5));
        assert_eq!(header_kind(" 管理用(メモ) "), Some(5));
        assert_eq!(header_kind("更新日時"), None);
    }

    /// 書き出し → 表を直す → 上書きで取り込む
    #[test]
    fn overwrite_by_key() {
        let (conn, dir) = open("key");
        let f = |w: &str, m: &str, t: &str, rd: &str| core::Fields { word: w.into(), meaning: m.into(), tags: t.into(), reading: rd.into(), link: String::new() };
        let a = core::insert_entry(&conn, &f("apple", "りんご", "果物", "あっぷる"), Some(100)).unwrap();
        let b = core::insert_entry(&conn, &f("banana", "バナナ", "果物", ""), Some(200)).unwrap();
        let c = core::insert_entry(&conn, &f("carot", "にんじん", "野菜", ""), Some(300)).unwrap();
        let d = core::insert_entry(&conn, &f("apple", "アップル社", "会社", ""), Some(400)).unwrap(); // 同じ見出し語がもう1つ
        let exported = export_table(&sorted(&conn), ',');
        let decoded = decode_unicode(exported.as_bytes()).unwrap();

        // 書き出した後、だれかが banana を編集した
        let ts_b = core::update(&conn, b.id, b.updated_at, &f("banana", "バナナ(ほかの人が変更)", "果物", "")).unwrap();

        // 表を直す: apple(1つ目)の意味、banana の意味、carot の見出し語の誤字。タグを空にする。行を1つ足す
        let edited = decoded
            .replace("りんご,果物", "りんご。赤い果物,") // 意味を変え、タグを空に
            .replace("banana,,バナナ", "banana,,バナナ(表で変更)")
            .replace("carot,", "carrot,")
            + "date,,なつめやし,果物,,,\r\n";
        let (rows, _) = to_rows(parse_table(&edited, ','));
        assert_eq!(rows.len(), 5);

        // 見込み: 何も書かない
        let (plan, undo) = import_rows(&conn, &rows, Mode::Overwrite, true).unwrap();
        assert!(undo.is_empty());
        assert_eq!((plan.added, plan.overwritten, plan.unchanged, plan.duplicate), (1, 2, 1, 0));
        assert_eq!(plan.conflicts, vec!["banana".to_string()]);
        assert_eq!(plan.changes, vec![("apple".to_string(), "apple".to_string()), ("carot".to_string(), "carrot".to_string())]);
        assert_eq!(plan.summary(), "追加 1 件、上書き 2 件、変更なし 1 件、書き出した後に変更されたため上書きしない 1 件");
        assert_eq!(plan.writes(), 3);
        assert_eq!(
            overwrite_details(&plan),
            "■ 上書きする語(2 件)\n・apple\n    意味: 「りんご」 → 「りんご。赤い果物」\n    タグ: 「果物」 → 空にする\n・carot → carrot\n    見出し語: 「carot」 → 「carrot」\n\n■ 書き出した後に変更されたため、上書きしない語(1 件)\n・banana\n"
        );
        assert_eq!(overwrite_details(&Outcome::default()), "");
        assert_eq!(sorted(&conn).len(), 4);

        // 実行: 見込みと同じ結果になる
        let (done, undo) = import_rows(&conn, &rows, Mode::Overwrite, false).unwrap();
        assert_eq!(done, plan);
        let now = sorted(&conn);
        assert_eq!(now.len(), 5);
        let get = |id: i64| now.iter().find(|e| e.id == id).unwrap();
        // apple(1つ目): 同じ見出し語が2つあっても、番号で正しい語に当たる。列があって空のタグは、空になる。読みはそのまま
        assert_eq!((get(a.id).meaning.as_str(), get(a.id).tags.as_str(), get(a.id).reading.as_str()), ("りんご。赤い果物", "", "あっぷる"));
        assert!(get(a.id).updated_at > a.updated_at); // 内容を変えたら、更新日時も必ず変える
        // banana: ほかの人の変更を残す
        assert_eq!((get(b.id).meaning.as_str(), get(b.id).updated_at), ("バナナ(ほかの人が変更)", ts_b));
        // carot → carrot: 見出し語を直しても、同じ語(同じ id)が書き換わる。新しい語は増えない
        assert_eq!(get(c.id).word, "carrot");
        // apple(2つ目): 変えていないので、そのまま(更新日時も変わらない)
        assert_eq!((get(d.id).meaning.as_str(), get(d.id).updated_at), ("アップル社", 400));
        // 足した行は追加される
        assert!(now.iter().any(|e| e.word == "date"));
        assert_eq!((undo.inserted.len(), undo.updated.len(), undo.deleted.len()), (1, 2, 0));

        // 同じ表をもう一度取り込んでも、変わらない(上書きした語も、足した行も、内容が同じなので「変更なし」)
        let (again, undo2) = import_rows(&conn, &rows, Mode::Overwrite, false).unwrap();
        assert_eq!((again.added, again.overwritten, again.unchanged, again.duplicate), (0, 0, 4, 0));
        assert_eq!(again.conflicts, vec!["banana".to_string()]);
        assert!(undo2.is_empty());

        // 元に戻す: 取り込む前の状態になる
        assert_eq!(crate::undo::revert(&conn, &undo).map(|r| (r.done, r.skipped)).unwrap(), (3, 0));
        let back = sorted(&conn);
        assert_eq!(back.len(), 4);
        let get = |id: i64| back.iter().find(|e| e.id == id).unwrap();
        assert_eq!((get(a.id).meaning.as_str(), get(a.id).tags.as_str(), get(a.id).updated_at), ("りんご", "果物", 100));
        assert_eq!((get(c.id).word.as_str(), get(c.id).updated_at), ("carot", 300));
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 管理用の列が無い表(手で作った表)は、見出し語で照合する
    #[test]
    fn overwrite_by_headword() {
        let (conn, dir) = open("word");
        let f = |w: &str, m: &str, t: &str, rd: &str| core::Fields { word: w.into(), meaning: m.into(), tags: t.into(), reading: rd.into(), link: "https://example.com/".into() };
        let a = core::insert_entry(&conn, &f("apple", "りんご", "果物", "あっぷる"), Some(100)).unwrap();
        core::insert_entry(&conn, &f("line", "線", "", ""), Some(200)).unwrap();
        core::insert_entry(&conn, &f("line", "行列", "", ""), Some(300)).unwrap(); // 辞書に同じ見出し語が2つ
        let c = core::insert_entry(&conn, &f("cat", "ねこ", "動物", ""), Some(400)).unwrap();

        // 見出し語と意味だけの表(タグ・参照先・読みの列は無い)
        let text = "見出し語,意味\napple,りんご。赤い果物\nline,回線\ncat,ねこ\ndog,いぬ\nbird,とり\nbird,鳥類\n";
        let (rows, _) = to_rows(parse_table(text, ','));
        let (out, undo) = import_rows(&conn, &rows, Mode::Overwrite, false).unwrap();
        // apple: 上書き。line: 辞書に2つあるので追加。cat: 同じ内容。dog: 追加。bird: ファイルに2つあるが辞書に無いので、どちらも追加
        assert_eq!((out.added, out.overwritten, out.unchanged, out.duplicate), (4, 1, 1, 0));
        assert_eq!(out.ambiguous, vec!["line".to_string()]);
        assert!(out.conflicts.is_empty());
        let now = sorted(&conn);
        let ap = now.iter().find(|e| e.id == a.id).unwrap();
        // 列の無い項目(タグ・参照先・読み)は、今の値を残す
        assert_eq!((ap.meaning.as_str(), ap.tags.as_str(), ap.reading.as_str(), ap.link.as_str()), ("りんご。赤い果物", "果物", "あっぷる", "https://example.com/"));
        assert_eq!(now.iter().filter(|e| e.word == "line").count(), 3);
        assert_eq!(now.iter().find(|e| e.id == c.id).unwrap().updated_at, 400);
        assert_eq!((undo.inserted.len(), undo.updated.len()), (4, 1));

        // 飛ばす: 見出し語がすでにある行は取り込まない
        let text = "見出し語,意味\napple,別の意味\nline,さらに別\nfox,きつね\n";
        let (rows, _) = to_rows(parse_table(text, ','));
        let (out, _) = import_rows(&conn, &rows, Mode::Skip, false).unwrap();
        assert_eq!((out.added, out.skipped, out.overwritten), (1, 2, 0));
        assert_eq!(out.summary(), "追加 1 件、見出し語があるため取り込まない 2 件");

        // 両方残す(これまでの動作): 意味が違えば、同じ見出し語でも追加する
        let (out, _) = import_rows(&conn, &rows, Mode::KeepBoth, false).unwrap();
        assert_eq!((out.added, out.duplicate, out.skipped), (2, 1, 0));
        assert_eq!(import_rows(&conn, &[], Mode::Overwrite, true).unwrap().0.summary(), "取り込む語はありません");

        // 番号で指された語は、見出し語での照合の相手にしない(行の順番で結果が変わらないように)
        let (conn2, dir2) = open("claimed");
        let e = core::insert_entry(&conn2, &f("old", "意味", "", ""), Some(100)).unwrap();
        let keyed = Row { word: "new".into(), meaning: "意味".into(), key: Some((e.id, 100)), ..Default::default() };
        for rows in [vec![row("old", "別の語"), keyed.clone()], vec![keyed.clone(), row("old", "別の語")]] {
            let (out, _) = import_rows(&conn2, &rows, Mode::Overwrite, true).unwrap();
            assert_eq!((out.added, out.overwritten), (1, 1));
            assert_eq!(out.changes, vec![("old".to_string(), "new".to_string())]);
        }
        drop((conn, conn2));
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dir2);
    }
}
