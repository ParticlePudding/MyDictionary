//! Win32 GUI (windows-rs)。標準コントロール + オーナードローのフラットなボタン。
//! - 上段に検索(全体 / 詳細 / 辞書の選択)、左に一覧、右に詳細カード
//! - 登録・編集・設定はモーダルダイアログ
//! - ウィンドウを先に表示し、DBの読み込みは別スレッドで行う
//! - トレイ常駐 + グローバルホットキー(設定ダイアログ / 設定ファイルで変更可)

use crate::config::{self, Config};
use crate::core::{self, Entry, Query};
use crate::dialogs::{self, EntryDlg, SettingsDlg};
use crate::ui::{self, quiet, scale, Btn, Fonts};
use crate::ui::{get_text, send, set_text, wide};
use std::collections::HashSet;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Instant;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, GetProcessTimes};
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS_NAME: PCWSTR = w!("FastDictMainWindow");
const ID_EDIT: usize = 100; // 全体検索
const ID_LIST: usize = 101;
const ID_MEANING: usize = 102;
const ID_STATUS: usize = 103;
const ID_COMBO: usize = 110;
const ID_ADV: usize = 111; // 詳細検索の切替
const ID_QW: usize = 112; // カスタム検索: 見出し語
const ID_QM: usize = 113;
const ID_QT: usize = 114;
const ID_EDITBTN: usize = 115;
const ID_NEW: usize = 116;
const ID_SETTINGS: usize = 117;
const ID_DEL: usize = 118;
const ID_DTITLE: usize = 119;
const ID_DMETA: usize = 120;
const HOTKEY_ID: i32 = 1;
const WM_APP_LOADED: u32 = WM_APP + 1;
const WM_APP_TRAY: u32 = WM_APP + 2;
const WM_APP_SHOW: u32 = WM_APP + 3;
const MENU_OPEN: usize = 1;
const MENU_SETTINGS: usize = 2;
const MENU_OPEN_CFG: usize = 3;
const MENU_RELOAD: usize = 4;
const MENU_EXIT: usize = 5;

const EN_SETFOCUS_: usize = 0x0100;
const EN_KILLFOCUS_: usize = 0x0200;
const EN_CHANGE_: usize = 0x0300;
const BN_CLICKED_: usize = 0;
const CBN_SELCHANGE_: usize = 1;
const CB_ADDSTRING: u32 = 0x0143;
const CB_RESETCONTENT: u32 = 0x014B;
const CB_GETCURSEL: u32 = 0x0147;
const CB_SETCURSEL: u32 = 0x014E;
const LVM_SETCOLUMNWIDTH_: u32 = 0x101E;
const LVM_ENSUREVISIBLE_: u32 = 0x1013;
const LVM_SETIMAGELIST_: u32 = 0x1003;

// ---- 計測ログ -------------------------------------------------------------

static START: OnceLock<Instant> = OnceLock::new();
static TIMING: AtomicBool = AtomicBool::new(false);
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

fn tlog(msg: &str) {
    if !TIMING.load(Ordering::Relaxed) {
        return;
    }
    let Some(p) = LOG_PATH.get() else { return };
    let ms = START.get().map_or(0.0, |s| s.elapsed().as_secs_f64() * 1000.0);
    let age = unsafe { process_age_ms() }.map(|a| format!(" / プロセス開始から {a:6.0}ms")).unwrap_or_default();
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = writeln!(f, "[main+{ms:7.1}ms{age}] {msg}");
    }
}

unsafe fn process_age_ms() -> Option<f64> {
    let (mut c, mut e, mut k, mut u) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u).ok()?;
    let now = GetSystemTimeAsFileTime();
    let to = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
    Some(to(now).saturating_sub(to(c)) as f64 / 10_000.0)
}

// ---- 状態 -----------------------------------------------------------------

struct Dict {
    name: String,
    path: PathBuf,
    readonly: bool,
    error: Option<String>,
}

struct Loaded {
    gen: u32,
    entries: Vec<Entry>,
    errors: Vec<Option<String>>,
    read_ms: f64,
    sort_ms: f64,
}

struct InitData {
    cfg: Config,
    cfg_path: PathBuf,
    warns: Vec<String>,
}

struct State {
    // 上段
    edit: HWND,
    combo: HWND,
    adv: HWND,
    qw: HWND,
    qm: HWND,
    qt: HWND,
    btn_new: HWND,
    btn_set: HWND,
    // 一覧
    list: HWND,
    imglist: HIMAGELIST,
    // 詳細カード
    d_title: HWND,
    d_meta: HWND,
    d_meaning: HWND,
    btn_edit: HWND,
    btn_del: HWND,
    status: HWND,

    fonts: Fonts,
    dpi: i32,
    fields: Vec<(RECT, HWND)>,
    panel: RECT,
    status_top: i32,
    white: HBRUSH,
    panel_brush: HBRUSH,

    cfg: Config,
    cfg_path: PathBuf,
    warns: Vec<String>,
    dicts: Vec<Dict>,
    entries: Vec<Entry>,
    matches: Vec<u32>,
    last_query: Option<Query>,
    loaded: bool,
    gen: u32,
    advanced: bool,
    notice: Option<String>,
    cell: Vec<u16>,
}

fn app_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("fastdict")
}

/// 設定ファイルの場所: exe と同じフォルダにあればそれ(ポータブル)、なければ %AppData%\fastdict
fn config_location() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("fastdict.ini");
            if p.exists() {
                return p;
            }
        }
    }
    app_dir().join("fastdict.ini")
}

fn load_config() -> InitData {
    let path = config_location();
    let base = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let mut warns = Vec::new();
    if !path.exists() {
        let _ = std::fs::create_dir_all(&base);
        if let Err(e) = std::fs::write(&path, config::default_ini()) {
            warns.push(format!("設定ファイルを作成できません: {e}"));
        }
    }
    let cfg = match std::fs::read_to_string(&path) {
        Ok(text) => {
            let (cfg, w) = config::parse(&text, &base, &|k| std::env::var(k).ok());
            warns.extend(w);
            cfg
        }
        Err(e) => {
            warns.push(format!("設定ファイルを読み込めません(既定を使用): {e}"));
            Config::default_for(&base)
        }
    };
    InitData { cfg, cfg_path: path, warns }
}

unsafe fn state<'a>(hwnd: HWND) -> Option<&'a mut State> {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
    if p.is_null() { None } else { Some(&mut *p) }
}

// ---- 起動 -----------------------------------------------------------------

pub fn run() {
    unsafe {
        let _ = START.set(Instant::now());
        let args: Vec<String> = std::env::args().collect();
        let background = args.iter().any(|a| a == "--background");

        // 二重起動: 既存ウィンドウを呼び出して終了
        let _mutex = CreateMutexW(None, true, w!("Local\\FastDictSingleInstance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(existing) = FindWindowW(CLASS_NAME, PCWSTR::null()) {
                let _ = PostMessageW(existing, WM_APP_SHOW, WPARAM(0), LPARAM(0));
            }
            return;
        }

        let init = load_config();
        if init.cfg.timing_log || args.iter().any(|a| a == "--timing") {
            let p = init.cfg_path.with_file_name("timing.log");
            let _ = std::fs::write(&p, "");
            let _ = LOG_PATH.set(p);
            TIMING.store(true, Ordering::Relaxed);
        }
        tlog("設定読み込み完了");

        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES | ICC_HOTKEY_CLASS,
        };
        let _ = InitCommonControlsEx(&icc);

        let hinst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap(),
            hbrBackground: CreateSolidBrush(ui::WHITE),
            lpszClassName: CLASS_NAME,
            hIcon: LoadIconW(None, IDI_APPLICATION).unwrap(),
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let init_ptr = Box::into_raw(Box::new(init));
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            w!("辞書"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1080,
            720,
            None,
            None,
            hinst,
            Some(init_ptr as *const c_void),
        )
        .unwrap();
        tlog("ウィンドウ作成完了");

        // ウィンドウを先に出す。読み込みは裏で。
        if !background {
            show_window(hwnd);
            tlog("ウィンドウ表示");
        }
        add_tray_icon(hwnd);
        if let Some(st) = state(hwnd) {
            if let Some(w) = register_hotkey(hwnd, st) {
                st.warns.push(w);
            }
            set_tray_tip(hwnd, st);
            start_load(hwnd, st);
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_KEYDOWN && GetActiveWindow() == hwnd {
                let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                let vk = msg.wParam.0 as u16;
                let cmd = if ctrl && vk == 'N' as u16 {
                    Some(ID_NEW)
                } else if ctrl && vk == VK_OEM_COMMA.0 {
                    Some(ID_SETTINGS)
                } else if vk == VK_F2.0 {
                    Some(ID_EDITBTN)
                } else {
                    None
                };
                if let Some(c) = cmd {
                    let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(c | BN_CLICKED_ << 16), LPARAM(0));
                    continue;
                }
                if ctrl && vk == 'F' as u16 {
                    if let Some(st) = state(hwnd) {
                        let _ = SetFocus(st.edit);
                        send(st.edit, EM_SETSEL, 0, -1);
                    }
                    continue;
                }
            }
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

/// `--bench` の結果を表示する(ファイルにも保存)
pub fn show_report(report: &str) {
    unsafe {
        let p = app_dir().join("bench.txt");
        let _ = std::fs::create_dir_all(app_dir());
        let _ = std::fs::write(&p, report);
        let text = format!("{report}\r\n(保存先: {})", p.display());
        let (t, c) = (wide(&text), wide("fastdict ベンチマーク"));
        MessageBoxW(None, PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), MB_OK | MB_ICONINFORMATION);
    }
}

unsafe fn show_window(hwnd: HWND) {
    let _ = ShowWindow(hwnd, SW_SHOW);
    if IsIconic(hwnd).as_bool() {
        let _ = ShowWindow(hwnd, SW_RESTORE);
    }
    let _ = SetForegroundWindow(hwnd);
    if let Some(st) = state(hwnd) {
        let _ = SetFocus(st.edit);
        send(st.edit, EM_SETSEL, 0, -1);
    }
}

unsafe fn register_hotkey(hwnd: HWND, st: &mut State) -> Option<String> {
    let _ = UnregisterHotKey(hwnd, HOTKEY_ID);
    let h = st.cfg.hotkey.as_ref()?;
    if RegisterHotKey(hwnd, HOTKEY_ID, HOT_KEY_MODIFIERS(h.mods | MOD_NOREPEAT.0), h.vk).is_err() {
        return Some(format!("ホットキー {} を登録できません(他のアプリが使用中の可能性。設定で変更できます)", h.text));
    }
    None
}

unsafe fn tray_data(hwnd: HWND, tip: &str) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_APP_TRAY,
        hIcon: LoadIconW(None, IDI_APPLICATION).unwrap(),
        ..Default::default()
    };
    for (i, c) in tip.encode_utf16().take(120).enumerate() {
        nid.szTip[i] = c;
    }
    nid
}

unsafe fn add_tray_icon(hwnd: HWND) {
    let _ = Shell_NotifyIconW(NIM_ADD, &tray_data(hwnd, "辞書"));
}

unsafe fn set_tray_tip(hwnd: HWND, st: &State) {
    let tip = match &st.cfg.hotkey {
        Some(h) => format!("辞書 ({})", h.text),
        None => "辞書".to_string(),
    };
    let _ = Shell_NotifyIconW(NIM_MODIFY, &tray_data(hwnd, &tip));
}

unsafe fn remove_tray_icon(hwnd: HWND) {
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    };
    let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
}

// ---- 読み込み -------------------------------------------------------------

/// 設定の辞書一覧から状態を作り直し、別スレッドで全辞書を読み込む
unsafe fn start_load(hwnd: HWND, st: &mut State) {
    st.gen += 1;
    st.loaded = false;
    st.entries.clear();
    st.matches.clear();
    st.last_query = None;
    st.dicts = st
        .cfg
        .dicts
        .iter()
        .map(|d| Dict { name: d.name.clone(), path: d.path.clone(), readonly: d.readonly, error: None })
        .collect();
    fill_combo(st);
    layout(hwnd, st); // 辞書が複数のときだけ「辞書」列を出す
    send(st.list, LVM_SETITEMCOUNT, 0, 0);
    update_detail(st);
    update_status(st);

    let specs: Vec<(PathBuf, bool)> = st.dicts.iter().map(|d| (d.path.clone(), d.readonly)).collect();
    let (h, gen) = (hwnd.0 as usize, st.gen);
    std::thread::spawn(move || {
        let t = Instant::now();
        let mut entries: Vec<Entry> = Vec::new();
        let mut errors = Vec::new();
        for (i, (path, ro)) in specs.iter().enumerate() {
            let r = if *ro { core::open_ro(path) } else { core::open(path) }.and_then(|c| core::load_all(&c));
            match r {
                Ok(mut v) => {
                    for e in &mut v {
                        e.dict = i;
                    }
                    entries.append(&mut v);
                    errors.push(None);
                }
                Err(e) => errors.push(Some(e.to_string())),
            }
        }
        let read_ms = t.elapsed().as_secs_f64() * 1000.0;
        let t = Instant::now();
        core::sort_entries(&mut entries);
        let sort_ms = t.elapsed().as_secs_f64() * 1000.0;
        let boxed = Box::into_raw(Box::new(Loaded { gen, entries, errors, read_ms, sort_ms }));
        let hwnd = HWND(h as *mut c_void);
        if PostMessageW(hwnd, WM_APP_LOADED, WPARAM(0), LPARAM(boxed as isize)).is_err() {
            drop(Box::from_raw(boxed));
        }
    });
}

unsafe fn fill_combo(st: &State) {
    let prev = send(st.combo, CB_GETCURSEL, 0, 0).0;
    quiet(|| {
        send(st.combo, CB_RESETCONTENT, 0, 0);
        let all = wide("すべての辞書");
        send(st.combo, CB_ADDSTRING, 0, all.as_ptr() as isize);
        for d in &st.dicts {
            let name = if d.readonly { format!("{}(参照のみ)", d.name) } else { d.name.clone() };
            let w = wide(&name);
            send(st.combo, CB_ADDSTRING, 0, w.as_ptr() as isize);
        }
        let idx = if prev >= 0 && (prev as usize) <= st.dicts.len() { prev as usize } else { 0 };
        send(st.combo, CB_SETCURSEL, idx, 0);
    });
}

// ---- コントロール作成・レイアウト --------------------------------------------

unsafe fn create_child(parent: HWND, class: PCWSTR, ex: u32, style: u32, id: usize) -> HWND {
    let hinst = GetModuleHandleW(None).unwrap();
    CreateWindowExW(
        WINDOW_EX_STYLE(ex),
        class,
        PCWSTR::null(),
        WINDOW_STYLE(style) | WS_CHILD | WS_VISIBLE,
        0,
        0,
        10,
        10,
        parent,
        HMENU(id as *mut c_void),
        hinst,
        None,
    )
    .unwrap()
}

unsafe fn owner_button(parent: HWND, id: usize, text: &str) -> HWND {
    let b = create_child(parent, w!("BUTTON"), 0, WS_TABSTOP.0 | 0x0B /* BS_OWNERDRAW */, id);
    set_text(b, text);
    b
}

unsafe fn make_imglist(list: HWND, dpi: i32) -> HIMAGELIST {
    // 行の高さを広げるために、高さだけ指定した空のイメージリストを割り当てる
    let il = ImageList_Create(1, scale(34, dpi), IMAGELIST_CREATION_FLAGS(0x20 /* ILC_COLOR32 */), 1, 1);
    send(list, LVM_SETIMAGELIST_, 1 /* LVSIL_SMALL */, il.0 as isize);
    il
}

unsafe fn on_create(hwnd: HWND, init: InitData) -> State {
    let dpi = GetDpiForWindow(hwnd).max(96) as i32;
    let fonts = Fonts::new(dpi);
    let mut fields: Vec<(RECT, HWND)> = Vec::new();

    let edit = ui::field(hwnd, ID_EDIT, (0, 0, 10, 10), dpi, false, &mut fields, fonts.search);
    let cue = wide("検索 — 見出し語・意味・タグ(スペース区切りで AND)");
    send(edit, EM_SETCUEBANNER, 1, cue.as_ptr() as isize);
    let combo = create_child(hwnd, w!("COMBOBOX"), 0, WS_TABSTOP.0 | WS_VSCROLL.0 | 3 /* CBS_DROPDOWNLIST */, ID_COMBO);
    let adv = owner_button(hwnd, ID_ADV, "詳細");
    let btn_new = owner_button(hwnd, ID_NEW, "＋ 新規登録");
    let btn_set = owner_button(hwnd, ID_SETTINGS, "設定");
    let mut adv_edit = |id: usize, cue: &str| {
        let h = ui::field(hwnd, id, (0, 0, 10, 10), dpi, false, &mut fields, fonts.base);
        let c = wide(cue);
        send(h, EM_SETCUEBANNER, 1, c.as_ptr() as isize);
        let _ = ShowWindow(h, SW_HIDE);
        h
    };
    let qw = adv_edit(ID_QW, "見出し語");
    let qm = adv_edit(ID_QM, "意味");
    let qt = adv_edit(ID_QT, "タグ(複数はスペース区切りで AND)");

    let list = create_child(
        hwnd,
        w!("SysListView32"),
        0,
        WS_TABSTOP.0 | LVS_REPORT as u32 | LVS_OWNERDATA as u32 | LVS_SHOWSELALWAYS as u32,
        ID_LIST,
    );
    send(list, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP) as isize);
    let _ = SetWindowTheme(list, w!("Explorer"), PCWSTR::null());
    for (i, title) in ["見出し語", "意味", "タグ", "辞書"].iter().enumerate() {
        let mut t = wide(title);
        let col = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM,
            cx: 100,
            pszText: PWSTR(t.as_mut_ptr()),
            iSubItem: i as i32,
            ..Default::default()
        };
        send(list, LVM_INSERTCOLUMNW, i, &col as *const _ as isize);
    }
    let imglist = make_imglist(list, dpi);

    let d_title = create_child(hwnd, w!("STATIC"), 0, 0x4000 /* SS_ENDELLIPSIS */ | 0x80 /* SS_NOPREFIX */, ID_DTITLE);
    let d_meta = create_child(hwnd, w!("STATIC"), 0, 0x80, ID_DMETA);
    let d_meaning = create_child(hwnd, w!("EDIT"), 0, WS_VSCROLL.0 | ES_MULTILINE as u32 | ES_READONLY as u32 | ES_AUTOVSCROLL as u32, ID_MEANING);
    let btn_edit = owner_button(hwnd, ID_EDITBTN, "編集");
    let btn_del = owner_button(hwnd, ID_DEL, "削除");
    let status = create_child(hwnd, w!("STATIC"), 0, 0x0000000C | 0x80 /* SS_LEFTNOWORDWRAP | SS_NOPREFIX */, ID_STATUS);

    let st = State {
        edit,
        combo,
        adv,
        qw,
        qm,
        qt,
        btn_new,
        btn_set,
        list,
        imglist,
        d_title,
        d_meta,
        d_meaning,
        btn_edit,
        btn_del,
        status,
        fonts,
        dpi,
        fields,
        panel: RECT::default(),
        status_top: 0,
        white: CreateSolidBrush(ui::WHITE),
        panel_brush: CreateSolidBrush(ui::PANEL),
        warns: init.warns,
        cfg: init.cfg,
        cfg_path: init.cfg_path,
        dicts: Vec::new(),
        entries: Vec::new(),
        matches: Vec::new(),
        last_query: None,
        loaded: false,
        gen: 0,
        advanced: false,
        notice: None,
        cell: Vec::new(),
    };
    apply_fonts(&st);
    let _ = SetFocus(edit);
    st
}

unsafe fn apply_fonts(st: &State) {
    let f = &st.fonts;
    for (h, font) in [
        (st.edit, f.search),
        (st.combo, f.base),
        (st.qw, f.base),
        (st.qm, f.base),
        (st.qt, f.base),
        (st.list, f.base),
        (st.d_title, f.title),
        (st.d_meta, f.small),
        (st.d_meaning, f.body),
        (st.status, f.small),
    ] {
        send(h, WM_SETFONT, font.0 as usize, 1);
    }
}

unsafe fn layout(hwnd: HWND, st: &mut State) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let dpi = st.dpi;
    let s = |v: i32| scale(v, dpi);
    let (m, gap, box_h) = (s(16), s(8), s(40));
    let w = rc.right - 2 * m;

    // --- 上段: 検索欄 | 辞書 | 詳細 | ＋新規登録 | 設定 ---
    let (combo_w, adv_w, new_w, set_w) = (s(190), s(70), s(128), s(64));
    let search_w = (w - combo_w - adv_w - new_w - set_w - 4 * gap).max(s(120));
    let y = s(14);
    ui::place_field(st.edit, (m, y, search_w, box_h), dpi, false, &mut st.fields);
    let mut x = m + search_w + gap;
    let _ = MoveWindow(st.combo, x, y + (box_h - s(32)) / 2, combo_w, s(300), true);
    x += combo_w + gap;
    let _ = MoveWindow(st.adv, x, y, adv_w, box_h, true);
    x += adv_w + gap;
    let _ = MoveWindow(st.btn_new, x, y, new_w, box_h, true);
    x += new_w + gap;
    let _ = MoveWindow(st.btn_set, x, y, set_w, box_h, true);
    let mut top = y + box_h + s(12);

    // --- 詳細検索(列ごと) ---
    let adv_cmd = if st.advanced { SW_SHOW } else { SW_HIDE };
    for h in [st.qw, st.qm, st.qt] {
        let _ = ShowWindow(h, adv_cmd);
    }
    if st.advanced {
        let fh = s(36);
        let cw = (w - 2 * gap) / 3;
        ui::place_field(st.qw, (m, top, cw, fh), dpi, false, &mut st.fields);
        ui::place_field(st.qm, (m + cw + gap, top, cw, fh), dpi, false, &mut st.fields);
        ui::place_field(st.qt, (m + 2 * (cw + gap), top, w - 2 * (cw + gap), fh), dpi, false, &mut st.fields);
        top += fh + s(12);
    }

    // --- 下端のステータスバー ---
    let status_h = s(30);
    st.status_top = rc.bottom - status_h;
    let _ = MoveWindow(st.status, m, st.status_top + s(5), w, s(20), true);
    let bottom = st.status_top - s(12);

    // --- 左: 一覧 / 右: 詳細カード ---
    let list_w = (w * 56 / 100).max(s(320));
    let list_h = (bottom - top).max(s(80));
    let _ = MoveWindow(st.list, m, top, list_w, list_h, true);
    // 列幅(辞書が複数のときだけ「辞書」列を出す)
    let inner = list_w - s(22);
    let multi = st.dicts.len() > 1;
    let widths: [i32; 4] = if multi {
        [inner * 28 / 100, inner * 40 / 100, inner * 18 / 100, inner * 14 / 100]
    } else {
        [inner * 30 / 100, inner * 50 / 100, inner * 20 / 100, 0]
    };
    for (i, wd) in widths.iter().enumerate() {
        send(st.list, LVM_SETCOLUMNWIDTH_, i, *wd as isize);
    }

    let px = m + list_w + s(12);
    st.panel = RECT { left: px, top, right: rc.right - m, bottom: top + list_h };
    let pad = s(22);
    let (ix, iw) = (px + pad, (st.panel.right - st.panel.left) - 2 * pad);
    let btn_h = s(36);
    let _ = MoveWindow(st.d_title, ix, top + s(18), iw, s(34), true);
    let _ = MoveWindow(st.d_meta, ix, top + s(58), iw, s(40), true);
    let meaning_top = top + s(108);
    let btn_y = top + list_h - pad - btn_h;
    let _ = MoveWindow(st.d_meaning, ix, meaning_top, iw, (btn_y - s(12) - meaning_top).max(s(30)), true);
    let (bw, bgap) = (s(112), s(10));
    let _ = MoveWindow(st.btn_del, ix + iw - bw, btn_y, bw, btn_h, true);
    let _ = MoveWindow(st.btn_edit, ix + iw - 2 * bw - bgap, btn_y, bw, btn_h, true);
    let _ = InvalidateRect(hwnd, None, false);
}

unsafe fn paint(hwnd: HWND, st: &State) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    ui::fill(hdc, &rc, ui::WHITE);
    // ステータスバー上の区切り線
    ui::fill(hdc, &RECT { left: 0, top: st.status_top, right: rc.right, bottom: st.status_top + 1 }, ui::BORDER);
    // 詳細カード
    ui::round_box(hdc, &st.panel, ui::PANEL, ui::BORDER, scale(12, st.dpi));
    ui::paint_fields(hdc, &st.fields, st.dpi);
    let _ = EndPaint(hwnd, &ps);
}

// ---- 表示更新 -------------------------------------------------------------

unsafe fn build_query(st: &State) -> Query {
    let sel = send(st.combo, CB_GETCURSEL, 0, 0).0;
    Query {
        all: get_text(st.edit),
        word: if st.advanced { get_text(st.qw) } else { String::new() },
        meaning: if st.advanced { get_text(st.qm) } else { String::new() },
        tag: if st.advanced { get_text(st.qt) } else { String::new() },
        dict: if sel > 0 { Some(sel as usize - 1) } else { None },
    }
}

unsafe fn selected_rows(st: &State) -> Vec<usize> {
    let mut rows = Vec::new();
    let mut i = usize::MAX; // -1 から開始
    loop {
        let n = send(st.list, LVM_GETNEXTITEM, i, LVNI_SELECTED as isize).0;
        if n < 0 {
            break;
        }
        rows.push(n as usize);
        i = n as usize;
    }
    rows
}

/// 一覧の選択行 → entries の添字
unsafe fn selected_entries(st: &State) -> Vec<usize> {
    selected_rows(st).into_iter().filter_map(|r| st.matches.get(r)).map(|&i| i as usize).collect()
}

unsafe fn select_row(st: &State, row: Option<usize>) {
    quiet(|| {
        let clear = LVITEMW {
            stateMask: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
            state: LIST_VIEW_ITEM_STATE_FLAGS(0),
            ..Default::default()
        };
        send(st.list, LVM_SETITEMSTATE, usize::MAX, &clear as *const _ as isize);
        if let Some(r) = row {
            let on = LVITEMW {
                stateMask: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
                state: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
                ..Default::default()
            };
            send(st.list, LVM_SETITEMSTATE, r, &on as *const _ as isize);
            send(st.list, LVM_ENSUREVISIBLE_, r, 0);
        }
    });
}

/// 指定のレコードを選択して見える位置へ
unsafe fn select_entry(st: &mut State, dict: usize, id: i64) {
    let row = st.matches.iter().position(|&i| {
        let e = &st.entries[i as usize];
        e.dict == dict && e.id == id
    });
    select_row(st, row);
    update_detail(st);
}

unsafe fn refresh(st: &mut State) {
    let t = Instant::now();
    st.notice = None;
    let q = build_query(st);
    // 文字を足しただけなら、前回のヒットの中だけを調べる
    let narrowed = st.last_query.as_ref().map_or(false, |p| core::narrows(p, &q));
    st.matches = if narrowed {
        let mut cand = st.matches.clone();
        cand.sort_unstable();
        core::filter_in(&st.entries, &q, Some(&cand))
    } else {
        core::filter(&st.entries, &q)
    };
    st.last_query = Some(q);
    let filter_ms = t.elapsed().as_secs_f64() * 1000.0;

    quiet(|| {
        send(st.list, LVM_SETITEMCOUNT, st.matches.len(), 0);
    });
    select_row(st, if st.matches.is_empty() { None } else { Some(0) });
    let _ = InvalidateRect(st.list, None, true);
    update_detail(st);
    update_status(st);
    if TIMING.load(Ordering::Relaxed) {
        tlog(&format!(
            "検索 {} 件ヒット: 絞り込み {:.2}ms{} / 反映込み {:.2}ms",
            st.matches.len(),
            filter_ms,
            if narrowed { "(前回結果内)" } else { "" },
            t.elapsed().as_secs_f64() * 1000.0
        ));
    }
}

unsafe fn update_status(st: &State) {
    let errs: Vec<String> = st
        .dicts
        .iter()
        .filter_map(|d| d.error.as_ref().map(|e| format!("辞書「{}」を読み込めません: {}", d.name, e)))
        .collect();
    let text = if !st.loaded {
        "読み込み中…".to_string()
    } else {
        let last = core::last_update(&st.entries).map(core::format_jst).unwrap_or_else(|| "-".into());
        format!("{} 件 / 全 {} 件   ·   最終更新 {}", st.matches.len(), st.entries.len(), last)
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(n) = &st.notice {
        parts.push(n.clone());
    }
    parts.extend(errs);
    parts.push(text);
    set_text(st.status, &parts.join("    |    "));
}

unsafe fn notice(st: &mut State, msg: &str) {
    st.notice = Some(msg.to_string());
    update_status(st);
}

fn writable(d: &Dict) -> Result<(), String> {
    if d.readonly {
        return Err(format!("辞書「{}」は参照のみです(設定で変更できます)", d.name));
    }
    if d.error.is_some() {
        return Err(format!("辞書「{}」を読み込めていないため書き込めません", d.name));
    }
    Ok(())
}

/// 詳細カード: 0件 / 1件(内容) / 複数(件数)
unsafe fn update_detail(st: &State) {
    let sel = selected_entries(st);
    let (title, meta, meaning): (String, String, String) = match sel.len() {
        0 => {
            if !st.loaded {
                ("読み込み中…".into(), String::new(), String::new())
            } else if st.entries.is_empty() {
                ("まだ単語がありません".into(), "右上の「＋ 新規登録」(Ctrl+N)から最初の単語を追加できます。".into(), String::new())
            } else if st.matches.is_empty() {
                ("見つかりません".into(), "検索語を変えるか、「＋ 新規登録」で追加できます。".into(), String::new())
            } else {
                (String::new(), String::new(), String::new())
            }
        }
        1 => {
            let e = &st.entries[sel[0]];
            let mut parts: Vec<String> = Vec::new();
            if !e.tags.is_empty() {
                parts.push(e.tags.split(' ').map(|t| format!("#{t}")).collect::<Vec<_>>().join(" "));
            }
            if st.dicts.len() > 1 {
                parts.push(st.dicts.get(e.dict).map_or("?".into(), |d| d.name.clone()));
            }
            parts.push(format!("更新 {}", core::format_jst(e.updated_at)));
            (e.word.clone(), parts.join("    ·    "), e.meaning.replace("\r\n", "\n").replace('\n', "\r\n"))
        }
        n => (format!("{n} 件を選択中"), "Delete キーまたは「削除」でまとめて削除できます。".into(), String::new()),
    };
    set_text(st.d_title, &title);
    set_text(st.d_meta, &meta);
    set_text(st.d_meaning, &meaning);
    let can_edit = sel.len() == 1 && writable(&st.dicts[st.entries[sel[0]].dict]).is_ok();
    let can_del = !sel.is_empty() && sel.iter().all(|&i| writable(&st.dicts[st.entries[i].dict]).is_ok());
    let _ = EnableWindow(st.btn_edit, can_edit);
    let _ = EnableWindow(st.btn_del, can_del);
    set_text(st.btn_del, &if sel.len() > 1 { format!("{} 件を削除", sel.len()) } else { "削除".to_string() });
    let _ = InvalidateRect(st.btn_edit, None, false);
    let _ = InvalidateRect(st.btn_del, None, false);
}

// ---- 操作: 新規登録 / 編集 / 削除 / 設定 -----------------------------------------

unsafe fn message_box(hwnd: HWND, text: &str, caption: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    let (t, c) = (wide(text), wide(caption));
    MessageBoxW(hwnd, PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), style)
}

unsafe fn do_new(hwnd: HWND, st: &mut State) {
    if !st.loaded {
        notice(st, "データベースの読み込みが完了していません");
        return;
    }
    let targets: Vec<usize> = (0..st.dicts.len()).filter(|&i| writable(&st.dicts[i]).is_ok()).collect();
    if targets.is_empty() {
        notice(st, "書き込める辞書がありません(設定で確認してください)");
        return;
    }
    // 既定の登録先: ドロップダウンで選んでいる辞書(書き込み可なら)。なければ先頭
    let sel = send(st.combo, CB_GETCURSEL, 0, 0).0;
    let choice = if sel > 0 { targets.iter().position(|&t| t == sel as usize - 1).unwrap_or(0) } else { 0 };
    // 検索して見つからなかった語を、見出し語の初期値にする
    let q = get_text(st.edit).trim().to_string();
    let word = if st.matches.is_empty() && !q.contains(char::is_whitespace) { q } else { String::new() };
    let mut dlg = EntryDlg {
        title: "新規登録".into(),
        word,
        tags: String::new(),
        meaning: String::new(),
        choices: targets.iter().map(|&i| st.dicts[i].name.clone()).collect(),
        choice,
        show_choice: targets.len() > 1,
        ok: false,
    };
    if !dialogs::entry_dialog(hwnd, &mut dlg) {
        return;
    }
    let d = targets[dlg.choice];
    match core::open(&st.dicts[d].path).and_then(|c| core::insert(&c, &dlg.word, &dlg.meaning, &dlg.tags)) {
        Ok(entry) => {
            let id = entry.id;
            st.entries.push(entry.with_dict(d));
            core::sort_entries(&mut st.entries);
            refresh(st);
            select_entry(st, d, id);
            let msg = if st.dicts.len() > 1 {
                format!("「{}」を「{}」に登録しました", dlg.word, st.dicts[d].name)
            } else {
                format!("「{}」を登録しました", dlg.word)
            };
            notice(st, &msg);
        }
        Err(e) => {
            message_box(hwnd, &format!("登録に失敗しました: {e}"), "エラー", MB_OK | MB_ICONERROR);
        }
    }
}

unsafe fn do_edit(hwnd: HWND, st: &mut State) {
    let sel = selected_entries(st);
    if sel.len() != 1 {
        notice(st, if sel.is_empty() { "編集する行を選択してください" } else { "編集できるのは1件ずつです" });
        return;
    }
    let e = st.entries[sel[0]].clone();
    if let Err(m) = writable(&st.dicts[e.dict]) {
        notice(st, &m);
        return;
    }
    let mut dlg = EntryDlg {
        title: "編集".into(),
        word: e.word.clone(),
        tags: e.tags.clone(),
        meaning: e.meaning.clone(),
        choices: Vec::new(),
        choice: 0,
        show_choice: false,
        ok: false,
    };
    if !dialogs::entry_dialog(hwnd, &mut dlg) {
        return;
    }
    match core::open(&st.dicts[e.dict].path).and_then(|c| core::update(&c, e.id, &dlg.word, &dlg.meaning, &dlg.tags)) {
        Ok(ts) => {
            if let Some(x) = st.entries.iter_mut().find(|x| x.dict == e.dict && x.id == e.id) {
                *x = Entry::new(e.id, dlg.word.clone(), dlg.meaning.clone(), dlg.tags.clone(), ts).with_dict(e.dict);
            }
            core::sort_entries(&mut st.entries);
            refresh(st);
            select_entry(st, e.dict, e.id);
            notice(st, &format!("「{}」を更新しました", dlg.word));
        }
        Err(err) => {
            message_box(hwnd, &format!("更新に失敗しました: {err}"), "エラー", MB_OK | MB_ICONERROR);
        }
    }
}

unsafe fn do_delete(hwnd: HWND, st: &mut State) {
    let sel = selected_entries(st);
    if sel.is_empty() {
        notice(st, "削除する行を選択してください");
        return;
    }
    for &i in &sel {
        if let Err(m) = writable(&st.dicts[st.entries[i].dict]) {
            notice(st, &m);
            return;
        }
    }
    let mut text = if sel.len() == 1 {
        format!("「{}」を削除しますか?", st.entries[sel[0]].word)
    } else {
        let mut t = format!("選択した {} 件を削除しますか?\n\n", sel.len());
        for &i in sel.iter().take(5) {
            t.push_str(&format!("  ・{}\n", st.entries[i].word));
        }
        if sel.len() > 5 {
            t.push_str(&format!("  …ほか {} 件\n", sel.len() - 5));
        }
        t
    };
    text.push_str("\nこの操作は元に戻せません。");
    if message_box(hwnd, &text, "削除の確認", MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) != IDYES {
        return;
    }

    // 辞書ごとにまとめて1トランザクションで削除
    let mut by_dict: Vec<Vec<i64>> = vec![Vec::new(); st.dicts.len()];
    for &i in &sel {
        by_dict[st.entries[i].dict].push(st.entries[i].id);
    }
    let mut removed: HashSet<(usize, i64)> = HashSet::new();
    let mut errors: Vec<String> = Vec::new();
    for (d, ids) in by_dict.iter().enumerate() {
        if ids.is_empty() {
            continue;
        }
        match core::open(&st.dicts[d].path).and_then(|mut c| core::delete_many(&mut c, ids)) {
            Ok(_) => removed.extend(ids.iter().map(|&id| (d, id))),
            Err(e) => errors.push(format!("辞書「{}」: {}", st.dicts[d].name, e)),
        }
    }
    st.entries.retain(|e| !removed.contains(&(e.dict, e.id)));
    refresh(st);
    if errors.is_empty() {
        notice(st, &format!("{} 件を削除しました", removed.len()));
    } else {
        message_box(hwnd, &format!("一部の削除に失敗しました:\n{}", errors.join("\n")), "エラー", MB_OK | MB_ICONERROR);
    }
}

unsafe fn do_settings(hwnd: HWND, st: &mut State) {
    let mut dlg = SettingsDlg { cfg: st.cfg.clone(), autostart: dialogs::autostart_enabled(), ok: false };
    if !dialogs::settings_dialog(hwnd, &mut dlg) {
        return;
    }
    let mut problems: Vec<String> = Vec::new();
    if dlg.autostart != dialogs::autostart_enabled() {
        if let Err(e) = dialogs::set_autostart(dlg.autostart) {
            problems.push(format!("自動起動の設定に失敗しました: {e}"));
        }
    }
    let dicts_changed = dlg.cfg.dicts != st.cfg.dicts;
    let base = st.cfg_path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    if let Err(e) = std::fs::write(&st.cfg_path, config::to_ini(&dlg.cfg, &base)) {
        problems.push(format!("設定ファイルを保存できません: {e}"));
    }
    st.cfg = dlg.cfg;
    st.warns.clear();
    if let Some(w) = register_hotkey(hwnd, st) {
        problems.push(w);
    }
    set_tray_tip(hwnd, st);
    if dicts_changed {
        st.warns = problems.clone();
        start_load(hwnd, st);
    } else {
        refresh(st);
    }
    if problems.is_empty() {
        notice(st, "設定を保存しました");
    } else {
        notice(st, &problems.join(" / "));
    }
}

// ---- 設定の再読み込み(ファイルを直接編集したとき) --------------------------------

unsafe fn reload_config(hwnd: HWND, st: &mut State) {
    let init = load_config();
    st.cfg = init.cfg;
    st.cfg_path = init.cfg_path;
    st.warns = init.warns;
    if let Some(w) = register_hotkey(hwnd, st) {
        st.warns.push(w);
    }
    set_tray_tip(hwnd, st);
    start_load(hwnd, st);
    tlog("設定を再読み込み");
}

unsafe fn open_config(hwnd: HWND, st: &State) {
    let p = wide(&st.cfg_path.to_string_lossy());
    ShellExecuteW(hwnd, w!("open"), PCWSTR(p.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
}

// ---- ウィンドウプロシージャ ---------------------------------------------------

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let init = *Box::from_raw(cs.lpCreateParams as *mut InitData);
            let st = Box::new(on_create(hwnd, init));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(st) as isize);
            if let Some(st) = state(hwnd) {
                update_status(st);
                update_detail(st);
                layout(hwnd, st);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            if let Some(st) = state(hwnd) {
                paint(hwnd, st);
                LRESULT(0)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        WM_GETMINMAXINFO => {
            let mmi = &mut *(lparam.0 as *mut MINMAXINFO);
            let dpi = state(hwnd).map_or(96, |s| s.dpi);
            mmi.ptMinTrackSize.x = scale(860, dpi);
            mmi.ptMinTrackSize.y = scale(520, dpi);
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(st) = state(hwnd) {
                layout(hwnd, st);
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let rc = &*(lparam.0 as *const RECT);
            let _ = SetWindowPos(hwnd, None, rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top, SWP_NOZORDER | SWP_NOACTIVATE);
            if let Some(st) = state(hwnd) {
                st.dpi = (wparam.0 & 0xFFFF) as i32;
                st.fonts.destroy();
                st.fonts = Fonts::new(st.dpi);
                apply_fonts(st);
                send(st.list, LVM_SETIMAGELIST_, 1, 0);
                let _ = ImageList_Destroy(st.imglist);
                st.imglist = make_imglist(st.list, st.dpi);
                layout(hwnd, st);
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
            let hdc = HDC(wparam.0 as *mut c_void);
            let id = GetDlgCtrlID(HWND(lparam.0 as *mut c_void)) as usize;
            match state(hwnd) {
                Some(st) => match id {
                    ID_DTITLE | ID_MEANING => ui::ctl_color(hdc, ui::TEXT, ui::PANEL, st.panel_brush),
                    ID_DMETA => ui::ctl_color(hdc, ui::MUTED, ui::PANEL, st.panel_brush),
                    ID_STATUS => ui::ctl_color(hdc, ui::MUTED, ui::WHITE, st.white),
                    _ => ui::ctl_color(hdc, ui::TEXT, ui::WHITE, st.white),
                },
                None => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
        WM_DRAWITEM => {
            let dis = &*(lparam.0 as *const DRAWITEMSTRUCT);
            if let Some(st) = state(hwnd) {
                let id = dis.CtlID as usize;
                let (kind, bg) = match id {
                    ID_NEW => (Btn::Primary, ui::WHITE),
                    ID_ADV => (Btn::Toggle(st.advanced), ui::WHITE),
                    ID_DEL => (Btn::Danger, ui::PANEL),
                    ID_EDITBTN => (Btn::Secondary, ui::PANEL),
                    _ => (Btn::Secondary, ui::WHITE),
                };
                ui::draw_button(dis, kind, st.fonts.base, bg, st.dpi);
            }
            LRESULT(1)
        }
        WM_APP_LOADED => {
            let loaded = Box::from_raw(lparam.0 as *mut Loaded);
            if let Some(st) = state(hwnd) {
                if loaded.gen != st.gen {
                    return LRESULT(0); // 古い読み込み結果(設定の再読み込み前)は捨てる
                }
                tlog(&format!(
                    "読み込み完了: {} 件 (読み込み+小文字化 {:.1}ms / 並べ替え {:.1}ms)",
                    loaded.entries.len(),
                    loaded.read_ms,
                    loaded.sort_ms
                ));
                for (i, e) in loaded.errors.iter().enumerate() {
                    if let Some(d) = st.dicts.get_mut(i) {
                        d.error = e.clone();
                    }
                }
                st.entries = loaded.entries;
                st.loaded = true;
                refresh(st); // 読み込み中に入力された語句をここで反映
                if !st.warns.is_empty() {
                    let w = st.warns.join(" / ");
                    notice(st, &w);
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = wparam.0 & 0xFFFF;
            let code = wparam.0 >> 16;
            let Some(st) = state(hwnd) else { return LRESULT(0) };
            // 入力欄のフォーカス枠(青)の更新
            if matches!(id, ID_EDIT | ID_QW | ID_QM | ID_QT) && (code == EN_SETFOCUS_ || code == EN_KILLFOCUS_) {
                ui::invalidate_field(hwnd, &st.fields, HWND(lparam.0 as *mut c_void));
                return LRESULT(0);
            }
            if ui::is_busy() {
                return LRESULT(0);
            }
            match (id, code) {
                (ID_EDIT | ID_QW | ID_QM | ID_QT, EN_CHANGE_) | (ID_COMBO, CBN_SELCHANGE_) => {
                    if st.loaded {
                        refresh(st);
                    }
                }
                (ID_ADV, BN_CLICKED_) => {
                    st.advanced = !st.advanced;
                    if !st.advanced {
                        for h in [st.qw, st.qm, st.qt] {
                            set_text(h, "");
                        }
                    }
                    layout(hwnd, st);
                    let _ = InvalidateRect(st.adv, None, false);
                    if st.loaded {
                        refresh(st);
                    }
                    let _ = SetFocus(if st.advanced { st.qw } else { st.edit });
                }
                (ID_NEW, BN_CLICKED_) => do_new(hwnd, st),
                (ID_EDITBTN, BN_CLICKED_) => do_edit(hwnd, st),
                (ID_DEL, BN_CLICKED_) => do_delete(hwnd, st),
                (ID_SETTINGS, BN_CLICKED_) => do_settings(hwnd, st),
                _ => {
                    if id == IDCANCEL.0 as usize {
                        // Esc: 入力があれば消す。なければウィンドウを隠す
                        let has_input = !get_text(st.edit).is_empty() || (st.advanced && [st.qw, st.qm, st.qt].iter().any(|&h| !get_text(h).is_empty()));
                        if has_input {
                            for h in [st.edit, st.qw, st.qm, st.qt] {
                                set_text(h, "");
                            }
                            if st.loaded {
                                refresh(st);
                            }
                        } else {
                            let _ = ShowWindow(hwnd, SW_HIDE);
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            let hdr = &*(lparam.0 as *const NMHDR);
            if hdr.idFrom == ID_LIST {
                if let Some(st) = state(hwnd) {
                    if hdr.code == LVN_GETDISPINFOW {
                        let di = &mut *(lparam.0 as *mut NMLVDISPINFOW);
                        if di.item.mask.0 & LVIF_TEXT.0 != 0 {
                            let text = st
                                .matches
                                .get(di.item.iItem as usize)
                                .map(|&i| {
                                    let e = &st.entries[i as usize];
                                    match di.item.iSubItem {
                                        0 => e.word.clone(),
                                        1 => e.meaning.replace(['\r', '\n'], " "),
                                        2 => e.tags.clone(),
                                        _ => st.dicts.get(e.dict).map(|d| d.name.clone()).unwrap_or_default(),
                                    }
                                })
                                .unwrap_or_default();
                            st.cell = wide(&text);
                            di.item.pszText = PWSTR(st.cell.as_mut_ptr());
                        }
                    } else if !ui::is_busy() {
                        if hdr.code == LVN_KEYDOWN {
                            let kd = &*(lparam.0 as *const NMLVKEYDOWN);
                            if kd.wVKey == VK_DELETE.0 {
                                do_delete(hwnd, st);
                            } else if kd.wVKey == VK_RETURN.0 {
                                do_edit(hwnd, st);
                            }
                        } else if hdr.code == LVN_ITEMCHANGED {
                            let nm = &*(lparam.0 as *const NMLISTVIEW);
                            if (nm.uOldState ^ nm.uNewState) & LVIS_SELECTED.0 != 0 {
                                update_detail(st);
                            }
                        } else if hdr.code == LVN_ODSTATECHANGED {
                            // 仮想リストでは、範囲選択(Shift+クリック / Ctrl+A)はこの通知で届く
                            update_detail(st);
                        } else if hdr.code == NM_DBLCLK {
                            do_edit(hwnd, st);
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_HOTKEY | WM_APP_SHOW => {
            show_window(hwnd);
            LRESULT(0)
        }
        WM_APP_TRAY => {
            match (lparam.0 & 0xFFFF) as u32 {
                WM_LBUTTONUP => {
                    if IsWindowVisible(hwnd).as_bool() {
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    } else {
                        show_window(hwnd);
                    }
                }
                WM_RBUTTONUP => {
                    if let Ok(menu) = CreatePopupMenu() {
                        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("辞書を開く"));
                        let _ = AppendMenuW(menu, MF_STRING, MENU_SETTINGS, w!("設定…"));
                        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN_CFG, w!("設定ファイルを開く"));
                        let _ = AppendMenuW(menu, MF_STRING, MENU_RELOAD, w!("設定ファイルを再読み込み"));
                        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                        let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("終了"));
                        let mut pt = POINT::default();
                        let _ = GetCursorPos(&mut pt);
                        let _ = SetForegroundWindow(hwnd);
                        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
                        let _ = DestroyMenu(menu);
                        match cmd.0 as usize {
                            MENU_OPEN => show_window(hwnd),
                            MENU_SETTINGS => {
                                show_window(hwnd);
                                if let Some(st) = state(hwnd) {
                                    do_settings(hwnd, st);
                                }
                            }
                            MENU_OPEN_CFG => {
                                if let Some(st) = state(hwnd) {
                                    open_config(hwnd, st);
                                }
                            }
                            MENU_RELOAD => {
                                if let Some(st) = state(hwnd) {
                                    reload_config(hwnd, st);
                                }
                            }
                            MENU_EXIT => {
                                let _ = DestroyWindow(hwnd);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // 設定 close_to_tray が true なら隠すだけ(終了はトレイメニュー)
            let hide = state(hwnd).map_or(true, |st| st.cfg.close_to_tray);
            if hide {
                let _ = ShowWindow(hwnd, SW_HIDE);
            } else {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = UnregisterHotKey(hwnd, HOTKEY_ID);
            remove_tray_icon(hwnd);
            let p = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut State;
            if !p.is_null() {
                let st = Box::from_raw(p);
                st.fonts.destroy();
                let _ = DeleteObject(st.white);
                let _ = DeleteObject(st.panel_brush);
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
