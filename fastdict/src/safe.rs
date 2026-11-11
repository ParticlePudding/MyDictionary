//! 共有フォルダ(複数人での利用)でも壊れにくくするための書き込み手順。
//!
//! SQLite はネットワーク共有上のロックを完全には信頼できないため、破損を「防ぐ」ことはできない。
//! そこで、書き込みのたびに次の手順を踏み、壊れても直前の状態へ戻せるようにする。
//!
//! 1. 書き込みロックを取得(他の人が書き込み中なら数秒待つ)
//! 2. 書き込み前の整合性を検査(すでに壊れていたら書き込まない)
//! 3. 直前の状態をローカルへスナップショット(`pre-write.db`)。10分に1回は世代バックアップも残す
//! 4. 書き込み → コミット
//! 5. ファイルを開き直して整合性を検査。壊れていたらスナップショットから復元(ロールバック)
//! 6. 検査に通った最新の状態もローカルへ保存(`latest.db`)。後で他の人の書き込みで壊れても、ここまで戻せる

use crate::core;
use rusqlite::{Connection, ErrorCode, OpenFlags, TransactionBehavior};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const GENERATIONS: usize = 10;
const GENERATION_INTERVAL_SECS: i64 = 600;

#[derive(Debug)]
pub enum WriteError {
    /// 他の人が書き込み中で、待っても順番が来なかった
    Busy,
    /// 他の人が先に同じ項目を変更または削除していた
    Conflict,
    /// 書き込み前の時点でファイルが壊れていた(書き込みは行っていない)
    CorruptBefore,
    /// 書き込み後に破損を検出し、直前の状態へ戻した
    RolledBack,
    /// 書き込み後に破損を検出したが、復元にも失敗した
    RestoreFailed(String),
    /// ローカルへのバックアップを作れなかった(書き込みは行っていない)
    Backup(String),
    Sql(rusqlite::Error),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::Busy => write!(f, "他の人が書き込み中のため保存できませんでした。少し待ってからやり直してください。"),
            WriteError::Conflict => write!(f, "他の人が先にこの項目を変更または削除しました。最新の状態を読み込み直します。"),
            WriteError::CorruptBefore => write!(f, "辞書ファイルが壊れているため、書き込みを中止しました。"),
            WriteError::RolledBack => {
                write!(f, "書き込み後にファイルの破損を検出したため、書き込み前の状態へ戻しました。今回の変更は保存されていません。")
            }
            WriteError::RestoreFailed(e) => write!(f, "書き込み後にファイルの破損を検出しましたが、復元にも失敗しました: {e}"),
            WriteError::Backup(e) => write!(f, "書き込み前のバックアップを作れなかったため、書き込みを中止しました: {e}"),
            WriteError::Sql(e) => write!(f, "{e}"),
        }
    }
}

/// ファイル破損を示すエラーか
pub fn is_corrupt(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _) if matches!(f.code, ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase))
}

fn classify(e: rusqlite::Error) -> WriteError {
    match &e {
        rusqlite::Error::QueryReturnedNoRows => WriteError::Conflict,
        rusqlite::Error::SqliteFailure(f, _) => match f.code {
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => WriteError::Busy,
            ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase => WriteError::CorruptBefore,
            _ => WriteError::Sql(e),
        },
        _ => WriteError::Sql(e),
    }
}

fn quick_check(conn: &Connection) -> bool {
    matches!(conn.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)), Ok(s) if s == "ok")
}

/// ファイルを開き直して整合性を検査する
pub fn file_ok(db: &Path) -> bool {
    Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .and_then(|c| {
            c.busy_timeout(core::BUSY_TIMEOUT)?;
            Ok(quick_check(&c))
        })
        .unwrap_or(false)
}

fn unix(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// 辞書ファイルごとのバックアップ置き場(ローカル)。同名ファイルを区別するためパスのハッシュを付ける。
pub fn backup_dir(root: &Path, db: &Path) -> PathBuf {
    let full = db.to_string_lossy().to_lowercase();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in full.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let stem: String = db
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "dictionary".into())
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    root.join(format!("{stem}-{:08x}", h as u32))
}

fn snapshot(src: &Connection, dst: &Path) -> Result<(), String> {
    let tmp = dst.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    {
        let mut out = Connection::open(&tmp).map_err(|e| e.to_string())?;
        let b = rusqlite::backup::Backup::new(src, &mut out).map_err(|e| e.to_string())?;
        b.run_to_completion(512, Duration::from_millis(0), None).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, dst).map_err(|e| e.to_string())
}

fn generations(dir: &Path) -> Vec<(i64, PathBuf)> {
    let mut v: Vec<(i64, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_name()?.to_string_lossy().to_string();
            let ts = name.strip_prefix("gen-")?.strip_suffix(".db")?.parse::<i64>().ok()?;
            Some((ts, p))
        })
        .collect();
    v.sort();
    v
}

/// 一定間隔で世代バックアップを残し、古いものを消す
fn rotate(dir: &Path, pre: &Path) {
    let now = core::now_unix();
    let gens = generations(dir);
    if gens.last().map_or(true, |(ts, _)| now - ts >= GENERATION_INTERVAL_SECS) {
        let _ = std::fs::copy(pre, dir.join(format!("gen-{now}.db")));
    }
    let gens = generations(dir);
    if gens.len() > GENERATIONS {
        for (_, p) in &gens[..gens.len() - GENERATIONS] {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// 復元に使える最新のバックアップ(整合性検査に通るもの)と、その時刻(UNIX秒)
pub fn latest_backup(db: &Path, root: &Path) -> Option<(PathBuf, i64)> {
    let dir = backup_dir(root, db);
    // (時刻, 優先度, パス)。同時刻なら 書き込み後(latest) > 書き込み前(pre-write) > 世代 の順に新しいとみなす
    let mut cands: Vec<(i64, u8, PathBuf)> = generations(&dir).into_iter().map(|(ts, p)| (ts, 0, p)).collect();
    for (name, prio) in [("pre-write.db", 1), ("latest.db", 2)] {
        let p = dir.join(name);
        if let Ok(m) = std::fs::metadata(&p).and_then(|m| m.modified()) {
            cands.push((unix(m), prio, p));
        }
    }
    cands.sort();
    cands.into_iter().rev().find(|(_, _, p)| file_ok(p)).map(|(ts, _, p)| (p, ts))
}

/// バックアップを辞書ファイルへ書き戻す。同じフォルダに一時ファイルを作ってから置き換える。
pub fn restore(db: &Path, backup: &Path) -> Result<(), String> {
    if !file_ok(backup) {
        return Err("バックアップ自体が壊れています".into());
    }
    let mut tmp_name = db.file_name().map(|s| s.to_os_string()).unwrap_or_default();
    tmp_name.push(".restore-tmp");
    let tmp = db.with_file_name(tmp_name);
    std::fs::copy(backup, &tmp).map_err(|e| e.to_string())?;
    // 壊れた書き込みの残骸(ジャーナル)を適用させない
    let mut journal = db.as_os_str().to_os_string();
    journal.push("-journal");
    let _ = std::fs::remove_file(PathBuf::from(journal));
    std::fs::rename(&tmp, db).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// 変更検知用: (更新時刻, サイズ)
pub fn file_stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// 安全な手順で書き込む。`f` の中の変更は1トランザクションで、失敗すれば何も変わらない。
pub fn write<T>(db: &Path, backup_root: &Path, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T, WriteError> {
    write_impl(db, backup_root, f, &|_| {})
}

fn write_impl<T>(
    db: &Path,
    backup_root: &Path,
    f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    after_commit: &dyn Fn(&Path),
) -> Result<T, WriteError> {
    let dir = backup_dir(backup_root, db);
    std::fs::create_dir_all(&dir).map_err(|e| WriteError::Backup(e.to_string()))?;
    let pre = dir.join("pre-write.db");

    let mut conn = core::open(db).map_err(classify)?;
    let result = {
        // 1. 書き込みロック(以後、他の人の書き込みは待たされる。読み取りは可能)
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(classify)?;
        // 2. 書き込み前の検査
        if !quick_check(&tx) {
            return Err(WriteError::CorruptBefore);
        }
        // 3. 直前の状態をローカルへ(ロック中なので、他の人の変更もすべて含んだ状態になる)
        {
            let reader = core::open_ro(db).map_err(classify)?;
            snapshot(&reader, &pre).map_err(WriteError::Backup)?;
        }
        rotate(&dir, &pre);
        // 4. 書き込み
        let r = f(&tx).map_err(classify)?;
        tx.commit().map_err(classify)?;
        r
    };
    drop(conn);
    after_commit(db);

    // 5. 開き直して検査。壊れていたら直前の状態へ戻す
    if !file_ok(db) {
        return match restore(db, &pre) {
            Ok(()) => Err(WriteError::RolledBack),
            Err(e) => Err(WriteError::RestoreFailed(e)),
        };
    }
    // 6. 検査に通った最新の状態もローカルに残す(他の人の書き込みで壊れたとき、ここまで戻せる)。
    //    失敗しても書き込み自体は成功しているので、結果には影響させない。
    if let Ok(reader) = core::open_ro(db) {
        let _ = snapshot(&reader, &dir.join("latest.db"));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fastdict-safe-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn words(db: &Path) -> Vec<String> {
        let c = core::open(db).unwrap();
        let mut v: Vec<String> = core::load_all(&c).unwrap().into_iter().map(|e| e.word).collect();
        v.sort();
        v
    }

    fn corrupt(db: &Path) {
        let mut bytes = std::fs::read(db).unwrap();
        for b in bytes.iter_mut().take(2048) {
            *b = 0xAB;
        }
        std::fs::write(db, bytes).unwrap();
    }

    #[test]
    fn normal_write_and_backup() {
        let d = tmp("normal");
        let (db, root) = (d.join("share").join("dict.db"), d.join("local"));
        write(&db, &root, |c| core::insert(c, "a", "1", "")).unwrap();
        write(&db, &root, |c| core::insert(c, "b", "2", "")).unwrap();
        assert_eq!(words(&db), vec!["a", "b"]);
        // 直前(b を入れる前)の状態がローカルに残っている
        let pre = backup_dir(&root, &db).join("pre-write.db");
        assert_eq!(words(&pre), vec!["a"]);
        assert_eq!(generations(&backup_dir(&root, &db)).len(), 1); // 10分以内は世代を増やさない
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn failed_closure_changes_nothing() {
        let d = tmp("fail");
        let (db, root) = (d.join("dict.db"), d.join("local"));
        write(&db, &root, |c| core::insert(c, "a", "1", "")).unwrap();
        let r = write(&db, &root, |c| {
            core::insert(c, "b", "2", "")?;
            core::update(c, 9999, 0, "x", "y", "") // 存在しない → 失敗
        });
        assert!(matches!(r, Err(WriteError::Conflict)));
        assert_eq!(words(&db), vec!["a"]); // b も入っていない(1トランザクション)
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn corruption_after_write_is_rolled_back() {
        let d = tmp("rollback");
        let (db, root) = (d.join("dict.db"), d.join("local"));
        write(&db, &root, |c| core::insert(c, "a", "1", "")).unwrap();
        // 書き込み直後にファイルが壊れた、という状況を作る
        let r = write_impl(&db, &root, |c| core::insert(c, "b", "2", ""), &|p| corrupt(p));
        assert!(matches!(r, Err(WriteError::RolledBack)), "{r:?}");
        assert!(file_ok(&db));
        assert_eq!(words(&db), vec!["a"]); // 書き込み前の状態に戻っている
        // その後も普通に書ける
        write(&db, &root, |c| core::insert(c, "c", "3", "")).unwrap();
        assert_eq!(words(&db), vec!["a", "c"]);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn corrupt_before_write_is_refused_and_restorable() {
        let d = tmp("before");
        let (db, root) = (d.join("dict.db"), d.join("local"));
        write(&db, &root, |c| core::insert(c, "a", "1", "")).unwrap();
        write(&db, &root, |c| core::insert(c, "b", "2", "")).unwrap();
        corrupt(&db); // 他の人の書き込みで壊れた、という状況
        let r = write(&db, &root, |c| core::insert(c, "c", "3", ""));
        assert!(matches!(r, Err(WriteError::CorruptBefore)), "{r:?}");
        // 最新の正常なバックアップ(自分の最後の書き込み b の直後の状態)から復元できる
        let (bk, ts) = latest_backup(&db, &root).unwrap();
        assert!(ts > 0);
        restore(&db, &bk).unwrap();
        assert!(file_ok(&db));
        assert_eq!(words(&db), vec!["a", "b"]);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn rotation_keeps_limited_generations() {
        let d = tmp("rotate");
        let dir = d.join("bk");
        std::fs::create_dir_all(&dir).unwrap();
        let pre = dir.join("pre-write.db");
        std::fs::write(&pre, b"x").unwrap();
        for i in 0..15 {
            std::fs::write(dir.join(format!("gen-{}.db", 1000 + i)), b"x").unwrap();
        }
        rotate(&dir, &pre); // 古い世代(1000台)しかないので新しい世代を追加し、10個に絞る
        let g = generations(&dir);
        assert_eq!(g.len(), GENERATIONS);
        assert!(g.last().unwrap().0 > 100_000);
        assert_eq!(g[0].0, 1006);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn backup_dirs_differ_by_path() {
        let r = Path::new("/r");
        assert_ne!(backup_dir(r, Path::new("/a/dict.db")), backup_dir(r, Path::new("/b/dict.db")));
        assert_eq!(backup_dir(r, Path::new("/a/dict.db")), backup_dir(r, Path::new("/a/dict.db")));
    }
}
