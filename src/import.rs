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
}

/// 区切りテキストを行×列に分解する。空行は捨てる
pub fn parse_table(text: &str, delim: char) -> Table {
    let tsv = delim == '\t';
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut escapes: Vec<Vec<bool>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut esc: Vec<bool> = Vec::new();
    let mut cell = String::new();
    let mut quoted = false; // 引用符の中
    let mut was_quoted = false; // このセルは引用符で始まった
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
            was_quoted = true;
            at_start = false;
        } else if c == delim {
            row.push(std::mem::take(&mut cell));
            esc.push(tsv && !was_quoted);
            was_quoted = false;
            at_start = true;
        } else if c == '\n' || c == '\r' {
            if c == '\r' && it.peek() == Some(&'\n') {
                it.next();
            }
            row.push(std::mem::take(&mut cell));
            esc.push(tsv && !was_quoted);
            was_quoted = false;
            if row.iter().any(|x| !x.trim().is_empty()) {
                rows.push(std::mem::take(&mut row));
                escapes.push(std::mem::take(&mut esc));
            } else {
                row.clear();
                esc.clear();
            }
            at_start = true;
        } else {
            cell.push(c);
            at_start = false;
        }
    }
    row.push(cell);
    esc.push(tsv && !was_quoted);
    if row.iter().any(|x| !x.trim().is_empty()) {
        rows.push(row);
        escapes.push(esc);
    }
    Table { rows, escapes }
}

/// 列名 → 列の種類(0: 見出し語, 1: 意味, 2: タグ, 3: 参照先, 4: 読み, 5: 管理用)。
/// 「管理用(変更しない)」のような、括弧の注意書きは無視する
fn header_kind(name: &str) -> Option<usize> {
    let n = name.trim().trim_start_matches('\u{feff}').to_lowercase();
    let n = n.split(['(', '(']).next().unwrap_or("").trim();
    const NAMES: [&[&str]; 6] = [
        &["見出し語", "見出し", "単語", "語", "word", "term", "headword"],
        &["意味", "説明", "meaning", "definition", "description"],
        &["タグ", "tag", "tags"],
        &["参照先", "リンク", "link", "links", "url"],
        &["読み", "よみ", "読み方", "ふりがな", "reading", "yomi", "kana"],
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

/// 表を登録用の行に変換する。戻り値は (行, 見出し語か意味が空で読み飛ばした件数)
pub fn to_rows(table: Table) -> (Vec<Row>, usize) {
    let Table { rows: cells, escapes } = table;
    // 列の対応: [見出し語, 意味, タグ, 参照先, 読み, 管理用] がそれぞれ何列目か
    let mut cols: [Option<usize>; 6] = [Some(0), Some(1), Some(2), Some(3), Some(4), None];
    let mut start = 0;
    let header = cells.first().map(|first| first.iter().map(|c| header_kind(c)).collect::<Vec<_>>());
    let given = match header {
        Some(kinds) if kinds.contains(&Some(0)) && kinds.contains(&Some(1)) => {
            cols = [None; 6];
            for (i, k) in kinds.iter().enumerate() {
                if let Some(k) = k {
                    cols[*k].get_or_insert(i);
                }
            }
            start = 1; // 見出し行は登録しない
            Given { tags: cols[2].is_some(), link: cols[3].is_some(), reading: cols[4].is_some() }
        }
        _ => {
            // 見出し行が無い表: どこかの行にその列があれば、「列がある」とみなす
            let width = cells.iter().map(Vec::len).max().unwrap_or(0);
            Given { tags: width > 2, link: width > 3, reading: width > 4 }
        }
    };
    let mut rows = Vec::new();
    let mut skipped = 0;
    for (r, esc) in cells.iter().zip(&escapes).skip(start) {
        let get = |k: usize| cols[k].and_then(|i| r.get(i)).map(|s| s.trim().to_string()).unwrap_or_default();
        // 意味と参照先は改行を含められる。タブ区切りで "..." で囲んでいない値は、`\n` を改行として扱う
        let multiline = |k: usize| {
            let v = get(k);
            let escaped = cols[k].and_then(|i| esc.get(i)).copied().unwrap_or(false);
            let v = if escaped { v.replace("\\r\\n", "\n").replace("\\n", "\n") } else { v };
            v.replace("\r\n", "\n").replace('\r', "\n")
        };
        let row = Row {
            word: get(0),
            meaning: multiline(1),
            tags: core::normalize_tags(&get(2)),
            link: multiline(3),
            reading: get(4),
            updated_at: None,
            given,
            key: parse_key(&get(5)),
        };
        if row.word.is_empty() || row.meaning.is_empty() {
            skipped += 1;
        } else {
            rows.push(row);
        }
    }
    (rows, skipped)
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

/// 上書きの見込みの詳細(確認画面の「詳細」と、結果の画面に出す)。何も無ければ空
pub fn overwrite_details(plan: &Outcome) -> String {
    const MAX: usize = 8;
    let mut out = String::new();
    let mut section = |title: &str, items: Vec<String>| {
        if items.is_empty() {
            return;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("{title}({} 件)\n", items.len()));
        for it in items.iter().take(MAX) {
            out.push_str(&format!("  ・{it}\n"));
        }
        if items.len() > MAX {
            out.push_str(&format!("  …ほか {} 件\n", items.len() - MAX));
        }
    };
    section("上書きする語", plan.changes.iter().map(|(a, b)| if a == b { a.clone() } else { format!("{a} → {b}") }).collect());
    section("書き出した後に変更されたため、上書きしない語", plan.conflicts.clone());
    section("同じ見出し語が複数あるため、追加として扱う語", plan.ambiguous.clone());
    out
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
        self.seen.insert((s.word.clone(), s.meaning.clone()));
        self.by_word.entry(s.word.clone()).or_default().push(s.id);
        self.by_id.insert(s.id, s);
    }

    /// 語の内容を置き換える(見出し語が変わっていれば、見出し語の索引も直す)
    fn replace(&mut self, s: Snapshot) {
        if let Some(old) = self.by_id.get(&s.id) {
            if old.word != s.word {
                if let Some(v) = self.by_word.get_mut(&old.word) {
                    v.retain(|&i| i != s.id);
                }
                self.by_word.entry(s.word.clone()).or_default().push(s.id);
            }
        }
        self.seen.insert((s.word.clone(), s.meaning.clone()));
        self.by_id.insert(s.id, s);
    }
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
    let mut in_file: HashMap<&str, usize> = HashMap::new();
    for r in rows {
        *in_file.entry(r.word.as_str()).or_default() += 1;
    }
    // 取り込む前から辞書にあった語
    let original: HashSet<i64> = book.by_id.keys().copied().collect();
    // 管理用の番号で指されている語。見出し語での照合の相手にはしない(その行が担当するため)
    let claimed: HashSet<i64> = rows.iter().filter_map(|r| r.key.map(|k| k.0)).filter(|id| book.by_id.contains_key(id)).collect();

    // 「両方残す」の規則: 見出し語と意味が両方同じ語が無ければ、追加する
    let add = |book: &mut Book, out: &mut Outcome, undo: &mut Undo, r: &Row| -> rusqlite::Result<()> {
        if book.seen.contains(&(r.word.clone(), r.meaning.clone())) {
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
                let by_key = r.key.map_or(false, |k| book.by_id.contains_key(&k.0));
                let by_word = book.by_word.get(&r.word).map_or(false, |v| !v.is_empty());
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
                if let Some((id, ts)) = r.key {
                    if let Some(ex) = book.by_id.get(&id) {
                        by_word = false;
                        if same_content(ex, r) {
                            out.unchanged += 1;
                        } else if ex.updated_at == ts {
                            target = Some(id);
                        } else {
                            out.conflicts.push(ex.word.clone());
                        }
                    }
                }
                if by_word {
                    // その見出し語の語のうち、取り込む前から辞書にあったもの
                    let olds: Vec<i64> = book.by_word.get(&r.word).map(|v| v.iter().copied().filter(|id| original.contains(id)).collect()).unwrap_or_default();
                    let single = olds.len() == 1 && !claimed.contains(&olds[0]) && in_file.get(r.word.as_str()) == Some(&1);
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
            "上書きする語(2 件)\n  ・apple\n  ・carot → carrot\n\n書き出した後に変更されたため、上書きしない語(1 件)\n  ・banana\n"
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
        assert_eq!(crate::undo::revert(&conn, &undo).unwrap(), crate::undo::Reverted { done: 3, skipped: 0 });
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
