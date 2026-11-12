//! 直前の操作を元に戻す(画面に依存しない部分)。
//!
//! 書き込みのときに、変える前の内容を `Undo` に控えておき、`revert` でその逆を行う。
//!
//! - 追加した語は、削除する
//! - 削除した語は、同じ id で入れ直す(`words.id` は AUTOINCREMENT なので、消した id が
//!   ほかの語に使い回されることはない。id が同じなので、しおりもそのまま有効になる)
//! - 書き換えた語は、前の内容に戻す(更新日時も前の値に戻す)
//!
//! 共有の辞書では、自分の操作のあとで、ほかの人が同じ語を変更・削除していることがある。
//! その語は戻さずに、件数だけを返す(編集の衝突と同じ考え方。更新日時が、自分の書いた値の
//! ままの語だけを戻す)。
//!
//! 更新日時を前の値に戻してよい理由: 「(id, 更新日時) が同じなら内容も同じ」という関係が
//! 保たれるため。自分の変更後の内容を読み込んでいた人は、更新日時が変わるので衝突として検知される。
//! 変更前の内容を読み込んでいた人は、同じ内容に対して編集することになる。

use crate::core::Entry;
use rusqlite::{params, Connection, OptionalExtension};

/// 語1つの全項目
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub id: i64,
    pub word: String,
    pub reading: String,
    pub meaning: String,
    pub tags: String,
    pub link: String,
    pub updated_at: i64,
}

impl Snapshot {
    pub fn of(e: &Entry) -> Snapshot {
        Snapshot { id: e.id, word: e.word.clone(), reading: e.reading.clone(), meaning: e.meaning.clone(), tags: e.tags.clone(), link: e.link.clone(), updated_at: e.updated_at }
    }
}

/// 1つの辞書への1回の書き込みを、元に戻すための記録
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Undo {
    /// 追加した語 (id, 書き込んだ更新日時)。元に戻すときは削除する
    pub inserted: Vec<(i64, i64)>,
    /// 削除した語の、消す前の内容。元に戻すときは、同じ id で入れ直す
    pub deleted: Vec<Snapshot>,
    /// 書き換えた語 (変える前の内容, 書き込んだ後の更新日時)。元に戻すときは、前の内容に戻す
    pub updated: Vec<(Snapshot, i64)>,
}

impl Undo {
    pub fn is_empty(&self) -> bool {
        self.inserted.is_empty() && self.deleted.is_empty() && self.updated.is_empty()
    }

    /// 元に戻す対象の語の数
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn len(&self) -> usize {
        self.inserted.len() + self.deleted.len() + self.updated.len()
    }
}

/// 元に戻した結果
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Reverted {
    /// 元に戻した語の数
    pub done: usize,
    /// ほかの人がその後に変更または削除していたので、戻さなかった語の数
    pub skipped: usize,
}

fn read(conn: &Connection, id: i64) -> rusqlite::Result<Option<Snapshot>> {
    conn.query_row("SELECT id, word, reading, meaning, tags, link, updated_at FROM words WHERE id = ?1", params![id], |r| {
        Ok(Snapshot { id: r.get(0)?, word: r.get(1)?, reading: r.get(2)?, meaning: r.get(3)?, tags: r.get(4)?, link: r.get(5)?, updated_at: r.get(6)? })
    })
    .optional()
}

/// 複数の語を削除し、消す前の内容を返す(すでに無い id は読み飛ばす)。
/// トランザクションは呼び出し側で管理する。列の移行(`core::migrate`)が済んでいること。
pub fn delete_with_snapshots(conn: &Connection, ids: &[i64]) -> rusqlite::Result<Vec<Snapshot>> {
    let mut out = Vec::new();
    for &id in ids {
        if let Some(s) = read(conn, id)? {
            conn.execute("DELETE FROM words WHERE id = ?1", params![id])?;
            out.push(s);
        }
    }
    Ok(out)
}

/// タグ `from` を、すべての語で `to` に付け替える(`to` が None なら外す)。
/// 付け替え先のタグがすでに付いている語では、1つにまとまる(統合)。
/// 書き換えた語の (変える前の内容, 書き込んだ後の更新日時) を返す(元に戻すための記録になる)。
/// タグは、書いたとおりの文字で比べる(`IT` と `it` は別のタグ。書き方の違いをそろえるのに使えるように)。
/// 内容を変えるので、更新日時も必ず変える(編集の衝突を、更新日時で検知しているため)。
/// トランザクションは呼び出し側で管理する。
pub fn retag(conn: &Connection, from: &str, to: Option<&str>) -> rusqlite::Result<Vec<(Snapshot, i64)>> {
    let ids: Vec<i64> = {
        // 候補を大まかに絞ってから(LIKE)、タグ1つずつを正確に比べる
        let mut st = conn.prepare("SELECT id, tags FROM words WHERE instr(' ' || tags || ' ', ' ' || ?1 || ' ') > 0")?;
        let v = st.query_map(params![from], |r| r.get::<_, i64>(0))?.collect::<rusqlite::Result<_>>()?;
        v
    };
    let now = crate::core::now_unix();
    let mut out = Vec::new();
    for id in ids {
        let Some(before) = read(conn, id)? else { continue };
        let replaced: Vec<&str> = before.tags.split(' ').filter(|t| !t.is_empty()).filter_map(|t| if t == from { to } else { Some(t) }).collect();
        let tags = crate::core::normalize_tags(&replaced.join(" "));
        if tags == before.tags {
            continue;
        }
        let ts = now.max(before.updated_at + 1);
        conn.execute("UPDATE words SET tags = ?1, updated_at = ?2 WHERE id = ?3", params![tags, ts, id])?;
        out.push((before, ts));
    }
    Ok(out)
}

/// 記録した操作を元に戻す。トランザクションは呼び出し側で管理する。
pub fn revert(conn: &Connection, u: &Undo) -> rusqlite::Result<Reverted> {
    let mut r = Reverted::default();
    let mut count = |n: usize| {
        if n > 0 {
            r.done += 1;
        } else {
            r.skipped += 1;
        }
    };
    // 書き換えた語: 自分が書いた更新日時のままなら、前の内容に戻す
    for (before, ts_after) in &u.updated {
        let n = conn.execute(
            "UPDATE words SET word = ?1, reading = ?2, meaning = ?3, tags = ?4, link = ?5, updated_at = ?6 WHERE id = ?7 AND updated_at = ?8",
            params![before.word, before.reading, before.meaning, before.tags, before.link, before.updated_at, before.id, ts_after],
        )?;
        count(n);
    }
    // 削除した語: 同じ id で入れ直す(その id がすでにあれば、入れない)
    for s in &u.deleted {
        let n = conn.execute(
            "INSERT INTO words (id, word, reading, meaning, tags, link, updated_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7 WHERE NOT EXISTS (SELECT 1 FROM words WHERE id = ?1)",
            params![s.id, s.word, s.reading, s.meaning, s.tags, s.link, s.updated_at],
        )?;
        count(n);
    }
    // 追加した語: 自分が書いた更新日時のままなら、削除する
    for (id, ts) in &u.inserted {
        let n = conn.execute("DELETE FROM words WHERE id = ?1 AND updated_at = ?2", params![id, ts])?;
        count(n);
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{self, Fields};

    fn open(name: &str) -> (Connection, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("MyDictionary-undo-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let conn = core::open(&dir.join("d.db")).unwrap();
        core::migrate(&conn).unwrap();
        (conn, dir)
    }

    fn f(w: &str, m: &str) -> Fields {
        Fields { word: w.into(), meaning: m.into(), reading: "よみ".into(), tags: "t1 t2".into(), link: "https://example.com/".into() }
    }

    fn all(conn: &Connection) -> Vec<Snapshot> {
        let mut v: Vec<Snapshot> = core::load_all(conn).unwrap().iter().map(Snapshot::of).collect();
        v.sort_by_key(|s| s.id);
        v
    }

    #[test]
    fn revert_insert_delete_update() {
        let (conn, dir) = open("basic");
        let a = core::insert_entry(&conn, &f("a", "1"), Some(100)).unwrap();
        let b = core::insert_entry(&conn, &f("b", "2"), Some(200)).unwrap();
        let before = all(&conn);

        // 追加を元に戻す
        let c = core::insert_entry(&conn, &f("c", "3"), None).unwrap();
        let u = Undo { inserted: vec![(c.id, c.updated_at)], ..Default::default() };
        assert_eq!((u.len(), u.is_empty()), (1, false));
        assert_eq!(revert(&conn, &u).unwrap(), Reverted { done: 1, skipped: 0 });
        assert_eq!(all(&conn), before);

        // 削除を元に戻す: 同じ id・同じ内容・同じ更新日時で戻る
        let snaps = delete_with_snapshots(&conn, &[a.id, 9999, b.id]).unwrap();
        assert_eq!(snaps.len(), 2); // 無い id は読み飛ばす
        assert!(all(&conn).is_empty());
        let u = Undo { deleted: snaps, ..Default::default() };
        assert_eq!(revert(&conn, &u).unwrap(), Reverted { done: 2, skipped: 0 });
        assert_eq!(all(&conn), before);
        // 消した id は使い回されない(戻したあとに追加した語は、新しい id になる)
        let d = core::insert_entry(&conn, &f("d", "4"), None).unwrap();
        assert!(d.id > c.id);
        core::delete_ids(&conn, &[d.id]).unwrap();

        // 編集を元に戻す: 内容も更新日時も前の値に戻る
        let snap_a = Snapshot::of(&a);
        let ts = core::update(&conn, a.id, a.updated_at, &Fields { word: "a2".into(), meaning: "変更".into(), ..Default::default() }).unwrap();
        assert_ne!(all(&conn), before);
        let u = Undo { updated: vec![(snap_a, ts)], ..Default::default() };
        assert_eq!(revert(&conn, &u).unwrap(), Reverted { done: 1, skipped: 0 });
        assert_eq!(all(&conn), before);

        assert!(Undo::default().is_empty());
        assert_eq!(revert(&conn, &Undo::default()).unwrap(), Reverted::default());
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn revert_skips_entries_changed_by_others() {
        let (conn, dir) = open("conflict");
        let a = core::insert_entry(&conn, &f("a", "1"), Some(100)).unwrap();
        let b = core::insert_entry(&conn, &f("b", "2"), Some(200)).unwrap();
        let c = core::insert_entry(&conn, &f("c", "3"), Some(300)).unwrap();

        // 自分: a を編集、b を削除、d を追加
        let ts_a = core::update(&conn, a.id, a.updated_at, &f("a", "自分の変更")).unwrap();
        let snaps = delete_with_snapshots(&conn, &[b.id]).unwrap();
        let d = core::insert_entry(&conn, &f("d", "4"), Some(400)).unwrap();
        let u = Undo { inserted: vec![(d.id, d.updated_at)], deleted: snaps, updated: vec![(Snapshot::of(&a), ts_a)] };

        // ほかの人: a をさらに編集、d も編集
        let ts_a2 = core::update(&conn, a.id, ts_a, &f("a", "ほかの人の変更")).unwrap();
        core::update(&conn, d.id, d.updated_at, &f("d", "ほかの人の変更")).unwrap();

        // 元に戻す: b だけ戻る。a と d は、ほかの人の変更を残す
        assert_eq!(revert(&conn, &u).unwrap(), Reverted { done: 1, skipped: 2 });
        let now = all(&conn);
        assert_eq!(now.len(), 4);
        let get = |id: i64| now.iter().find(|s| s.id == id).unwrap();
        assert_eq!((get(a.id).meaning.as_str(), get(a.id).updated_at), ("ほかの人の変更", ts_a2));
        assert_eq!(get(b.id), &Snapshot::of(&b));
        assert_eq!(get(c.id), &Snapshot::of(&c));
        assert_eq!(get(d.id).meaning, "ほかの人の変更");

        // 追加した語を、ほかの人が先に削除していた場合も、戻さない(何もしない)
        let e = core::insert_entry(&conn, &f("e", "5"), Some(500)).unwrap();
        core::delete_ids(&conn, &[e.id]).unwrap();
        let u = Undo { inserted: vec![(e.id, e.updated_at)], ..Default::default() };
        assert_eq!(revert(&conn, &u).unwrap(), Reverted { done: 0, skipped: 1 });
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn retag_renames_merges_and_removes() {
        let (conn, dir) = open("retag");
        let mk = |w: &str, tags: &str, ts: i64| core::insert_entry(&conn, &Fields { word: w.into(), meaning: "m".into(), tags: tags.into(), ..Default::default() }, Some(ts)).unwrap();
        let a = mk("a", "it 略語", 100);
        let b = mk("b", "IT it", 200); // 付け替え先がすでに付いている → 1つにまとまる
        let c = mk("c", "item", 300); // 別のタグ(it で始まるだけ)は変えない
        let d = mk("d", "IT", 400);
        let before = all(&conn);

        // 名前の変更(書き方をそろえる): it → IT
        let changed = retag(&conn, "it", Some("IT")).unwrap();
        assert_eq!(changed.len(), 2);
        let now = all(&conn);
        let tags = |id: i64| now.iter().find(|s| s.id == id).unwrap().tags.clone();
        assert_eq!((tags(a.id).as_str(), tags(b.id).as_str(), tags(c.id).as_str(), tags(d.id).as_str()), ("IT 略語", "IT", "item", "IT"));
        // 書き換えた語は、更新日時が変わる。変えていない語は、そのまま
        assert!(now.iter().find(|s| s.id == a.id).unwrap().updated_at > 100);
        assert_eq!(now.iter().find(|s| s.id == d.id).unwrap().updated_at, 400);
        // もう一度行っても、変わらない
        assert!(retag(&conn, "it", Some("IT")).unwrap().is_empty());

        // 元に戻す
        let u = Undo { updated: changed, ..Default::default() };
        assert_eq!(revert(&conn, &u).unwrap(), Reverted { done: 2, skipped: 0 });
        assert_eq!(all(&conn), before);

        // 削除: タグを外す
        let changed = retag(&conn, "IT", None).unwrap();
        assert_eq!(changed.len(), 2);
        let now = all(&conn);
        let tags = |id: i64| now.iter().find(|s| s.id == id).unwrap().tags.clone();
        assert_eq!((tags(a.id).as_str(), tags(b.id).as_str(), tags(d.id).as_str()), ("it 略語", "it", ""));
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 元に戻したあと、変更前の内容を読み込んでいた人の編集は通り、変更後の内容を読み込んでいた人の編集は衝突になる
    #[test]
    fn conflict_detection_still_works_after_revert() {
        let (conn, dir) = open("detect");
        let a = core::insert_entry(&conn, &f("a", "元の内容"), Some(100)).unwrap();
        let ts = core::update(&conn, a.id, a.updated_at, &f("a", "自分の変更")).unwrap();
        let u = Undo { updated: vec![(Snapshot::of(&a), ts)], ..Default::default() };
        revert(&conn, &u).unwrap();
        // 変更後(更新日時 ts)を見ていた人: 衝突
        assert!(core::update(&conn, a.id, ts, &f("a", "x")).is_err());
        // 変更前(更新日時 100)を見ていた人: 同じ内容に対する編集なので通る
        assert!(core::update(&conn, a.id, 100, &f("a", "y")).is_ok());
        drop(conn);
        let _ = std::fs::remove_dir_all(dir);
    }
}
