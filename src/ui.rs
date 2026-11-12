//! 共通の見た目: 色(明るい配色・暗い配色)、フォント、フラットなボタン描画、小さな Win32 ヘルパー。

use crate::config::Theme;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::OnceLock;
use windows::core::{s, w, PCSTR, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, GetKeyNameTextW, IsWindowEnabled, MapVirtualKeyW, MAPVK_VK_TO_VSC};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// 画面の色ひとそろい。明るい配色と暗い配色があり、起動時にどちらかに決める(`init_theme`)。
/// 画面を描くところは、色を直接書かずに `pal()` から取る
pub struct Palette {
    /// 暗い配色か
    pub dark: bool,
    /// 地の色(ウィンドウ、入力欄、ボタン、本文)
    pub bg: COLORREF,
    /// 押しているボタン、使えないボタンの地
    pub panel: COLORREF,
    pub border: COLORREF,
    pub text: COLORREF,
    /// 補足の文字
    pub muted: COLORREF,
    /// 強調(主なボタン、リンク、選んでいる行の印、入力中の欄の枠)
    pub accent: COLORREF,
    pub accent_pressed: COLORREF,
    /// 選んでいる行、オンになっているボタンの地
    pub accent_soft: COLORREF,
    /// `accent` で塗ったところに置く文字
    pub on_accent: COLORREF,
    pub danger: COLORREF,
    pub danger_soft: COLORREF,
    /// 注意(エラーではない知らせ)
    pub warn: COLORREF,
    /// 検索語の強調表示
    pub mark: COLORREF,
    pub hairline: COLORREF,
    /// しおりの ★
    pub star: COLORREF,
}

/// 明るい配色(既定)
pub static LIGHT: Palette = Palette {
    dark: false,
    bg: rgb(255, 255, 255),
    panel: rgb(246, 247, 249),
    border: rgb(214, 217, 222),
    text: rgb(31, 35, 40),
    muted: rgb(101, 109, 118),
    accent: rgb(0, 103, 192),
    accent_pressed: rgb(0, 80, 160),
    accent_soft: rgb(228, 240, 252),
    on_accent: rgb(255, 255, 255),
    danger: rgb(196, 43, 28),
    danger_soft: rgb(253, 236, 234),
    warn: rgb(168, 92, 0),
    mark: rgb(214, 32, 32),
    hairline: rgb(236, 238, 241),
    star: rgb(232, 163, 23),
};

/// 暗い配色。文字と地の明るさの差は、明るい配色と同じくらいになるように選んである
pub static DARK: Palette = Palette {
    dark: true,
    bg: rgb(32, 33, 36),
    panel: rgb(46, 48, 52),
    border: rgb(78, 81, 87),
    text: rgb(232, 234, 237),
    muted: rgb(158, 164, 171),
    accent: rgb(102, 178, 255),
    accent_pressed: rgb(77, 150, 230),
    accent_soft: rgb(38, 58, 84),
    on_accent: rgb(16, 20, 24),
    danger: rgb(255, 123, 114),
    danger_soft: rgb(74, 38, 36),
    warn: rgb(227, 179, 65),
    mark: rgb(255, 128, 110),
    hairline: rgb(50, 52, 56),
    star: rgb(240, 180, 41),
};

static DARK_ON: AtomicBool = AtomicBool::new(false);

/// いまの配色
pub fn pal() -> &'static Palette {
    if DARK_ON.load(Ordering::Relaxed) {
        &DARK
    } else {
        &LIGHT
    }
}

/// 起動時に、配色を決める(ウィンドウを作る前に、1回だけ呼ぶ)。
/// 「Windows の設定に合わせる」のときは、Windows の「アプリの色」の設定を読む。
/// 暗い配色のときは、Windows が描く部分(メニュー、スクロールバー)も暗くするように頼む(`DarkApi`)
pub unsafe fn init_theme(theme: Theme) {
    let dark = match theme {
        Theme::Light => false,
        Theme::Dark => true,
        Theme::System => system_is_dark(),
    };
    DARK_ON.store(dark, Ordering::Relaxed);
    if dark {
        if let Some(api) = dark_api() {
            api.enable_for_app();
        }
    }
}

/// Windows の「アプリの色」の設定が、暗い色か(設定 → 個人用設定 → 色)。読めなければ、明るい色とみなす
unsafe fn system_is_dark() -> bool {
    let (mut value, mut size) = (1u32, 4u32);
    let r = RegGetValueW(
        HKEY_CURRENT_USER,
        w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
        w!("AppsUseLightTheme"),
        RRF_RT_REG_DWORD,
        None,
        Some(&mut value as *mut u32 as *mut c_void),
        Some(&mut size),
    );
    r.is_ok() && value == 0
}

/// Windows のビルド番号(Windows 10 1809 = 17763、1903 = 18362、Windows 11 = 22000 以降)。分からなければ 0
unsafe fn windows_build() -> u32 {
    #[repr(C)]
    struct VersionInfo {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }
    static BUILD: OnceLock<u32> = OnceLock::new();
    *BUILD.get_or_init(|| {
        let Ok(ntdll) = GetModuleHandleW(w!("ntdll.dll")) else { return 0 };
        let Some(f) = GetProcAddress(ntdll, s!("RtlGetVersion")) else { return 0 };
        let f: unsafe extern "system" fn(*mut VersionInfo) -> i32 = std::mem::transmute(f);
        let mut info: VersionInfo = std::mem::zeroed();
        info.size = std::mem::size_of::<VersionInfo>() as u32;
        if f(&mut info) == 0 && info.major >= 10 {
            info.build
        } else {
            0
        }
    })
}

/// Windows が描く部分(メニュー、スクロールバー)を暗くするための、uxtheme.dll の関数。
///
/// これらは公開されていない機能で、名前ではなく番号で呼ぶ。Windows 10 1809(ビルド 17763)で入り、
/// 番号の意味は 1903(ビルド 18362)で1つ変わった。それより古い Windows では、同じ番号が別の関数かもしれないので、
/// ビルド番号を確かめて、分かっている範囲でだけ使う。使えない環境では、メニューとスクロールバーが明るい色のままになる
struct DarkApi {
    build: u32,
    /// 135番: 1903 以降は SetPreferredAppMode(mode)、1809 は AllowDarkModeForApp(allow)
    set_app_mode: unsafe extern "system" fn(i32) -> i32,
    /// 133番: AllowDarkModeForWindow(hwnd, allow)
    allow_window: unsafe extern "system" fn(HWND, i32) -> u8,
    /// 104番: RefreshImmersiveColorPolicyState()
    refresh: unsafe extern "system" fn(),
    /// 136番: FlushMenuThemes()
    flush_menus: unsafe extern "system" fn(),
}

impl DarkApi {
    unsafe fn enable_for_app(&self) {
        // 1903 以降: 2 = ForceDark(Windows の設定が明るい色でも、このアプリは暗くする)。1809: 1 = 暗い色を許可する
        (self.set_app_mode)(if self.build >= 18362 { 2 } else { 1 });
        (self.refresh)();
        (self.flush_menus)();
    }
}

unsafe fn dark_api() -> Option<&'static DarkApi> {
    static API: OnceLock<Option<DarkApi>> = OnceLock::new();
    API.get_or_init(|| {
        let build = windows_build();
        if build < 17763 {
            return None;
        }
        let lib = LoadLibraryExW(w!("uxtheme.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
        let by_number = |n: usize| GetProcAddress(lib, PCSTR(n as *const u8));
        let (app, window, refresh, flush) = (by_number(135)?, by_number(133)?, by_number(104)?, by_number(136)?);
        Some(DarkApi {
            build,
            set_app_mode: std::mem::transmute::<unsafe extern "system" fn() -> isize, unsafe extern "system" fn(i32) -> i32>(app),
            allow_window: std::mem::transmute::<unsafe extern "system" fn() -> isize, unsafe extern "system" fn(HWND, i32) -> u8>(window),
            refresh: std::mem::transmute::<unsafe extern "system" fn() -> isize, unsafe extern "system" fn()>(refresh),
            flush_menus: std::mem::transmute::<unsafe extern "system" fn() -> isize, unsafe extern "system" fn()>(flush),
        })
    })
    .as_ref()
}

/// 暗い配色のとき、ウィンドウのタイトルバーを暗くする(Windows 10 1809 以降。公開されている機能)。
/// ウィンドウを表示する前に呼ぶ
pub unsafe fn dark_title(hwnd: HWND) {
    if !pal().dark {
        return;
    }
    let build = windows_build();
    if build < 17763 {
        return;
    }
    // DWMWA_USE_IMMERSIVE_DARK_MODE: 20(Windows 10 2004 以降)。それより前のビルドでは 19
    let attr = if build >= 18985 { 20 } else { 19 };
    let on = 1i32;
    let _ = DwmSetWindowAttribute(hwnd, DWMWINDOWATTRIBUTE(attr), &on as *const i32 as *const c_void, 4);
}

/// 暗い配色のとき、Windows が描く部品(スクロールバー、一覧の選択、見出しなど)を、暗い見た目にする。
/// `class` は、Windows の暗い見た目の名前(`DarkMode_Explorer` など)。その見た目が無い環境では、何も変わらない
pub unsafe fn dark_control(hwnd: HWND, class: PCWSTR) {
    if !pal().dark || hwnd.is_invalid() {
        return;
    }
    if let Some(api) = dark_api() {
        (api.allow_window)(hwnd, 1);
    }
    let _ = SetWindowTheme(hwnd, class, PCWSTR::null());
}

/// スクロールバーのある部品(一覧、本文、複数行の入力欄)
pub unsafe fn dark_scroll(hwnd: HWND) {
    dark_control(hwnd, w!("DarkMode_Explorer"));
}

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
        Btn::Primary => (if pressed { pal().accent_pressed } else { pal().accent }, pal().accent, pal().on_accent),
        Btn::Secondary => (if pressed { pal().panel } else { pal().bg }, pal().border, pal().text),
        Btn::Danger => (if pressed { pal().danger_soft } else { pal().bg }, pal().danger, pal().danger),
        Btn::Toggle(on) => {
            if on {
                (pal().accent_soft, pal().accent, pal().accent)
            } else {
                (if pressed { pal().panel } else { pal().bg }, pal().border, pal().text)
            }
        }
    };
    let (bg, border, fg) = if disabled { (pal().panel, pal().border, pal().muted) } else { (bg, border, fg) };
    let border = if focused && !disabled && !matches!(kind, Btn::Primary | Btn::Danger) { pal().accent } else { border };

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

/// Wine(Linux などで Windows のアプリを動かす仕組み)の上で動いているか。
/// Windows と動きの違うところを、いくつか手当てしている
pub unsafe fn on_wine() -> bool {
    static WINE: OnceLock<bool> = OnceLock::new();
    *WINE.get_or_init(|| GetModuleHandleW(w!("ntdll.dll")).map_or(false, |m| GetProcAddress(m, s!("wine_get_version")).is_some()))
}

/// 地の色のブラシ(部品の背景)。アプリが終わるまで使うので、1つだけ作る。
/// ウィンドウのクラスの背景には、これではなく、別に作ったブラシを渡すこと(下の `ctl_color` の注を参照)
pub fn bg_brush() -> HBRUSH {
    static B: OnceLock<usize> = OnceLock::new();
    HBRUSH(*B.get_or_init(|| unsafe { CreateSolidBrush(pal().bg).0 as usize }) as *mut c_void)
}

/// 部品のクラス名(小文字)
unsafe fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 32];
    let n = GetClassNameW(h, &mut buf) as usize;
    String::from_utf16_lossy(&buf[..n]).to_ascii_lowercase()
}

/// WM_CTLCOLORSTATIC / WM_CTLCOLOREDIT / WM_CTLCOLORBTN / WM_CTLCOLORLISTBOX の答え:
/// 文字の色を `text` にして、地のブラシを返す(`hwnd` は親、`wparam` `lparam` はメッセージのまま渡す)
pub unsafe fn ctl_color(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, text: COLORREF) -> LRESULT {
    let hdc = HDC(wparam.0 as *mut c_void);
    let p = pal();
    let (mut color, mut brush) = (text, bg_brush());
    match class_of(HWND(lparam.0 as *mut c_void)).as_str() {
        "syslink" => {
            // リンク: 暗い地でも読める色にする(`apply_dark` で、この色を使うように頼んである)
            if p.dark {
                color = p.accent;
            }
            // Wine のリンクの部品は、受け取ったブラシを消してしまう(Windows は消さない)。
            // 共有のブラシを渡すと、ほかの部品の地が塗れなくなるので、Wine の上では使い捨てのブラシを渡す
            if on_wine() {
                brush = CreateSolidBrush(p.bg);
            }
        }
        // 選ぶ欄(コンボボックス)は、枠を Windows が描く。暗い見た目(DarkMode_CFD)が無い環境では、
        // 明るい枠の中に明るい文字を描いてしまわないように、色を変えない
        "combobox" | "combolbox" if p.dark && dark_api().is_none() => return DefWindowProcW(hwnd, msg, wparam, lparam),
        _ => {}
    }
    SetTextColor(hdc, color);
    SetBkColor(hdc, p.bg);
    LRESULT(brush.0 as isize)
}

// ---- 暗い配色: Windows の部品の手当て ------------------------------------------

/// 暗い配色のとき、ウィンドウ `hwnd` と、その中の部品を、暗い見た目にする。
/// 部品をすべて作った後、ウィンドウを表示する前に呼ぶ(明るい配色のときは、何もしない)。
///
/// - タイトルバー、スクロールバー、一覧の選択の色: Windows の暗い見た目を使う(ある環境でだけ)
/// - チェックボックス、一覧の見出し: Windows の部品は文字の色を変えられないので、自前で描く
/// - リンク: 文字の色を、親が決められるようにする
pub unsafe fn apply_dark(hwnd: HWND) {
    if !pal().dark {
        return;
    }
    dark_title(hwnd);
    unsafe extern "system" fn each(child: HWND, _: LPARAM) -> BOOL {
        let p = pal();
        let style = GetWindowLongW(child, GWL_STYLE) as u32;
        match class_of(child).as_str() {
            "syslistview32" => {
                send(child, LVM_SETBKCOLOR, 0, p.bg.0 as isize);
                send(child, LVM_SETTEXTBKCOLOR, 0, p.bg.0 as isize);
                send(child, LVM_SETTEXTCOLOR, 0, p.text.0 as isize);
                dark_scroll(child);
                // 見出しの行(一覧の子。描くときの問い合わせは一覧に届く)と、まわりの枠を、一覧で受けて描く
                let _ = SetWindowSubclass(child, Some(list_subclass), DARK_SUBCLASS, 0);
            }
            "msctls_hotkey32" => {
                let _ = SetWindowSubclass(child, Some(hotkey_subclass), DARK_SUBCLASS, 0);
            }
            "richedit50w" => dark_scroll(child),
            "edit" if style & ES_MULTILINE as u32 != 0 => dark_scroll(child),
            "combobox" => dark_control(child, w!("DarkMode_CFD")),
            "syslink" => dark_link(child),
            // チェックボックス(BS_CHECKBOX = 2、BS_AUTOCHECKBOX = 3)
            "button" if matches!(style & 0xF, 2 | 3) => {
                let _ = SetWindowSubclass(child, Some(check_subclass), DARK_SUBCLASS, 0);
            }
            _ => {}
        }
        TRUE
    }
    let _ = EnumChildWindows(hwnd, Some(each), LPARAM(0));
}

/// 暗い配色のとき、リンクの部品(SysLink)の文字を、親が WM_CTLCOLORSTATIC で決めた色(`ctl_color`)で描かせる。
/// 部品の文字を入れ替えると、この指定は消えるので、入れ替えるたびに呼ぶ
pub unsafe fn dark_link(link: HWND) {
    if !pal().dark || link.is_invalid() {
        return;
    }
    // LIS_DEFAULTCOLORS: リンクの文字の色を、決まった青ではなく、親が決めた色にする
    let mut item = LITEM { mask: LIF_ITEMINDEX | LIF_STATE, iLink: 0, state: LIS_DEFAULTCOLORS, stateMask: LIS_DEFAULTCOLORS, ..Default::default() };
    while item.iLink < 8 && send(link, LM_SETITEM, 0, &item as *const _ as isize).0 != 0 {
        item.iLink += 1;
    }
}

/// `apply_dark` が付けるサブクラスの番号
const DARK_SUBCLASS: usize = 0xDA;

/// 暗い配色のときの、チェックボックス。
/// Windows のチェックボックスは、文字の色を変えられない(黒で描かれる)ので、絵を自前で描く。
/// 部品そのものは Windows のもののままなので、キーやマウスの操作、状態の取得は、そのまま使える
unsafe extern "system" fn check_subclass(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            paint_check(hwnd, hdc);
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        WM_PRINTCLIENT => {
            paint_check(hwnd, HDC(wparam.0 as *mut c_void));
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(check_subclass), DARK_SUBCLASS);
        }
        _ => {}
    }
    let r = DefSubclassProc(hwnd, msg, wparam, lparam);
    // 状態が変わる操作では、部品が自分で(明るい色で)描き直すことがあるので、すぐに上から描き直す
    const BM_SETCHECK: u32 = 0x00F1;
    const BM_SETSTATE: u32 = 0x00F3;
    const WM_MOUSELEAVE: u32 = 0x02A3;
    const WM_UPDATEUISTATE: u32 = 0x0128;
    if matches!(
        msg,
        BM_SETCHECK | BM_SETSTATE | WM_SETFOCUS | WM_KILLFOCUS | WM_ENABLE | WM_SETTEXT | WM_UPDATEUISTATE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_KEYDOWN | WM_KEYUP | WM_MOUSELEAVE | WM_CAPTURECHANGED
    ) {
        let hdc = GetDC(hwnd);
        paint_check(hwnd, hdc);
        ReleaseDC(hwnd, hdc);
    }
    r
}

unsafe fn paint_check(hwnd: HWND, hdc: HDC) {
    let p = pal();
    let dpi = windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd).max(96) as i32;
    let s = |v: i32| scale(v, dpi);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    fill(hdc, &rc, p.bg);
    let checked = send(hwnd, 0x00F0 /* BM_GETCHECK */, 0, 0).0 == 1;
    let pressed = send(hwnd, 0x00F2 /* BM_GETSTATE */, 0, 0).0 & 0x4 != 0; // BST_PUSHED
    let enabled = IsWindowEnabled(hwnd).as_bool();
    let focused = GetFocus() == hwnd;

    // 四角(オンなら塗って、チェックの印を描く)
    let size = s(16);
    let top = rc.top + (rc.bottom - rc.top - size) / 2;
    let boxr = RECT { left: rc.left + 1, top, right: rc.left + 1 + size, bottom: top + size };
    let (fill_c, border_c) = match (enabled, checked) {
        (false, _) => (p.panel, p.border),
        (true, true) => (if pressed { p.accent_pressed } else { p.accent }, p.accent),
        (true, false) => (if pressed { p.panel } else { p.bg }, if focused { p.accent } else { p.muted }),
    };
    round_box(hdc, &boxr, fill_c, border_c, s(5));
    if checked {
        let pen = CreatePen(PS_SOLID, s(2).max(2), if enabled { p.on_accent } else { p.muted });
        let old = SelectObject(hdc, pen);
        let at = |x: i32, y: i32| POINT { x: boxr.left + size * x / 16, y: boxr.top + size * y / 16 };
        let _ = Polyline(hdc, &[at(4, 8), at(7, 11), at(12, 5)]);
        SelectObject(hdc, old);
        let _ = DeleteObject(pen);
    }
    if focused && checked && enabled {
        // 入力の位置が分かるように、外側に1本、線を足す
        let outer = RECT { left: boxr.left - 1, top: boxr.top - 1, right: boxr.right + 1, bottom: boxr.bottom + 1 };
        let pen = CreatePen(PS_SOLID, 1, p.text);
        let (op, ob) = (SelectObject(hdc, pen), SelectObject(hdc, GetStockObject(NULL_BRUSH)));
        let _ = RoundRect(hdc, outer.left, outer.top, outer.right, outer.bottom, s(6), s(6));
        SelectObject(hdc, op);
        SelectObject(hdc, ob);
        let _ = DeleteObject(pen);
    }

    // 文字
    let mut buf = vec![0u16; 256];
    let n = GetWindowTextW(hwnd, &mut buf) as usize;
    let font = HFONT(send(hwnd, WM_GETFONT, 0, 0).0 as *mut c_void);
    let old = SelectObject(hdc, font);
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, if enabled { p.text } else { p.muted });
    let mut tr = RECT { left: boxr.right + s(8), top: rc.top, right: rc.right, bottom: rc.bottom };
    DrawTextW(hdc, &mut buf[..n], &mut tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
    SelectObject(hdc, old);
}

/// 部品のまわりの枠(WS_EX_CLIENTEDGE。Windows は、明るい色の立体の枠を描く)を、暗い配色の枠で塗り直す。
/// 外側の1本を枠の色(入力の位置にあるときは、強調の色)、その内側を地の色にする
unsafe fn paint_dark_edge(hwnd: HWND) {
    if GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_CLIENTEDGE.0 == 0 {
        return;
    }
    let p = pal();
    let mut wr = RECT::default();
    let _ = GetWindowRect(hwnd, &mut wr);
    let (w, h) = (wr.right - wr.left, wr.bottom - wr.top);
    let hdc = GetWindowDC(hwnd);
    let edge = GetSystemMetrics(SM_CXEDGE).max(1);
    for i in 0..edge {
        let color = if i > 0 {
            p.bg
        } else if GetFocus() == hwnd {
            p.accent
        } else {
            p.border
        };
        let brush = CreateSolidBrush(color);
        FrameRect(hdc, &RECT { left: i, top: i, right: w - i, bottom: h - i }, brush);
        let _ = DeleteObject(brush);
    }
    ReleaseDC(hwnd, hdc);
}

/// 暗い配色のときの、ホットキーの欄。Windows の部品は、白い地に黒い文字で描くので、絵を自前で描く。
/// キーの受け付けは、部品のまま(押されているキーは、部品に問い合わせる)
unsafe extern "system" fn hotkey_subclass(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
    const HKM_SETHOTKEY: u32 = 0x0401;
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            paint_hotkey(hwnd, hdc);
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        WM_NCPAINT => {
            let r = DefSubclassProc(hwnd, msg, wparam, lparam);
            paint_dark_edge(hwnd);
            return r;
        }
        WM_NCDESTROY => {
            let _ = RemoveWindowSubclass(hwnd, Some(hotkey_subclass), DARK_SUBCLASS);
        }
        _ => {}
    }
    let r = DefSubclassProc(hwnd, msg, wparam, lparam);
    if matches!(msg, WM_SETFOCUS | WM_KILLFOCUS) {
        paint_dark_edge(hwnd);
    }
    if matches!(msg, WM_SETFOCUS | WM_KILLFOCUS | WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP | WM_CHAR | WM_ENABLE | HKM_SETHOTKEY) {
        let _ = InvalidateRect(hwnd, None, false);
    }
    r
}

/// ホットキーの欄に表示する文字(Windows の部品と同じ書き方: 「Ctrl + Alt + D」。何も無ければ「なし」)
unsafe fn hotkey_label(vk: u32, mods: u32) -> String {
    if vk == 0 && mods == 0 {
        return "なし".to_string();
    }
    let mut s = String::new();
    // 部品の中での修飾キー: Shift = 1、Ctrl = 2、Alt = 4、拡張キー = 8
    for (bit, name) in [(2, "Ctrl"), (1, "Shift"), (4, "Alt")] {
        if mods & bit != 0 {
            s.push_str(name);
            s.push_str(" + ");
        }
    }
    match vk {
        0 => {}
        0x30..=0x39 | 0x41..=0x5A => s.push(vk as u8 as char),
        0x70..=0x87 => s.push_str(&format!("F{}", vk - 0x6F)),
        0x20 => s.push_str("Space"),
        _ => {
            // そのほかのキーは、Windows に名前をたずねる
            let scan = MapVirtualKeyW(vk, MAPVK_VK_TO_VSC);
            let mut buf = [0u16; 64];
            let n = GetKeyNameTextW(((scan << 16) | if mods & 8 != 0 { 1 << 24 } else { 0 }) as i32, &mut buf);
            if n > 0 {
                s.push_str(&String::from_utf16_lossy(&buf[..n as usize]));
            } else {
                s.push_str(&format!("キー {vk:#04X}"));
            }
        }
    }
    s
}

unsafe fn paint_hotkey(hwnd: HWND, hdc: HDC) {
    const HKM_GETHOTKEY: u32 = 0x0402;
    let p = pal();
    let dpi = windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd).max(96) as i32;
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    fill(hdc, &rc, p.bg);
    let v = send(hwnd, HKM_GETHOTKEY, 0, 0).0 as u32;
    let label = hotkey_label(v & 0xFF, (v >> 8) & 0xFF);
    let w16: Vec<u16> = label.encode_utf16().collect();
    let font = HFONT(send(hwnd, WM_GETFONT, 0, 0).0 as *mut c_void);
    let old = SelectObject(hdc, font);
    let mut size = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, &w16, &mut size);
    let (x, y) = (scale(8, dpi), (rc.bottom - size.cy).max(0) / 2);
    SetBkMode(hdc, TRANSPARENT);
    SetTextColor(hdc, if IsWindowEnabled(hwnd).as_bool() { p.text } else { p.muted });
    let _ = TextOutW(hdc, x, y, &w16);
    SelectObject(hdc, old);
    // 入力の位置の印(キャレット)を、文字の後ろに置く(部品は、自分で描くときに置いていたので、代わりに置く)
    if GetFocus() == hwnd {
        let _ = SetCaretPos(x + size.cx + 1, y);
    }
}

/// 暗い配色のときの、一覧。まわりの枠と、見出しの行を描く
/// (Windows の見出しは明るい色で描かれるので、項目を自前で描く)
unsafe extern "system" fn list_subclass(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
    if msg == WM_NCDESTROY {
        let _ = RemoveWindowSubclass(hwnd, Some(list_subclass), DARK_SUBCLASS);
    }
    if matches!(msg, WM_NCPAINT | WM_SETFOCUS | WM_KILLFOCUS) {
        let r = DefSubclassProc(hwnd, msg, wparam, lparam);
        paint_dark_edge(hwnd);
        return r;
    }
    if msg == WM_NOTIFY {
        let hdr = &*(lparam.0 as *const NMHDR);
        let header = HWND(send(hwnd, LVM_GETHEADER, 0, 0).0 as *mut c_void);
        if hdr.code == NM_CUSTOMDRAW && hdr.hwndFrom == header && !header.is_invalid() {
            let cd = &*(lparam.0 as *const NMCUSTOMDRAW);
            let p = pal();
            let dpi = windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd).max(96) as i32;
            match cd.dwDrawStage.0 {
                // CDDS_PREPAINT → 項目ごとに知らせてもらう(CDRF_NOTIFYITEMDRAW)。描き終わりにも(CDRF_NOTIFYPOSTPAINT)
                0x1 => return LRESULT(0x20 | 0x10),
                // CDDS_ITEMPREPAINT: 項目を自前で描く(CDRF_SKIPDEFAULT)
                0x10001 => {
                    let rc = cd.rc;
                    fill(cd.hdc, &rc, p.panel);
                    fill(cd.hdc, &RECT { left: rc.right - 1, top: rc.top + scale(4, dpi), right: rc.right, bottom: rc.bottom - scale(4, dpi) }, p.border);
                    fill(cd.hdc, &RECT { left: rc.left, top: rc.bottom - 1, right: rc.right, bottom: rc.bottom }, p.border);
                    let mut buf = [0u16; 128];
                    let mut item = HDITEMW { mask: HDI_TEXT, pszText: windows::core::PWSTR(buf.as_mut_ptr()), cchTextMax: buf.len() as i32, ..Default::default() };
                    send(header, HDM_GETITEMW, cd.dwItemSpec, &mut item as *mut _ as isize);
                    let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
                    let font = HFONT(send(header, WM_GETFONT, 0, 0).0 as *mut c_void);
                    let old = SelectObject(cd.hdc, font);
                    SetBkMode(cd.hdc, TRANSPARENT);
                    SetTextColor(cd.hdc, p.muted);
                    let mut tr = RECT { left: rc.left + scale(8, dpi), top: rc.top, right: rc.right - scale(6, dpi), bottom: rc.bottom };
                    DrawTextW(cd.hdc, &mut buf[..n], &mut tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX);
                    SelectObject(cd.hdc, old);
                    return LRESULT(0x4);
                }
                // CDDS_POSTPAINT: 最後の項目より右の、空いているところを塗る
                0x2 => {
                    let mut rc = RECT::default();
                    let _ = GetClientRect(header, &mut rc);
                    let count = send(header, HDM_GETITEMCOUNT, 0, 0).0;
                    let mut right = 0;
                    for i in 0..count.max(0) {
                        let mut r = RECT::default();
                        send(header, HDM_GETITEMRECT, i as usize, &mut r as *mut _ as isize);
                        right = right.max(r.right);
                    }
                    if right < rc.right {
                        fill(cd.hdc, &RECT { left: right, top: rc.top, right: rc.right, bottom: rc.bottom }, p.panel);
                        fill(cd.hdc, &RECT { left: right, top: rc.bottom - 1, right: rc.right, bottom: rc.bottom }, p.border);
                    }
                    return LRESULT(0);
                }
                _ => {}
            }
        }
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
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
        let color = if *h == focus { pal().accent } else { pal().border };
        round_box(hdc, rc, pal().bg, color, scale(8, dpi));
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
