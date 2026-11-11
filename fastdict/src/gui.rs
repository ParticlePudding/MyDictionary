//! Win32 GUI (windows-rs)。標準コントロールのみ使用。
//! - ウィンドウを先に表示し、DBの読み込みは別スレッドで行う
//! - 検索欄の変更ごとにメモリ上で絞り込み、仮想リストビューへ反映
//! - 複数辞書、全体検索 + 列別のカスタム検索、登録/編集/複数選択削除
//! - トレイ常駐 + グローバルホットキー(設定ファイルで変更可)

use crate::config::{self, Config};
use crate::core::{self, Entry, Query};
use std::collections::HashSet;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
const ID_DETAIL: usize = 102;
const ID_STATUS: usize = 103;
const ID_WORD: usize = 104; // 登録: 見出し語
const ID_TAGS: usize = 105;
const ID_MEANING: usize = 106;
const ID_ADD: usize = 107; // 登録 / 更新
const ID_DEL: usize = 108;
const ID_LABEL: usize = 109;
const ID_COMBO: usize = 110;
const ID_ADV: usize = 111; // 詳細検索チェック
const ID_QW: usize = 112; // カスタム検索: 見出し語
const ID_QM: usize = 113;
const ID_QT: usize = 114;
const ID_EDITBTN: usize = 115;
const ID_CLEAR: usize = 116;
const HOTKEY_ID: i32 = 1;
const WM_APP_LOADED: u32 = WM_APP + 1;
const WM_APP_TRAY: u32 = WM_APP + 2;
const WM_APP_SHOW: u32 = WM_APP + 3;
const MENU_OPEN_CFG: usize = 1;
const MENU_RELOAD: usize = 2;
const MENU_EXIT: usize = 3;

const EN_CHANGE_: usize = 0x0300;
const BN_CLICKED_: usize = 0;
const CBN_SELCHANGE_: usize = 1;
const CB_ADDSTRING: u32 = 0x0143;
const CB_RESETCONTENT: u32 = 0x014B;
const CB_GETCURSEL: u32 = 0x0147;
const CB_SETCURSEL: u32 = 0x014E;
const BM_GETCHECK_: u32 = 0x00F0;
const LVM_SETCOLUMNWIDTH_: u32 = 0x101E;

// ---- 計測ログ -------------------------------------------------------------

static START: OnceLock<Instant> = OnceLock::new();
static TIMING: AtomicBool = AtomicBool::new(false);
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();
/// プログラムから入力欄/リストを操作している間、通知(EN_CHANGE など)を無視する
static BUSY: AtomicU32 = AtomicU32::new(0);

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

fn quiet<R>(f: impl FnOnce() -> R) -> R {
    BUSY.fetch_add(1, Ordering::SeqCst);
    let r = f();
    BUSY.fetch_sub(1, Ordering::SeqCst);
    r
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
    edit: HWND,
    combo: HWND,
    adv: HWND,
    qw: HWND,
    qm: HWND,
    qt: HWND,
    list: HWND,
    detail: HWND,
    status: HWND,
    word: HWND,
    tags: HWND,
    meaning: HWND,
    label: HWND,
    btn_add: HWND,
    btn_edit: HWND,
    btn_del: HWND,
    btn_clear: HWND,
    font: HFONT,

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
    /// 編集中のレコード (辞書, id)
    editing: Option<(usize, i64)>,
    notice: Option<String>,
    cell: Vec<u16>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
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

unsafe fn send(h: HWND, msg: u32, w: usize, l: isize) -> LRESULT {
    SendMessageW(h, msg, WPARAM(w), LPARAM(l))
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
            dwICC: ICC_LISTVIEW_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);

        let hinst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap(),
            hbrBackground: HBRUSH((COLOR_BTNFACE.0 + 1) as isize as *mut c_void),
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
            960,
            680,
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
            // 登録欄で Ctrl+Enter → 登録/更新
            if msg.message == WM_KEYDOWN && msg.wParam.0 == VK_RETURN.0 as usize && GetKeyState(VK_CONTROL.0 as i32) < 0 {
                if let Some(st) = state(hwnd) {
                    let f = GetFocus();
                    if f == st.word || f == st.tags || f == st.meaning {
                        let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_ADD | BN_CLICKED_ << 16), LPARAM(0));
                        continue;
                    }
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
        return Some(format!("ホットキー {} を登録できません(他のアプリが使用中の可能性)", h.text));
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
    exit_edit(st);
    st.dicts = st
        .cfg
        .dicts
        .iter()
        .map(|d| Dict { name: d.name.clone(), path: d.path.clone(), readonly: d.readonly, error: None })
        .collect();
    fill_combo(st);
    // 辞書が1つのときは「辞書」列を隠す
    let w = if st.dicts.len() > 1 { GetDpiForWindow(hwnd).max(96) as isize * 110 / 96 } else { 0 };
    send(st.list, LVM_SETCOLUMNWIDTH_, 3, w);
    send(st.list, LVM_SETITEMCOUNT, 0, 0);
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

unsafe fn edit_with_cue(parent: HWND, id: usize, cue: &str) -> HWND {
    let h = create_child(parent, w!("EDIT"), WS_EX_CLIENTEDGE.0, WS_TABSTOP.0 | ES_AUTOHSCROLL as u32, id);
    let c = wide(cue);
    send(h, EM_SETCUEBANNER, 1, c.as_ptr() as isize);
    h
}

unsafe fn on_create(hwnd: HWND, init: InitData) -> State {
    let edit = edit_with_cue(hwnd, ID_EDIT, "全体検索(見出し語・意味・タグ。スペース区切りでAND)");
    let combo = create_child(hwnd, w!("COMBOBOX"), 0, WS_TABSTOP.0 | WS_VSCROLL.0 | 3 /* CBS_DROPDOWNLIST */, ID_COMBO);
    let adv = create_child(hwnd, w!("BUTTON"), 0, WS_TABSTOP.0 | 3 /* BS_AUTOCHECKBOX */, ID_ADV);
    set_text(adv, "詳細");
    let qw = edit_with_cue(hwnd, ID_QW, "見出し語で検索");
    let qm = edit_with_cue(hwnd, ID_QM, "意味で検索");
    let qt = edit_with_cue(hwnd, ID_QT, "タグで検索(複数はスペース区切りでAND)");
    for h in [qw, qm, qt] {
        let _ = ShowWindow(h, SW_HIDE);
    }

    let list = create_child(
        hwnd,
        w!("SysListView32"),
        WS_EX_CLIENTEDGE.0,
        WS_TABSTOP.0 | LVS_REPORT as u32 | LVS_OWNERDATA as u32 | LVS_SHOWSELALWAYS as u32,
        ID_LIST,
    );
    send(
        list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        0,
        (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP) as isize,
    );
    let _ = SetWindowTheme(list, w!("Explorer"), PCWSTR::null());
    for (i, (title, width)) in [("見出し語", 200), ("意味", 440), ("タグ", 120), ("辞書", 110)].iter().enumerate() {
        let mut t = wide(title);
        let col = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM,
            cx: *width,
            pszText: PWSTR(t.as_mut_ptr()),
            iSubItem: i as i32,
            ..Default::default()
        };
        send(list, LVM_INSERTCOLUMNW, i, &col as *const _ as isize);
    }

    let detail = create_child(
        hwnd,
        w!("EDIT"),
        WS_EX_CLIENTEDGE.0,
        WS_VSCROLL.0 | ES_MULTILINE as u32 | ES_READONLY as u32 | ES_AUTOVSCROLL as u32,
        ID_DETAIL,
    );
    let word = edit_with_cue(hwnd, ID_WORD, "見出し語");
    let tags = edit_with_cue(hwnd, ID_TAGS, "タグ(スペースまたはカンマ区切り・任意)");
    let label = create_child(hwnd, w!("STATIC"), 0, 0, ID_LABEL);
    set_text(label, "意味(Ctrl+Enter で登録/更新)");
    let meaning = create_child(
        hwnd,
        w!("EDIT"),
        WS_EX_CLIENTEDGE.0,
        WS_TABSTOP.0 | WS_VSCROLL.0 | ES_MULTILINE as u32 | ES_WANTRETURN as u32 | ES_AUTOVSCROLL as u32,
        ID_MEANING,
    );
    let mk_btn = |id: usize, text: &str| {
        let b = create_child(hwnd, w!("BUTTON"), 0, WS_TABSTOP.0, id);
        set_text(b, text);
        b
    };
    let btn_add = mk_btn(ID_ADD, "登録");
    let btn_clear = mk_btn(ID_CLEAR, "クリア");
    let btn_edit = mk_btn(ID_EDITBTN, "選択を編集");
    let btn_del = mk_btn(ID_DEL, "選択を削除");
    let status = create_child(hwnd, w!("STATIC"), 0, 0x0000000C /* SS_LEFTNOWORDWRAP */, ID_STATUS);

    // システムのメッセージフォント(日本語Windowsでは Yu Gothic UI / Meiryo UI)
    let mut ncm = NONCLIENTMETRICSW {
        cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    let _ = SystemParametersInfoW(
        SPI_GETNONCLIENTMETRICS,
        ncm.cbSize,
        Some(&mut ncm as *mut _ as *mut c_void),
        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
    );
    let font = CreateFontIndirectW(&ncm.lfMessageFont);
    for h in [edit, combo, adv, qw, qm, qt, list, detail, status, word, tags, label, meaning, btn_add, btn_clear, btn_edit, btn_del] {
        send(h, WM_SETFONT, font.0 as usize, 1);
    }
    let _ = SetFocus(edit);

    State {
        edit,
        combo,
        adv,
        qw,
        qm,
        qt,
        list,
        detail,
        status,
        word,
        tags,
        meaning,
        label,
        btn_add,
        btn_edit,
        btn_del,
        btn_clear,
        font,
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
        editing: None,
        notice: None,
        cell: Vec::new(),
    }
}

unsafe fn layout(hwnd: HWND, st: &State) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let dpi = GetDpiForWindow(hwnd).max(96) as i32;
    let s = |v: i32| v * dpi / 96;
    let (m, row_h, status_h, detail_h, label_h, meaning_h) = (s(8), s(28), s(20), s(90), s(18), s(64));
    let (combo_w, adv_w, btn_w, gap) = (s(190), s(64), s(96), s(6));
    let w = rc.right - 2 * m;

    // 上段: 全体検索 | 辞書 | 詳細
    let edit_w = w - combo_w - adv_w - 2 * gap;
    let _ = MoveWindow(st.edit, m, m, edit_w, row_h, true);
    let _ = MoveWindow(st.combo, m + edit_w + gap, m, combo_w, s(300), true);
    let _ = MoveWindow(st.adv, m + edit_w + combo_w + 2 * gap, m, adv_w, row_h, true);
    let mut list_y = m + row_h + m;
    if st.advanced {
        let cw = (w - 2 * gap) / 3;
        let y = m + row_h + gap;
        let _ = MoveWindow(st.qw, m, y, cw, row_h, true);
        let _ = MoveWindow(st.qm, m + cw + gap, y, cw, row_h, true);
        let _ = MoveWindow(st.qt, m + 2 * (cw + gap), y, w - 2 * (cw + gap), row_h, true);
        list_y += row_h + gap;
    }

    // 下から積む: ステータス / 意味+ボタン / ラベル / 見出し語+タグ / 詳細、残りが一覧
    let status_y = rc.bottom - m - status_h;
    let meaning_y = status_y - m - meaning_h;
    let label_y = meaning_y - label_h;
    let word_y = label_y - m / 2 - row_h;
    let detail_y = word_y - m - detail_h;
    let list_h = (detail_y - m - list_y).max(s(40));
    let _ = MoveWindow(st.list, m, list_y, w, list_h, true);
    let _ = MoveWindow(st.detail, m, detail_y, w, detail_h, true);
    let word_w = w * 6 / 10;
    let _ = MoveWindow(st.word, m, word_y, word_w, row_h, true);
    let _ = MoveWindow(st.tags, m + word_w + m, word_y, w - word_w - m, row_h, true);
    let _ = MoveWindow(st.label, m, label_y, w, label_h, true);
    let btns_w = 2 * btn_w + gap;
    let _ = MoveWindow(st.meaning, m, meaning_y, w - btns_w - m, meaning_h, true);
    let bh = (meaning_h - gap) / 2;
    let (x1, x2) = (m + w - btns_w, m + w - btn_w);
    let (y1, y2) = (meaning_y, meaning_y + bh + gap);
    let _ = MoveWindow(st.btn_add, x1, y1, btn_w, bh, true);
    let _ = MoveWindow(st.btn_clear, x1, y2, btn_w, bh, true);
    let _ = MoveWindow(st.btn_edit, x2, y1, btn_w, bh, true);
    let _ = MoveWindow(st.btn_del, x2, y2, btn_w, bh, true);
    let _ = MoveWindow(st.status, m, status_y, w, status_h, true);
}

// ---- 表示更新 -------------------------------------------------------------

unsafe fn get_text(h: HWND) -> String {
    let len = GetWindowTextLengthW(h);
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(h, &mut buf);
    String::from_utf16_lossy(&buf[..n as usize])
}

/// 入力欄への設定(EN_CHANGE を発生させない)
unsafe fn set_text(h: HWND, s: &str) {
    let w = wide(s);
    quiet(|| {
        let _ = SetWindowTextW(h, PCWSTR(w.as_ptr()));
    });
}

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
        // 既存の選択を解除し、先頭行を選択
        let clear = LVITEMW {
            stateMask: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
            state: LIST_VIEW_ITEM_STATE_FLAGS(0),
            ..Default::default()
        };
        send(st.list, LVM_SETITEMSTATE, usize::MAX, &clear as *const _ as isize);
        if !st.matches.is_empty() {
            let first = LVITEMW {
                stateMask: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
                state: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
                ..Default::default()
            };
            send(st.list, LVM_SETITEMSTATE, 0, &first as *const _ as isize);
        }
        let _ = InvalidateRect(st.list, None, true);
    });
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
        format!("{} 件 / 全 {} 件  最終更新: {}", st.matches.len(), st.entries.len(), last)
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(n) = &st.notice {
        parts.push(n.clone());
    }
    parts.extend(errs);
    parts.push(text);
    set_text(st.status, &parts.join("  |  "));
}

unsafe fn notice(st: &mut State, msg: &str) {
    st.notice = Some(msg.to_string());
    update_status(st);
}

/// 詳細欄: 1件選択ならその内容、複数なら件数
unsafe fn update_detail(st: &State) {
    let sel = selected_entries(st);
    match sel.len() {
        0 => set_text(st.detail, ""),
        1 => {
            let e = &st.entries[sel[0]];
            let dict = if st.dicts.len() > 1 {
                format!("辞書: {}\r\n", st.dicts.get(e.dict).map_or("?", |d| d.name.as_str()))
            } else {
                String::new()
            };
            let mut s = format!("{}\r\n{}", e.word, dict);
            if !e.tags.is_empty() {
                s.push_str(&format!("タグ: {}\r\n", e.tags));
            }
            s.push_str(&format!("更新: {}\r\n\r\n", core::format_jst(e.updated_at)));
            s.push_str(&e.meaning.replace("\r\n", "\n").replace('\n', "\r\n"));
            set_text(st.detail, &s);
        }
        n => set_text(st.detail, &format!("{n} 件を選択中\r\n(Delete キーまたは「選択を削除」でまとめて削除できます)")),
    }
}

// ---- 操作: 登録 / 更新 / 編集 / 削除 ------------------------------------------

unsafe fn message_box(hwnd: HWND, text: &str, caption: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    let (t, c) = (wide(text), wide(caption));
    MessageBoxW(hwnd, PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), style)
}

unsafe fn exit_edit(st: &mut State) {
    st.editing = None;
    set_text(st.btn_add, "登録");
}

unsafe fn clear_inputs(st: &mut State) {
    set_text(st.word, "");
    set_text(st.tags, "");
    set_text(st.meaning, "");
    exit_edit(st);
}

/// 書き込める辞書か確認し、だめなら理由を返す
fn writable(d: &Dict) -> Result<(), String> {
    if d.readonly {
        return Err(format!("辞書「{}」は参照のみです(設定ファイルで readonly = false にすると編集できます)", d.name));
    }
    if d.error.is_some() {
        return Err(format!("辞書「{}」を読み込めていないため書き込めません", d.name));
    }
    Ok(())
}

unsafe fn do_add(hwnd: HWND, st: &mut State) {
    if !st.loaded {
        notice(st, "データベースの読み込みが完了していません");
        return;
    }
    let word = get_text(st.word).trim().to_string();
    let meaning = get_text(st.meaning).replace("\r\n", "\n").trim().to_string();
    let tags = core::normalize_tags(&get_text(st.tags));
    if word.is_empty() {
        notice(st, "見出し語を入力してください");
        let _ = SetFocus(st.word);
        return;
    }
    if meaning.is_empty() {
        notice(st, "意味を入力してください");
        let _ = SetFocus(st.meaning);
        return;
    }

    if let Some((d, id)) = st.editing {
        // 更新
        if let Err(m) = writable(&st.dicts[d]) {
            notice(st, &m);
            return;
        }
        match core::open(&st.dicts[d].path).and_then(|c| core::update(&c, id, &word, &meaning, &tags)) {
            Ok(ts) => {
                if let Some(e) = st.entries.iter_mut().find(|e| e.dict == d && e.id == id) {
                    *e = Entry::new(id, word.clone(), meaning, tags, ts).with_dict(d);
                }
                core::sort_entries(&mut st.entries);
                clear_inputs(st);
                refresh(st);
                notice(st, &format!("「{word}」を更新しました"));
            }
            Err(e) => {
                message_box(hwnd, &format!("更新に失敗しました: {e}"), "エラー", MB_OK | MB_ICONERROR);
            }
        }
        return;
    }

    // 登録先: 辞書を選んでいればその辞書、「すべて」なら最初に書き込める辞書
    let sel = send(st.combo, CB_GETCURSEL, 0, 0).0;
    let target = if sel > 0 {
        Some(sel as usize - 1)
    } else {
        st.dicts.iter().position(|d| writable(d).is_ok())
    };
    let Some(d) = target else {
        notice(st, "書き込める辞書がありません(設定ファイルを確認してください)");
        return;
    };
    if let Err(m) = writable(&st.dicts[d]) {
        notice(st, &m);
        return;
    }
    match core::open(&st.dicts[d].path).and_then(|c| core::insert(&c, &word, &meaning, &tags)) {
        Ok(entry) => {
            st.entries.push(entry.with_dict(d));
            core::sort_entries(&mut st.entries);
            clear_inputs(st);
            refresh(st);
            let name = st.dicts[d].name.clone();
            let msg = if st.dicts.len() > 1 { format!("「{word}」を「{name}」に登録しました") } else { format!("「{word}」を登録しました") };
            notice(st, &msg);
            let _ = SetFocus(st.word);
        }
        Err(e) => {
            message_box(hwnd, &format!("登録に失敗しました: {e}"), "エラー", MB_OK | MB_ICONERROR);
        }
    }
}

unsafe fn do_edit(st: &mut State) {
    let sel = selected_entries(st);
    if sel.len() != 1 {
        notice(st, if sel.is_empty() { "編集する行を選択してください" } else { "編集できるのは1件ずつです(1件だけ選択してください)" });
        return;
    }
    let e = st.entries[sel[0]].clone();
    if let Err(m) = writable(&st.dicts[e.dict]) {
        notice(st, &m);
        return;
    }
    set_text(st.word, &e.word);
    set_text(st.tags, &e.tags);
    set_text(st.meaning, &e.meaning.replace("\r\n", "\n").replace('\n', "\r\n"));
    st.editing = Some((e.dict, e.id));
    set_text(st.btn_add, "更新");
    let _ = SetFocus(st.meaning);
    notice(st, &format!("「{}」を編集中(Ctrl+Enter または「更新」で保存、「クリア」で取りやめ)", e.word));
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
    if let Some((d, id)) = st.editing {
        if removed.contains(&(d, id)) {
            clear_inputs(st);
        }
    }
    refresh(st);
    if errors.is_empty() {
        notice(st, &format!("{} 件を削除しました", removed.len()));
    } else {
        message_box(hwnd, &format!("一部の削除に失敗しました:\n{}", errors.join("\n")), "エラー", MB_OK | MB_ICONERROR);
    }
}

// ---- 設定の再読み込み ---------------------------------------------------------

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
                layout(hwnd, st);
            }
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
            LRESULT(0)
        }
        WM_APP_LOADED => {
            let loaded = Box::from_raw(lparam.0 as *mut Loaded);
            if let Some(st) = state(hwnd) {
                if loaded.gen != st.gen {
                    return LRESULT(0); // 古い読み込み結果(設定の再読み込み前)は捨てる
                }
                let n = loaded.entries.len();
                tlog(&format!(
                    "読み込み完了: {} 件 (読み込み+小文字化 {:.1}ms / 並べ替え {:.1}ms)",
                    n, loaded.read_ms, loaded.sort_ms
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
            if BUSY.load(Ordering::SeqCst) > 0 {
                return LRESULT(0);
            }
            let Some(st) = state(hwnd) else { return LRESULT(0) };
            match (id, code) {
                (ID_EDIT | ID_QW | ID_QM | ID_QT, EN_CHANGE_) => {
                    if st.loaded {
                        refresh(st);
                    }
                }
                (ID_COMBO, CBN_SELCHANGE_) => {
                    if st.loaded {
                        refresh(st);
                    }
                }
                (ID_ADV, BN_CLICKED_) => {
                    st.advanced = send(st.adv, BM_GETCHECK_, 0, 0).0 == 1;
                    let cmd = if st.advanced { SW_SHOW } else { SW_HIDE };
                    for h in [st.qw, st.qm, st.qt] {
                        if !st.advanced {
                            set_text(h, "");
                        }
                        let _ = ShowWindow(h, cmd);
                    }
                    layout(hwnd, st);
                    if st.loaded {
                        refresh(st);
                    }
                    let _ = SetFocus(if st.advanced { st.qw } else { st.edit });
                }
                (ID_ADD, BN_CLICKED_) => do_add(hwnd, st),
                (ID_EDITBTN, BN_CLICKED_) => do_edit(st),
                (ID_DEL, BN_CLICKED_) => do_delete(hwnd, st),
                (ID_CLEAR, BN_CLICKED_) => {
                    clear_inputs(st);
                    notice(st, "入力欄をクリアしました");
                }
                _ => {
                    if id == IDCANCEL.0 as usize {
                        let _ = ShowWindow(hwnd, SW_HIDE); // Esc で隠す
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
                    } else if BUSY.load(Ordering::SeqCst) == 0 {
                        if hdr.code == LVN_KEYDOWN {
                            let kd = &*(lparam.0 as *const NMLVKEYDOWN);
                            if kd.wVKey == VK_DELETE.0 {
                                do_delete(hwnd, st);
                            }
                        } else if hdr.code == LVN_ITEMCHANGED {
                            let nm = &*(lparam.0 as *const NMLISTVIEW);
                            if (nm.uOldState ^ nm.uNewState) & LVIS_SELECTED.0 != 0 {
                                update_detail(st);
                            }
                        } else if hdr.code == NM_DBLCLK {
                            do_edit(st);
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
                        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN_CFG, w!("設定ファイルを開く"));
                        let _ = AppendMenuW(menu, MF_STRING, MENU_RELOAD, w!("設定を再読み込み"));
                        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                        let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("終了"));
                        let mut pt = POINT::default();
                        let _ = GetCursorPos(&mut pt);
                        let _ = SetForegroundWindow(hwnd);
                        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
                        let _ = DestroyMenu(menu);
                        match cmd.0 as usize {
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
                LRESULT(0)
            } else {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
        }
        WM_DESTROY => {
            let _ = UnregisterHotKey(hwnd, HOTKEY_ID);
            remove_tray_icon(hwnd);
            let p = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut State;
            if !p.is_null() {
                let st = Box::from_raw(p);
                let _ = DeleteObject(st.font);
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
