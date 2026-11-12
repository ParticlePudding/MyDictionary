//! 共通の見た目: 色、フォント、フラットなボタン描画、小さな Win32 ヘルパー。

use std::sync::atomic::{AtomicU32, Ordering};
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::DRAWITEMSTRUCT;
use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

pub const WHITE: COLORREF = rgb(255, 255, 255);
pub const PANEL: COLORREF = rgb(246, 247, 249);
pub const BORDER: COLORREF = rgb(214, 217, 222);
pub const TEXT: COLORREF = rgb(31, 35, 40);
pub const MUTED: COLORREF = rgb(101, 109, 118);
pub const ACCENT: COLORREF = rgb(0, 103, 192);
pub const ACCENT_PRESSED: COLORREF = rgb(0, 80, 160);
pub const ACCENT_SOFT: COLORREF = rgb(228, 240, 252);
pub const DANGER: COLORREF = rgb(196, 43, 28);
pub const DANGER_SOFT: COLORREF = rgb(253, 236, 234);
/// 注意(エラーではない知らせ)
pub const WARN: COLORREF = rgb(168, 92, 0);
/// 検索語の強調表示
pub const MARK: COLORREF = rgb(214, 32, 32);
pub const HAIRLINE: COLORREF = rgb(236, 238, 241);
/// しおりの ★
pub const STAR: COLORREF = rgb(232, 163, 23);

/// プログラムから入力欄を操作している間、通知(EN_CHANGE など)を無視するためのカウンタ
pub static BUSY: AtomicU32 = AtomicU32::new(0);

pub fn quiet<R>(f: impl FnOnce() -> R) -> R {
    BUSY.fetch_add(1, Ordering::SeqCst);
    let r = f();
    BUSY.fetch_sub(1, Ordering::SeqCst);
    r
}

pub fn is_busy() -> bool {
    BUSY.load(Ordering::SeqCst) > 0
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub unsafe fn send(h: HWND, msg: u32, w: usize, l: isize) -> LRESULT {
    SendMessageW(h, msg, WPARAM(w), LPARAM(l))
}

pub unsafe fn get_text(h: HWND) -> String {
    let len = GetWindowTextLengthW(h);
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(h, &mut buf);
    String::from_utf16_lossy(&buf[..n as usize])
}

/// 設定(EN_CHANGE を発生させない)
pub unsafe fn set_text(h: HWND, s: &str) {
    let w = wide(s);
    quiet(|| {
        let _ = SetWindowTextW(h, PCWSTR(w.as_ptr()));
    });
}

pub fn scale(v: i32, dpi: i32) -> i32 {
    v * dpi / 96
}

pub unsafe fn font(face: &str, pt: i32, dpi: i32, bold: bool) -> HFONT {
    let face = wide(face);
    CreateFontW(
        -(pt * dpi / 72),
        0,
        0,
        0,
        if bold { 700 } else { 400 },
        0,
        0,
        0,
        DEFAULT_CHARSET.0 as u32,
        OUT_DEFAULT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32,
        CLEARTYPE_QUALITY.0 as u32,
        0,
        PCWSTR(face.as_ptr()),
    )
}

/// アプリの名前と版(ウィンドウのタイトル、設定画面の表示に使う)
pub const APP_NAME: &str = "MyDictionary";
pub const APP_VERSION: &str = "2.02";

/// 実行ファイルに埋め込んだアイコン(リソース ID 1)。読み込めなければ Windows 標準のアイコン
pub unsafe fn app_icon() -> HICON {
    let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default();
    LoadIconW(hinst, PCWSTR(1 as *const u16)).or_else(|_| LoadIconW(None, IDI_APPLICATION)).unwrap_or_default()
}

/// 画面全体で共通の寸法(96dpi 基準)。ボタンと1行入力欄は、どの画面でもこの大きさにそろえる。
pub const CTL_H: i32 = 36;
pub const BTN_W: i32 = 108;

/// 画面全体で共通のフォント。操作部品(ボタン・入力欄・一覧)はすべて `base`。
pub struct Fonts {
    /// 操作部品の標準(10pt)
    pub base: HFONT,
    /// 補足(9pt): ラベル、候補リストの2行目、ステータス
    pub small: HFONT,
    /// 見出し(10pt 太字): 候補リストの見出し語、設定の区分名
    pub heading: HFONT,
}

impl Fonts {
    pub unsafe fn new(dpi: i32) -> Fonts {
        let face = "Yu Gothic UI";
        Fonts { base: font(face, 10, dpi, false), small: font(face, 9, dpi, false), heading: font(face, 10, dpi, true) }
    }

    pub unsafe fn destroy(&self) {
        for f in [self.base, self.small, self.heading] {
            let _ = DeleteObject(f);
        }
    }
}

pub unsafe fn fill(hdc: HDC, rc: &RECT, color: COLORREF) {
    let b = CreateSolidBrush(color);
    FillRect(hdc, rc, b);
    let _ = DeleteObject(b);
}

/// 角丸の枠(塗り + 1px 枠線)
pub unsafe fn round_box(hdc: HDC, rc: &RECT, fill_color: COLORREF, border: COLORREF, radius: i32) {
    let pen = CreatePen(PS_SOLID, 1, border);
    let brush = CreateSolidBrush(fill_color);
    let (op, ob) = (SelectObject(hdc, pen), SelectObject(hdc, brush));
    let _ = RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, radius, radius);
    SelectObject(hdc, op);
    SelectObject(hdc, ob);
    let _ = DeleteObject(pen);
    let _ = DeleteObject(brush);
}

#[derive(Clone, Copy)]
pub enum Btn {
    Primary,
    Secondary,
    Danger,
    Toggle(bool),
}

/// WM_DRAWITEM のボタン描画(BS_OWNERDRAW)。背景は親の色で塗ってから角丸で描く。
pub unsafe fn draw_button(dis: &DRAWITEMSTRUCT, kind: Btn, font: HFONT, parent_bg: COLORREF, dpi: i32) {
    let hdc = dis.hDC;
    let rc = dis.rcItem;
    let pressed = dis.itemState.0 & 0x1 != 0; // ODS_SELECTED
    let disabled = dis.itemState.0 & 0x4 != 0; // ODS_DISABLED
    let focused = dis.itemState.0 & 0x10 != 0; // ODS_FOCUS
    fill(hdc, &rc, parent_bg);

    let (bg, border, fg) = match kind {
        Btn::Primary => (if pressed { ACCENT_PRESSED } else { ACCENT }, ACCENT, WHITE),
        Btn::Secondary => (if pressed { PANEL } else { WHITE }, BORDER, TEXT),
        Btn::Danger => (if pressed { DANGER_SOFT } else { WHITE }, DANGER, DANGER),
        Btn::Toggle(on) => {
            if on {
                (ACCENT_SOFT, ACCENT, ACCENT)
            } else {
                (if pressed { PANEL } else { WHITE }, BORDER, TEXT)
            }
        }
    };
    let (bg, border, fg) = if disabled { (PANEL, BORDER, MUTED) } else { (bg, border, fg) };
    let border = if focused && !disabled && !matches!(kind, Btn::Primary | Btn::Danger) { ACCENT } else { border };

    let inner = RECT { left: rc.left, top: rc.top, right: rc.right, bottom: rc.bottom };
    round_box(hdc, &inner, bg, border, scale(8, dpi));

    let mut buf = vec![0u16; 128];
    let n = GetWindowTextW(dis.hwndItem, &mut buf);
    let mut text = buf[..n as usize].to_vec();
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, fg);
    let old = SelectObject(hdc, font);
    let mut r = inner;
    DrawTextW(hdc, &mut text, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
    SelectObject(hdc, old);
}

/// WM_CTLCOLORSTATIC / WM_CTLCOLOREDIT 用: 文字色と背景ブラシを返す
pub unsafe fn ctl_color(hdc: HDC, text: COLORREF, bg: COLORREF, brush: HBRUSH) -> LRESULT {
    SetTextColor(hdc, text);
    SetBkColor(hdc, bg);
    LRESULT(brush.0 as isize)
}

/// 枠付き入力欄。枠(角丸)は親の WM_PAINT で `paint_fields` が描き、EDIT 本体は枠の内側に置く。
pub unsafe fn field(
    parent: HWND,
    id: usize,
    (x, y, w, h): (i32, i32, i32, i32),
    dpi: i32,
    multiline: bool,
    fields: &mut Vec<(RECT, HWND)>,
    fnt: HFONT,
) -> HWND {
    let hinst = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap();
    let style = WS_TABSTOP.0
        | if multiline {
            WS_VSCROLL.0 | ES_MULTILINE as u32 | ES_WANTRETURN as u32 | ES_AUTOVSCROLL as u32
        } else {
            ES_AUTOHSCROLL as u32
        };
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        windows::core::w!("EDIT"),
        PCWSTR::null(),
        WINDOW_STYLE(style) | WS_CHILD | WS_VISIBLE,
        0,
        0,
        10,
        10,
        parent,
        HMENU(id as *mut std::ffi::c_void),
        hinst,
        None,
    )
    .unwrap();
    send(hwnd, WM_SETFONT, fnt.0 as usize, 1);
    place_field(hwnd, (x, y, w, h), dpi, multiline, fields);
    hwnd
}

/// 枠の位置・大きさを決め、EDIT をその内側に配置する(既存なら更新)
pub unsafe fn place_field(edit: HWND, (x, y, w, h): (i32, i32, i32, i32), dpi: i32, multiline: bool, fields: &mut Vec<(RECT, HWND)>) {
    let rc = RECT { left: x, top: y, right: x + w, bottom: y + h };
    let pad = scale(10, dpi);
    if multiline {
        let vpad = scale(8, dpi);
        let _ = MoveWindow(edit, x + pad, y + vpad, w - 2 * pad, h - 2 * vpad, true);
    } else {
        // 1行入力は枠の縦中央に置く(高さは文字高さに合わせる)
        let line_h = scale(22, dpi);
        let _ = MoveWindow(edit, x + pad, y + (h - line_h) / 2, w - 2 * pad, line_h, true);
    }
    if let Some(f) = fields.iter_mut().find(|f| f.1 == edit) {
        f.0 = rc;
    } else {
        fields.push((rc, edit));
    }
}

pub unsafe fn paint_fields(hdc: HDC, fields: &[(RECT, HWND)], dpi: i32) {
    let focus = GetFocus();
    for (rc, h) in fields {
        if !IsWindowVisible(*h).as_bool() {
            continue; // 非表示の欄(詳細検索オフ時など)の枠は描かない
        }
        let color = if *h == focus { ACCENT } else { BORDER };
        round_box(hdc, rc, WHITE, color, scale(8, dpi));
    }
}

/// フォーカス変化時にその入力欄の枠だけ再描画する
pub unsafe fn invalidate_field(parent: HWND, fields: &[(RECT, HWND)], ctl: HWND) {
    if let Some((rc, _)) = fields.iter().find(|f| f.1 == ctl) {
        let _ = InvalidateRect(parent, Some(rc), false);
    }
}

/// 1行のテキストを、`ranges`(UTF-16 単位)の部分だけ `mark` 色にして描く。
/// 幅に収まらないときは末尾を「…」にする。
pub unsafe fn draw_marked(hdc: HDC, text: &str, ranges: &[(usize, usize)], rc: &RECT, color: COLORREF, mark: COLORREF) {
    let width = rc.right - rc.left;
    if width <= 0 || text.is_empty() {
        return;
    }
    let mut w16: Vec<u16> = text.encode_utf16().collect();
    let mut dx = vec![0i32; w16.len()];
    let mut fit = 0i32;
    let mut size = SIZE::default();
    let _ = GetTextExtentExPointW(hdc, PCWSTR(w16.as_ptr()), w16.len() as i32, width, Some(&mut fit), Some(dx.as_mut_ptr()), &mut size);
    let mut ellipsis = false;
    if (fit as usize) < w16.len() {
        // 「…」の分を空けて、収まる文字数を求め直す
        let dots: Vec<u16> = "…".encode_utf16().collect();
        let mut ds = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &dots, &mut ds);
        let mut n = fit as usize;
        while n > 0 && dx[n - 1] > width - ds.cx {
            n -= 1;
        }
        // サロゲートペアの途中で切らない
        if n > 0 && (0xD800..0xDC00).contains(&w16[n - 1]) {
            n -= 1;
        }
        w16.truncate(n);
        ellipsis = true;
    }
    let n = w16.len();
    SetBkMode(hdc, TRANSPARENT);
    let x_at = |i: usize| rc.left + if i == 0 { 0 } else { dx[i - 1] };
    let mut pos = 0usize;
    let mut segs: Vec<(usize, usize, bool)> = Vec::new();
    for &(a, b) in ranges {
        let (a, b) = (a.min(n), b.min(n));
        if a > pos {
            segs.push((pos, a, false));
        }
        if b > a {
            segs.push((a, b, true));
        }
        pos = pos.max(b);
    }
    if pos < n {
        segs.push((pos, n, false));
    }
    for (a, b, marked) in segs {
        SetTextColor(hdc, if marked { mark } else { color });
        let _ = ExtTextOutW(hdc, x_at(a), rc.top, ETO_CLIPPED, Some(rc), PCWSTR(w16[a..b].as_ptr()), (b - a) as u32, None);
    }
    if ellipsis {
        let dots: Vec<u16> = "…".encode_utf16().collect();
        SetTextColor(hdc, color);
        let _ = ExtTextOutW(hdc, x_at(n), rc.top, ETO_CLIPPED, Some(rc), PCWSTR(dots.as_ptr()), dots.len() as u32, None);
    }
}

/// テキストの描画幅(現在選択中のフォント)
pub unsafe fn text_width(hdc: HDC, text: &str) -> i32 {
    let w: Vec<u16> = text.encode_utf16().collect();
    let mut s = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &w, &mut s);
    s.cx
}

/// テキストをクリップボードへコピーする
pub unsafe fn set_clipboard(hwnd: HWND, text: &str) -> bool {
    use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    let w = wide(&text.replace("\r\n", "\n").replace('\n', "\r\n"));
    if OpenClipboard(hwnd).is_err() {
        return false;
    }
    let _ = EmptyClipboard();
    let ok = match GlobalAlloc(GMEM_MOVEABLE, w.len() * 2) {
        Ok(h) => {
            let p = GlobalLock(h) as *mut u16;
            if p.is_null() {
                false
            } else {
                std::ptr::copy_nonoverlapping(w.as_ptr(), p, w.len());
                let _ = GlobalUnlock(h);
                SetClipboardData(13 /* CF_UNICODETEXT */, HANDLE(h.0)).is_ok()
            }
        }
        Err(_) => false,
    };
    let _ = CloseClipboard();
    ok
}

/// Ctrl+A: フォーカスが入力欄(EDIT)にあれば全選択する。処理したら true。
/// 複数行の入力欄は標準では Ctrl+A に対応していないため、メッセージループで補う。
pub unsafe fn select_all_if_edit(msg: &MSG) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU};
    if msg.message != WM_KEYDOWN || msg.wParam.0 != 'A' as usize {
        return false;
    }
    if GetKeyState(VK_CONTROL.0 as i32) >= 0 || GetKeyState(VK_MENU.0 as i32) < 0 {
        return false;
    }
    let f = GetFocus();
    let mut cls = [0u16; 16];
    let n = GetClassNameW(f, &mut cls) as usize;
    if String::from_utf16_lossy(&cls[..n]).eq_ignore_ascii_case("edit") {
        send(f, 0x00B1 /* EM_SETSEL */, 0, -1);
        return true;
    }
    false
}

/// WM_NEXTDLGCTL の処理。複数行の入力欄は、Tab での移動を親ウィンドウへこのメッセージで依頼する。
/// 通常のダイアログなら既定の処理が行うが、自前のウィンドウでは自分で実装する必要がある。
pub unsafe fn next_dlg_ctl(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) {
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    let target = if lparam.0 & 0xFFFF != 0 {
        HWND(wparam.0 as *mut std::ffi::c_void) // wParam が移動先のコントロール
    } else {
        // wParam が 0 なら次、0 以外なら前のコントロールへ
        GetNextDlgTabItem(hwnd, GetFocus(), wparam.0 != 0).unwrap_or_default()
    };
    if !target.is_invalid() {
        let _ = SetFocus(target);
        // 1行の入力欄へ移ったときは、通常のダイアログと同じく全選択にする
        let mut cls = [0u16; 16];
        let n = GetClassNameW(target, &mut cls) as usize;
        if String::from_utf16_lossy(&cls[..n]).eq_ignore_ascii_case("edit") && (GetWindowLongW(target, GWL_STYLE) as u32 & ES_MULTILINE as u32) == 0 {
            send(target, 0x00B1 /* EM_SETSEL */, 0, -1);
        }
    }
}
