//! 画像まわりの、画面に依存しない部分。
//!
//! - 画像ファイルの種類と大きさを、先頭の数バイトから調べる(`probe`)
//! - 「画像を追加…」で選んだファイルを、辞書と同じフォルダの `<辞書のファイル名>_pic` へコピーする(`place`)
//!
//! 意味の欄には、画像を `![説明](パス)` と書く(`core::parse_body`)。画像の中身は辞書ファイルには入れない
//! (書き込みのたびに辞書全体のバックアップを取っているので、辞書が大きくなると保存が遅くなるため)。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// 表示できる画像の種類
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Png,
    Jpeg,
    Gif,
    Bmp,
}

/// 1枚の画像として読み込む大きさの上限(これより大きいファイルは、表示しない)
pub const MAX_BYTES: u64 = 20 * 1024 * 1024;
/// 1つの語で表示する画像の数の上限
pub const MAX_IMAGES: usize = 20;
/// 「画像を追加…」で選べる拡張子
pub const EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "bmp"];

/// 拡張子が、画像のものか
pub fn has_image_ext(path: &str) -> bool {
    let name = path.rsplit(['\\', '/']).next().unwrap_or("").to_lowercase();
    match name.rsplit_once('.') {
        Some((_, ext)) => EXTENSIONS.contains(&ext),
        None => false,
    }
}

/// 表示のために、自動で読み込んでよい画像のパスか。
/// アプリは通信をしない方針なので、ファイルのパス(ドライブ、共有フォルダ、相対パス)だけを読む。次のものは読まない:
/// - URL(`file://…` など、スキームの付いたもの)
/// - `\\サーバー@SSL\…` `\\サーバー@8080\…` のように、共有フォルダではなく HTTP(WebDAV)でつながる形
/// - `\\.\…` `\\?\…` で始まる特別な形(機器などを指せる)
pub fn loadable(path: &str) -> bool {
    let p = path.replace('/', "\\");
    if let Some(rest) = p.strip_prefix("\\\\") {
        let host = rest.split('\\').next().unwrap_or("");
        return !host.is_empty() && host != "." && host != "?" && !host.contains('@');
    }
    crate::core::is_absolute_path(path) || crate::core::is_relative_path(path)
}

/// 画像の種類と、幅・高さ(ピクセル)。画像でなければ None
pub fn probe(b: &[u8]) -> Option<(Kind, u32, u32)> {
    let be16 = |i: usize| b.get(i..i + 2).map(|v| u16::from_be_bytes([v[0], v[1]]) as u32);
    let le16 = |i: usize| b.get(i..i + 2).map(|v| u16::from_le_bytes([v[0], v[1]]) as u32);
    let be32 = |i: usize| b.get(i..i + 4).map(|v| u32::from_be_bytes([v[0], v[1], v[2], v[3]]));
    let le32 = |i: usize| b.get(i..i + 4).map(|v| i32::from_le_bytes([v[0], v[1], v[2], v[3]]));
    let ok = |k: Kind, w: u32, h: u32| (w > 0 && h > 0 && w <= 65_535 && h <= 65_535).then_some((k, w, h));

    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        // 最初のチャンクが IHDR(幅・高さは、その先頭)
        if b.get(12..16) == Some(b"IHDR") {
            return ok(Kind::Png, be32(16)?, be32(20)?);
        }
        return None;
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return ok(Kind::Gif, le16(6)?, le16(8)?);
    }
    if b.starts_with(b"BM") {
        // ファイルヘッダ(14バイト)の後ろに、ビットマップ情報ヘッダ
        let size = le32(14)?;
        return if size == 12 { ok(Kind::Bmp, le16(18)?, le16(20)?) } else { ok(Kind::Bmp, le32(18)?.unsigned_abs(), le32(22)?.unsigned_abs()) };
    }
    if b.starts_with(&[0xFF, 0xD8]) {
        // 区切り(マーカー)を順にたどって、画像の大きさが入っているもの(SOF)を探す
        let mut i = 2;
        while i + 4 <= b.len() {
            if b[i] != 0xFF {
                i += 1;
                continue;
            }
            let m = b[i + 1];
            if m == 0xFF {
                i += 1; // 詰めもの
                continue;
            }
            if m == 0xD8 || m == 0x01 || (0xD0..=0xD7).contains(&m) {
                i += 2; // 長さを持たないマーカー
                continue;
            }
            let len = be16(i + 2)? as usize;
            let is_sof = (0xC0..=0xCF).contains(&m) && !matches!(m, 0xC4 | 0xC8 | 0xCC);
            if is_sof {
                return ok(Kind::Jpeg, be16(i + 7)?, be16(i + 5)?);
            }
            if m == 0xD9 || m == 0xDA || len < 2 {
                break; // 画像の本体まで来た(大きさが見つからなかった)
            }
            i += 2 + len;
        }
        return None;
    }
    None
}

/// 表示する大きさ(ピクセル)。`max_w` に収まるように縮小する(拡大はしない)。
/// `dpi` が 96 より大きい画面では、その倍率で大きく表示する(文字と同じ扱い)
pub fn fit(w: u32, h: u32, max_w: i32, dpi: i32) -> (i32, i32) {
    let (mut dw, mut dh) = (w as i64 * dpi as i64 / 96, h as i64 * dpi as i64 / 96);
    let max_w = max_w.max(16) as i64;
    if dw > max_w {
        dh = (dh * max_w / dw).max(1);
        dw = max_w;
    }
    (dw.max(1) as i32, dh.max(1) as i32)
}

/// 32ビットの画素(青・緑・赤・不透明度の順。色には不透明度を掛けてある)を、
/// 背景色 `bg`(赤, 緑, 青)の上に重ねた、24ビットの DIB(情報ヘッダ 40 バイト + 画素。行は下から上)にする。
/// 透明な部分のある画像(図など)を、本文の背景の上にそのまま置けるようにするため
pub fn dib_from_pbgra(pixels: &[u8], w: usize, h: usize, bg: (u8, u8, u8)) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || pixels.len() < w * h * 4 {
        return None;
    }
    let stride = (w * 3 + 3) & !3; // 行は4バイトの倍数にそろえる
    let mut dib = Vec::with_capacity(40 + stride * h);
    dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
    dib.extend_from_slice(&(w as i32).to_le_bytes());
    dib.extend_from_slice(&(h as i32).to_le_bytes()); // 正の値 = 下から上
    dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    dib.extend_from_slice(&24u16.to_le_bytes()); // biBitCount
    dib.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    dib.extend_from_slice(&((stride * h) as u32).to_le_bytes());
    dib.extend_from_slice(&[0u8; 16]); // 解像度と色数(使わない)
    let over = |c: u8, a: u8, back: u8| (c as u32 + back as u32 * (255 - a as u32) / 255).min(255) as u8;
    for y in (0..h).rev() {
        let row = &pixels[y * w * 4..(y + 1) * w * 4];
        for px in row.chunks_exact(4) {
            let a = px[3];
            dib.push(over(px[0], a, bg.2));
            dib.push(over(px[1], a, bg.1));
            dib.push(over(px[2], a, bg.0));
        }
        dib.resize(dib.len() + (stride - w * 3), 0);
    }
    Some(dib)
}

/// 辞書の画像を置くフォルダ: 辞書と同じフォルダの `<辞書のファイル名(拡張子なし)>_pic`。
/// 辞書の表示名ではなくファイル名から作る(表示名は、人ごとに違うことがあるため)
pub fn pic_dir(dict: &Path) -> PathBuf {
    let stem = dict.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "dictionary".into());
    dict.with_file_name(format!("{stem}_pic"))
}

/// 画像のファイル名を、`![説明](パス)` の中に書いても困らない形にする。
/// 記法とぶつかる括弧は `_` に置き換え、長すぎる名前は切り詰める(拡張子は残す)
pub fn safe_name(file_name: &str) -> String {
    let cleaned: String = file_name.chars().map(|c| if matches!(c, '(' | ')' | '[' | ']' | '<' | '>' | '"' | '\\' | '/' | ':' | '*' | '?' | '|') || c.is_control() { '_' } else { c }).collect();
    let cleaned = cleaned.trim().to_string();
    let (stem, ext) = match cleaned.rsplit_once('.') {
        Some((s, e)) if !e.is_empty() && e.chars().count() <= 5 => (s.to_string(), format!(".{e}")),
        _ => (cleaned.clone(), String::new()),
    };
    let stem: String = stem.trim().trim_matches('.').chars().take(60).collect();
    let stem = stem.trim().to_string();
    if stem.is_empty() {
        format!("image{ext}")
    } else {
        format!("{stem}{ext}")
    }
}

/// 意味の欄に書くパス(辞書のフォルダからの相対パス): `<辞書のファイル名>_pic\<ファイル名>`
pub fn rel_path(dict: &Path, file_name: &str) -> String {
    let dir = pic_dir(dict);
    format!("{}\\{}", dir.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(), file_name)
}

/// `![](…)` の中に書くパス。「画像を追加…」を押した時点での予定の名前(実際のコピーは保存のとき)
pub fn planned_path(src: &Path, dict: &Path) -> String {
    let name = src.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    rel_path(dict, &safe_name(&name))
}

fn same_content(a: &Path, b: &Path) -> std::io::Result<bool> {
    if std::fs::metadata(a)?.len() != std::fs::metadata(b)?.len() {
        return Ok(false);
    }
    let (mut fa, mut fb) = (std::fs::File::open(a)?, std::fs::File::open(b)?);
    let (mut ba, mut bb) = ([0u8; 65536], [0u8; 65536]);
    loop {
        let n = fa.read(&mut ba)?;
        if n == 0 {
            return Ok(true);
        }
        fb.read_exact(&mut bb[..n])?;
        if ba[..n] != bb[..n] {
            return Ok(false);
        }
    }
}

/// 画像 `src` を、辞書の画像のフォルダへコピーする。戻り値は、意味の欄に書くパス(相対パス)。
/// - すでに画像のフォルダの中にあるファイルなら、コピーしない
/// - 同じ名前のファイルがあるとき: 中身が同じならそれを使い、違えば `名前-2` のように番号を付ける
/// - 新しいファイルは「無ければ作る」で作るので、2人が同時に追加しても、上書きは起きない
pub fn place(src: &Path, dict: &Path) -> std::io::Result<String> {
    let dir = pic_dir(dict);
    let src_name = src.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    if src.parent() == Some(dir.as_path()) {
        return Ok(rel_path(dict, &src_name));
    }
    std::fs::create_dir_all(&dir)?;
    let name = safe_name(&src_name);
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.clone(), String::new()),
    };
    for n in 1..1000 {
        let candidate = if n == 1 { name.clone() } else { format!("{stem}-{n}{ext}") };
        let dst = dir.join(&candidate);
        if dst.exists() {
            if same_content(src, &dst)? {
                return Ok(rel_path(dict, &candidate));
            }
            continue;
        }
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&dst) {
            Ok(mut out) => {
                let mut input = std::fs::File::open(src)?;
                let copied = std::io::copy(&mut input, &mut out).and_then(|_| out.flush());
                if let Err(e) = copied {
                    drop(out);
                    let _ = std::fs::remove_file(&dst); // 途中で失敗したファイルは残さない
                    return Err(e);
                }
                return Ok(rel_path(dict, &candidate));
            }
            // ほかの人が、同じ名前で先に作った: 次の番号を試す
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "同じ名前の画像が多すぎます"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_headers() {
        // PNG: 署名 + IHDR(幅 640、高さ 480)
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(probe(&png), Some((Kind::Png, 640, 480)));
        assert_eq!(probe(&png[..18]), None); // 途中で切れている

        // GIF
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&[0x20, 0x03, 0x58, 0x02]); // 800 x 600
        assert_eq!(probe(&gif), Some((Kind::Gif, 800, 600)));

        // BMP: 上下が逆の(高さが負の)ものもある
        let mut bmp = b"BM".to_vec();
        bmp.extend_from_slice(&[0; 12]);
        bmp.extend_from_slice(&40i32.to_le_bytes());
        bmp.extend_from_slice(&320i32.to_le_bytes());
        bmp.extend_from_slice(&(-200i32).to_le_bytes());
        assert_eq!(probe(&bmp), Some((Kind::Bmp, 320, 200)));

        // JPEG: APP0 を読み飛ばして、SOF0 の高さ・幅を読む
        let mut jpg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x4A, 0x46];
        jpg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x01, 0xE0, 0x02, 0x80]); // 高さ 480、幅 640
        assert_eq!(probe(&jpg), Some((Kind::Jpeg, 640, 480)));
        // プログレッシブ(SOF2)も同じ
        let mut jpg2 = vec![0xFF, 0xD8];
        jpg2.extend_from_slice(&[0xFF, 0xC2, 0x00, 0x11, 0x08, 0x00, 0x10, 0x00, 0x20]);
        assert_eq!(probe(&jpg2), Some((Kind::Jpeg, 32, 16)));
        // 大きさが無いまま本体に入ったもの、画像でないもの
        assert_eq!(probe(&[0xFF, 0xD8, 0xFF, 0xDA, 0x00, 0x02, 0x00, 0x00]), None);
        assert_eq!(probe(b"MZ this is an exe"), None);
        assert_eq!(probe(b""), None);

        assert!(has_image_ext("C:\\a\\図.PNG") && has_image_ext("a.jpeg") && has_image_ext("x/y.gif"));
        assert!(!has_image_ext("a.exe") && !has_image_ext("png") && !has_image_ext("a.png.exe") && !has_image_ext("dir.png\\file"));
    }

    #[test]
    fn loadable_paths() {
        // ファイルのパスは読む(ドライブ、共有フォルダ、相対パス)
        for p in ["C:\\pic\\a.png", "c:/pic/a.png", "\\\\server\\share\\図.png", "//server/share/a.png", "dictionary_pic\\a.png", ".\\a.png", "..\\x\\a.png", "図.png"] {
            assert!(loadable(p), "{p}");
        }
        // URL、HTTP(WebDAV)でつながる形、特別な形は読まない
        for p in [
            "file://server/share/a.png",
            "ftp://host/a.png",
            "https://example.com/a.png",
            "\\\\server@SSL\\DavWWWRoot\\a.png",
            "\\\\server@8080\\a\\b.png",
            "//server@ssl/a/b.png",
            "\\\\.\\pipe\\a.png",
            "\\\\?\\UNC\\server\\share\\a.png",
            "\\\\?\\C:\\a.png",
            "\\\\",
            "",
        ] {
            assert!(!loadable(p), "{p}");
        }
    }

    #[test]
    fn fit_to_width() {
        assert_eq!(fit(400, 300, 600, 96), (400, 300)); // 収まるなら、そのまま(拡大しない)
        assert_eq!(fit(1200, 900, 600, 96), (600, 450)); // 幅に合わせて縮小
        assert_eq!(fit(400, 300, 1000, 144), (600, 450)); // 拡大率 150% の画面
        assert_eq!(fit(400, 300, 300, 144), (300, 225));
        assert_eq!(fit(1, 10000, 600, 96), (1, 10000));
        assert_eq!(fit(4000, 1, 600, 96), (600, 1));
    }

    #[test]
    fn dib_composites_over_background() {
        // 2x2 の画像: 上の行 = 不透明の赤、完全に透明 / 下の行 = 半透明の青(不透明度 128)、不透明の白
        let px: Vec<u8> = vec![
            0, 0, 255, 255, /* */ 0, 0, 0, 0, //
            128, 0, 0, 128, /* */ 255, 255, 255, 255,
        ];
        let dib = dib_from_pbgra(&px, 2, 2, (255, 255, 255)).unwrap();
        assert_eq!(dib.len(), 40 + 8 * 2); // 1行は 2 * 3 = 6 バイト → 8 バイトにそろえる
        assert_eq!(&dib[0..4], &40u32.to_le_bytes());
        assert_eq!(&dib[4..8], &2i32.to_le_bytes());
        assert_eq!(&dib[8..12], &2i32.to_le_bytes());
        assert_eq!((dib[14], dib[20..24].to_vec()), (24, 16u32.to_le_bytes().to_vec()));
        // 画素は下の行から。半透明の青は、白の上に重なって明るい青になる。透明なところは背景の白
        assert_eq!(&dib[40..48], &[255, 127, 127, /* */ 255, 255, 255, /* */ 0, 0]);
        assert_eq!(&dib[48..56], &[0, 0, 255, /* */ 255, 255, 255, /* */ 0, 0]);
        // 背景が暗い色のとき
        let dark = dib_from_pbgra(&px, 2, 2, (32, 32, 32)).unwrap();
        assert_eq!(&dark[51..54], &[32, 32, 32]);
        assert!(dib_from_pbgra(&px, 3, 2, (0, 0, 0)).is_none());
        assert!(dib_from_pbgra(&[], 0, 0, (0, 0, 0)).is_none());
    }

    #[test]
    fn names() {
        assert_eq!(safe_name("構成図.png"), "構成図.png");
        assert_eq!(safe_name("図 (1).PNG"), "図 _1_.PNG"); // 記法とぶつかる括弧は置き換える
        assert_eq!(safe_name("a[b]<c>.jpg"), "a_b__c_.jpg");
        assert_eq!(safe_name("  .png"), "image.png");
        assert_eq!(safe_name(""), "image");
        let long = format!("{}.png", "あ".repeat(100));
        assert_eq!(safe_name(&long).chars().count(), 64);
        assert_eq!(pic_dir(Path::new("/share/dict/用語集.db")), PathBuf::from("/share/dict/用語集_pic"));
        assert_eq!(rel_path(Path::new("/share/dict/用語集.db"), "a.png"), "用語集_pic\\a.png");
        assert_eq!(planned_path(Path::new("/tmp/図 (1).png"), Path::new("/share/dictionary.db")), "dictionary_pic\\図 _1_.png");
    }

    #[test]
    fn place_copies_and_avoids_overwrite() {
        let dir = std::env::temp_dir().join(format!("MyDictionary-pics-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let dict = dir.join("dictionary.db");
        let a = dir.join("src").join("図(1).png");
        std::fs::write(&a, b"AAAA").unwrap();

        // コピーされ、相対パスが返る
        assert_eq!(place(&a, &dict).unwrap(), "dictionary_pic\\図_1_.png");
        assert_eq!(std::fs::read(dir.join("dictionary_pic").join("図_1_.png")).unwrap(), b"AAAA");
        // 同じ中身をもう一度: コピーせずに、同じファイルを使う
        assert_eq!(place(&a, &dict).unwrap(), "dictionary_pic\\図_1_.png");
        assert_eq!(std::fs::read_dir(dir.join("dictionary_pic")).unwrap().count(), 1);
        // 同じ名前で中身が違う: 番号を付ける(上書きしない)
        std::fs::write(&a, b"BBBB").unwrap();
        assert_eq!(place(&a, &dict).unwrap(), "dictionary_pic\\図_1_-2.png");
        assert_eq!(std::fs::read(dir.join("dictionary_pic").join("図_1_.png")).unwrap(), b"AAAA");
        std::fs::write(&a, b"CCCCCC").unwrap();
        assert_eq!(place(&a, &dict).unwrap(), "dictionary_pic\\図_1_-3.png");
        // 2番目と同じ中身に戻すと、2番目を使う
        std::fs::write(&a, b"BBBB").unwrap();
        assert_eq!(place(&a, &dict).unwrap(), "dictionary_pic\\図_1_-2.png");
        // 画像のフォルダの中のファイルを選んだときは、コピーしない
        let inside = dir.join("dictionary_pic").join("図_1_.png");
        assert_eq!(place(&inside, &dict).unwrap(), "dictionary_pic\\図_1_.png");
        assert_eq!(std::fs::read_dir(dir.join("dictionary_pic")).unwrap().count(), 3);
        // 元のファイルが無い
        assert!(place(&dir.join("src").join("none.png"), &dict).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
