//! しおり。共有の辞書でも人ごとに持てるよう、辞書ファイルではなく各自の設定フォルダに保存する
//! (`bookmarks.tsv`: 辞書のパス<TAB>id<TAB>見出し語<TAB>追加日時)。

use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct Bookmark {
    /// 辞書ファイルのパス(小文字化して比較する)
    pub dict: String,
    pub id: i64,
    /// 一覧に出ないとき(辞書が外された等)でも何のしおりか分かるように控えておく
    pub word: String,
    /// 追加日時(UNIX秒)
    pub added: i64,
}

#[derive(Default)]
pub struct Bookmarks {
    pub items: Vec<Bookmark>,
}

fn key(dict: &str) -> String {
    dict.to_lowercase()
}

impl Bookmarks {
    pub fn parse(text: &str) -> Bookmarks {
        let items = text
            .lines()
            .filter_map(|l| {
                let mut it = l.split('\t');
                let (d, id, w, a) = (it.next()?, it.next()?, it.next()?, it.next()?);
                Some(Bookmark { dict: d.to_string(), id: id.trim().parse().ok()?, word: w.to_string(), added: a.trim().parse().ok()? })
            })
            .collect();
        Bookmarks { items }
    }

    pub fn to_text(&self) -> String {
        self.items.iter().map(|b| format!("{}\t{}\t{}\t{}\n", b.dict, b.id, b.word.replace(['\t', '\n', '\r'], " "), b.added)).collect()
    }

    pub fn load(path: &Path) -> Bookmarks {
        std::fs::read_to_string(path).map(|t| Bookmarks::parse(&t)).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // 途中で失敗しても元のファイルを壊さないよう、別名で書いてから置き換える
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, self.to_text())?;
        std::fs::rename(&tmp, path)
    }

    /// しおりの追加日時。しおりでなければ None
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn added(&self, dict: &str, id: i64) -> Option<i64> {
        let k = key(dict);
        self.items.iter().find(|b| b.id == id && key(&b.dict) == k).map(|b| b.added)
    }

    /// 付け外しする。付けたら true
    pub fn toggle(&mut self, dict: &str, id: i64, word: &str, now: i64) -> bool {
        let k = key(dict);
        if let Some(pos) = self.items.iter().position(|b| b.id == id && key(&b.dict) == k) {
            self.items.remove(pos);
            false
        } else {
            // 同じ秒に続けて付けても順番が保たれるよう、既存の最大より後にする
            let added = self.items.iter().map(|b| b.added + 1).max().unwrap_or(0).max(now);
            self.items.push(Bookmark { dict: dict.to_string(), id, word: word.to_string(), added });
            true
        }
    }

    /// 削除された語のしおりを外す。外した数を返す
    pub fn remove_ids(&mut self, dict: &str, ids: &[i64]) -> usize {
        let k = key(dict);
        let before = self.items.len();
        self.items.retain(|b| !(ids.contains(&b.id) && key(&b.dict) == k));
        before - self.items.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_and_roundtrip() {
        let mut b = Bookmarks::default();
        assert!(b.toggle("C:\\Dict\\a.db", 1, "apple", 100));
        assert!(b.toggle("C:\\Dict\\a.db", 2, "tab\tin word", 100)); // 同じ秒でも順番が付く
        assert!(b.toggle("D:\\b.db", 1, "別の辞書", 50));
        assert_eq!(b.added("c:\\dict\\A.DB", 1), Some(100)); // パスは大文字小文字を区別しない
        assert_eq!(b.added("C:\\Dict\\a.db", 2), Some(101));
        assert_eq!(b.added("D:\\b.db", 1), Some(102));
        assert_eq!(b.added("D:\\b.db", 2), None);

        let back = Bookmarks::parse(&b.to_text());
        assert_eq!(back.items.len(), 3);
        assert_eq!(back.items[1].word, "tab in word");
        assert_eq!(back.added("C:\\Dict\\a.db", 2), Some(101));

        assert!(!b.toggle("C:\\Dict\\a.db", 1, "apple", 200)); // もう一度で外れる
        assert_eq!(b.added("C:\\Dict\\a.db", 1), None);
        assert_eq!(b.remove_ids("c:\\dict\\a.db", &[2, 99]), 1);
        assert_eq!(b.items.len(), 1);
        // 壊れた行は読み飛ばす
        assert_eq!(Bookmarks::parse("x\ty\n\nC:\\a.db\t3\tw\t9\n").items.len(), 1);
    }

    #[test]
    fn save_and_load() {
        let dir = std::env::temp_dir().join(format!("MyDictionary-bm-{}", std::process::id()));
        let p = dir.join("bookmarks.tsv");
        let mut b = Bookmarks::default();
        b.toggle("/d/a.db", 7, "語", 1);
        b.save(&p).unwrap();
        assert_eq!(Bookmarks::load(&p).items, b.items);
        assert!(Bookmarks::load(&dir.join("none.tsv")).items.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
