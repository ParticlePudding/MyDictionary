//! 件数が多いときの読み込み・検索時間の計測(`fastdict --bench [件数]`)。

use crate::core::{self, Entry, Query};
use std::time::Instant;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn pick<'a>(&mut self, v: &[&'a str]) -> &'a str {
        v[(self.next() as usize) % v.len()]
    }
}

const SYL: &[&str] = &[
    "ka", "ki", "ku", "ke", "ko", "sa", "shi", "su", "se", "so", "ta", "chi", "tsu", "te", "to", "na", "ni", "nu", "ne",
    "no", "ha", "hi", "fu", "he", "ho", "ma", "mi", "mu", "me", "mo", "ya", "yu", "yo", "ra", "ri", "ru", "re", "ro",
];
const JP: &[&str] = &[
    "辞書", "言葉", "意味", "説明", "例文", "名詞", "動詞", "形容詞", "用語", "略語", "概念", "歴史", "技術", "日常", "仕事",
    "を表す", "ことば", "の一種", "に関する", "ある状態", "または", "一般に", "特に", "ものごと",
];
const TAGS: &[&str] = &[
    "名詞", "動詞", "形容詞", "IT", "医療", "法律", "経済", "歴史", "科学", "日常", "仕事", "英語", "略語", "固有名詞", "方言",
    "古語", "料理", "音楽", "スポーツ", "地名",
];

fn gen_entries(n: usize) -> Vec<(String, String, String)> {
    let mut r = Rng(12345);
    (0..n)
        .map(|i| {
            let word = format!("{}{}{}-{}", r.pick(SYL), r.pick(SYL), r.pick(SYL), i);
            let meaning: String = (0..6 + r.next() % 8).map(|_| r.pick(JP)).collect::<Vec<_>>().join(" ");
            let tags: Vec<&str> = (0..r.next() % 4).map(|_| r.pick(TAGS)).collect();
            (word, meaning, tags.join(" "))
        })
        .collect()
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

pub fn run(n: usize) -> String {
    let dir = std::env::temp_dir().join(format!("fastdict-bench-{}", std::process::id()));
    let path = dir.join("bench.db");
    let mut out = String::new();
    let mut line = |s: String| {
        out.push_str(&s);
        out.push_str("\r\n");
    };
    line(format!("fastdict ベンチマーク: {n} 件"));

    // 1) テストデータ作成(計測対象外だが所要時間は参考に記録)
    let t = Instant::now();
    let data = gen_entries(n);
    {
        let mut conn = core::open(&path).expect("open");
        let tx = conn.transaction().expect("tx");
        {
            let mut st = tx
                .prepare("INSERT INTO words (word, meaning, tags, updated_at) VALUES (?1, ?2, ?3, ?4)")
                .expect("prepare");
            for (w, m, tg) in &data {
                st.execute(rusqlite::params![w, m, tg, core::now_unix()]).expect("insert");
            }
        }
        tx.commit().expect("commit");
    }
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    line(format!("テストデータ作成: {:.0} ms / DBサイズ {:.1} MB", ms(t), size as f64 / 1_048_576.0));
    drop(data);

    // 2) 起動時に相当する処理: DBを開く → 全件読み込み(検索用の小文字化込み) → 並べ替え
    let t_all = Instant::now();
    let t = Instant::now();
    let conn = core::open(&path).expect("open");
    let open_ms = ms(t);
    let t = Instant::now();
    let mut entries: Vec<Entry> = core::load_all(&conn).expect("load");
    let load_ms = ms(t);
    let t = Instant::now();
    core::sort_entries(&mut entries);
    let sort_ms = ms(t);
    line(format!(
        "起動時の読み込み合計: {:.1} ms  (open {:.1} / 読み込み+小文字化 {:.1} / 並べ替え {:.1})",
        ms(t_all),
        open_ms,
        load_ms,
        sort_ms
    ));
    line("  ※ 読み込みは別スレッドで行うため、この間もウィンドウは表示され入力できます".to_string());

    // 3) 1回の検索(1キー入力あたりの処理時間)
    let cases: Vec<(&str, Query)> = vec![
        ("空欄(全件)", Query::default()),
        ("1文字 \"a\"", Query { all: "a".into(), ..Default::default() }),
        ("3文字 \"kak\"", Query { all: "kak".into(), ..Default::default() }),
        ("日本語 \"辞書\"", Query { all: "辞書".into(), ..Default::default() }),
        ("複数語 \"辞書 技術\"", Query { all: "辞書 技術".into(), ..Default::default() }),
        ("見出し語のみ \"kaki\"", Query { word: "kaki".into(), ..Default::default() }),
        ("意味のみ \"概念 歴史\"", Query { meaning: "概念 歴史".into(), ..Default::default() }),
        ("タグのみ \"IT 医療\"", Query { tag: "IT 医療".into(), ..Default::default() }),
        ("タグ先頭 \"#it\"", Query { all: "#it".into(), ..Default::default() }),
        ("ヒット無し \"zzzzq\"", Query { all: "zzzzq".into(), ..Default::default() }),
    ];
    line("検索1回あたり(5回平均):".to_string());
    for (name, q) in &cases {
        let mut hits = 0;
        let t = Instant::now();
        for _ in 0..5 {
            hits = core::filter(&entries, q).len();
        }
        line(format!("  {:<22} {:>8.2} ms  ({} 件ヒット)", name, ms(t) / 5.0, hits));
    }

    drop(conn);
    let _ = std::fs::remove_dir_all(&dir);
    out
}
