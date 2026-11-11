//! Win32 GUI (windows-rs)。電子辞書風の2ペイン: 左に候補リスト、右に本文。
//! - 起動直後から検索欄に入力でき、↑↓で候補を移動できる
//! - 候補リストは仮想リスト + カスタム描画(見出し語と意味の抜粋、検索語を赤で強調)
//! - 本文は RichEdit(見出し語を大きく、検索語を赤で強調、選択してコピー可)
//! - 登録・編集・設定はモーダルダイアログ。書き込みは safe モジュール経由(バックアップ + 検査 + ロールバック)
//! - DBの読み込みは別スレッド。他の人の変更はウィンドウを前面に出したときに検知して読み直す

use crate::config::{self, Config};
use crate::core::{self, Entry, Query};
use crate::dialogs::{self, EntryDlg, SelectDlg, SettingsDlg};
use crate::safe::{self, WriteError};
use crate::ui::{self, quiet, scale, Btn, Fonts, BTN_W, CTL_H};
use crate::ui::{get_text, send, set_text, wide};
use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::c_void;
use std::path::PathBuf;
use std::time::SystemTime;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, LoadLibraryW};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Controls::RichEdit::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS_NAME: PCWSTR = w!("FastDictMainWindow");
const ID_EDIT: usize = 100; // 全体検索
const ID_LIST: usize = 101;
const ID_RICH: usize = 102;
const ID_STATUS: usize = 103;
const ID_HINT: usize = 104;
const ID_DICTS: usize = 110; // 検索する辞書の選択
const ID_ADV: usize = 111; // 詳細検索の切替
const ID_QW: usize = 112; // カスタム検索: 見出し語
const ID_QM: usize = 113;
const ID_QT: usize = 114;
const ID_EDITBTN: usize = 115;
const ID_NEW: usize = 116;
const ID_SETTINGS: usize = 117;
const ID_DEL: usize = 118;
const ID_OPENLINK: usize = 119;
const HOTKEY_ID: i32 = 1;
const WM_APP_LOADED: u32 = WM_APP + 1;
const WM_APP_TRAY: u32 = WM_APP + 2;
const WM_APP_SHOW: u32 = WM_APP + 3;
const WM_APP_RELOAD: u32 = WM_APP + 4;
const MENU_OPEN: usize = 1;
const MENU_SETTINGS: usize = 2;
const MENU_OPEN_CFG: usize = 3;
const MENU_RELOAD: usize = 4;
const MENU_EXIT: usize = 5;

const EN_SETFOCUS_: usize = 0x0100;
const EN_KILLFOCUS_: usize = 0x0200;
const EN_CHANGE_: usize = 0x0300;
const BN_CLICKED_: usize = 0;
const LVM_SETCOLUMNWIDTH_: u32 = 0x101E;
const LVM_ENSUREVISIBLE_: u32 = 0x1013;
const LVM_SETIMAGELIST_: u32 = 0x1003;
const LVM_GETITEMRECT_: u32 = 0x100E;
const LVM_GETITEMSTATE_: u32 = 0x102C;

// ---- 状態 -----------------------------------------------------------------

type Stamp = Option<(SystemTime, u64)>;

struct Dict {
    name: String,
    path: PathBuf,
    readonly: bool,
    error: Option<String>,
    /// 読み込み時点のファイルの (更新時刻, サイズ)。他の人の変更検知に使う
    stamp: Stamp,
}

struct LoadedDict {
    error: Option<String>,
    corrupt: bool,
    stamp: Stamp,
}

struct Loaded {
    gen: u32,
    entries: Vec<Entry>,
    dicts: Vec<LoadedDict>,
}

struct InitData {
    cfg: Config,
    cfg_path: PathBuf,
    warns: Vec<String>,
}

struct State {
    // 上段
    edit: HWND,
    btn_dict: HWND,
    adv: HWND,
    qw: HWND,
    qm: HWND,
    qt: HWND,
    btn_new: HWND,
    btn_set: HWND,
    // 左: 候補リスト
    list: HWND,
    imglist: HIMAGELIST,
    // 右: 本文
    rich: HWND,
    rich_ok: bool,
    btn_edit: HWND,
    btn_del: HWND,
    status: HWND,
    hint: HWND,

    fonts: Fonts,
    dpi: i32,
    fields: Vec<(RECT, HWND)>,
    list_box: RECT,
    pane: RECT,
    status_top: i32,
    white: HBRUSH,

    cfg: Config,
    cfg_path: PathBuf,
    warns: Vec<String>,
    dicts: Vec<Dict>,
    /// 検索に含める辞書(dicts と同じ並び)
    dict_mask: Vec<bool>,
    /// 本文中のリンク (開始, 終了, 開く先)。位置は RichEdit 上の文字位置
    links: RefCell<Vec<(i32, i32, String)>>,
    entries: Vec<Entry>,
    matches: Vec<u32>,
    last_query: Option<Query>,
    /// 強調表示する語(見出し語用, 意味用, タグ用)
    marks: (Vec<String>, Vec<String>, Vec<String>),
    loaded: bool,
    loading: bool,
    gen: u32,
    advanced: bool,
    notice: Option<String>,
    cell: Vec<u16>,
    /// 読み込み完了後に選択し直すレコード (辞書, id)
    pending_select: Option<(usize, i64)>,
    /// 破損時の復元を一度たずねた辞書(同じ質問を繰り返さない)
    asked_restore: HashSet<PathBuf>,
}

fn app_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("fastdict")
}

/// バックアップ置き場(必ずローカル。ローミングされない LocalAppData を使う)
fn backup_root() -> PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(p) => PathBuf::from(p).join("fastdict").join("backup"),
        None => app_dir().join("backup"),
    }
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
        let background = std::env::args().any(|a| a == "--background");

        // 二重起動: 既存ウィンドウを呼び出して終了
        let _mutex = CreateMutexW(None, true, w!("Local\\FastDictSingleInstance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(existing) = FindWindowW(CLASS_NAME, PCWSTR::null()) {
                let _ = PostMessageW(existing, WM_APP_SHOW, WPARAM(0), LPARAM(0));
            }
            return;
        }

        let init = load_config();
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

        // ウィンドウを先に出す(すぐ入力できる)。読み込みは裏で。
        if !background {
            show_window(hwnd);
        }
        add_tray_icon(hwnd);
        if let Some(st) = state(hwnd) {
            if let Some(w) = register_hotkey(hwnd, st) {
                st.warns.push(w);
            }
            set_tray_tip(hwnd, st);
            start_load(hwnd, st, false);
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
                } else if ctrl && vk == 'O' as u16 {
                    Some(ID_OPENLINK)
                } else if vk == VK_F2.0 {
                    Some(ID_EDITBTN)
                } else {
                    None
                };
                if let Some(c) = cmd {
                    let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(c | BN_CLICKED_ << 16), LPARAM(0));
                    continue;
                }
                if let Some(st) = state(hwnd) {
                    if ctrl && vk == 'F' as u16 {
                        let _ = SetFocus(st.edit);
                        send(st.edit, EM_SETSEL, 0, -1);
                        continue;
                    }
                    // 検索欄にいるまま ↑↓ / PageUp / PageDown で候補を移動(電子辞書と同じ操作感)
                    let f = GetFocus();
                    let in_search = f == st.edit || f == st.qw || f == st.qm || f == st.qt;
                    if in_search && [VK_UP.0, VK_DOWN.0, VK_PRIOR.0, VK_NEXT.0].contains(&vk) {
                        SendMessageW(st.list, WM_KEYDOWN, msg.wParam, msg.lParam);
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

/// `--add-sample`: サンプル辞書(約300語)を作り、設定に「サンプル」として追加する。
/// 普段の辞書とは別ファイルなので、不要になったら設定で外すだけでよい。
pub fn add_sample() {
    unsafe {
        let init = load_config();
        let mut cfg = init.cfg;
        let base = init.cfg_path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
        let path = base.join("sample.db");
        let text = match core::create_sample(&path) {
            Ok(n) => {
                let mut msg = if n > 0 {
                    format!("サンプル辞書を作成しました({n} 語)。")
                } else {
                    "サンプル辞書はすでにあります(語は追加していません)。".to_string()
                };
                if !cfg.dicts.iter().any(|d| d.path == path) {
                    cfg.dicts.push(config::DictConf { name: "サンプル".into(), path: path.clone(), readonly: false });
                    match std::fs::write(&init.cfg_path, config::to_ini(&cfg, &base)) {
                        Ok(()) => msg.push_str("\n設定に辞書「サンプル」を追加しました。"),
                        Err(e) => msg.push_str(&format!("\n設定ファイルを更新できませんでした: {e}")),
                    }
                }
                // 起動中なら、すぐ反映させる
                if let Ok(existing) = FindWindowW(CLASS_NAME, PCWSTR::null()) {
                    let _ = PostMessageW(existing, WM_APP_RELOAD, WPARAM(0), LPARAM(0));
                }
                format!("{msg}\n\n場所: {}\n不要になったら、設定の「辞書」から外してください(ファイルは残ります)。", path.display())
            }
            Err(e) => format!("サンプル辞書を作成できませんでした: {e}"),
        };
        let (t, c) = (wide(&text), wide("fastdict サンプルデータ"));
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

/// 別スレッドで全辞書を読み込む。
/// `keep` が true なら、読み込みが終わるまで今の内容を表示したままにする(他の人の変更の取り込み用)。
unsafe fn start_load(hwnd: HWND, st: &mut State, keep: bool) {
    st.gen += 1;
    st.loading = true;
    if !keep {
        st.loaded = false;
        st.entries.clear();
        st.matches.clear();
        st.last_query = None;
        st.dicts = st
            .cfg
            .dicts
            .iter()
            .map(|d| Dict { name: d.name.clone(), path: d.path.clone(), readonly: d.readonly, error: None, stamp: None })
            .collect();
        st.dict_mask = vec![true; st.dicts.len()];
        update_dict_button(st);
        send(st.list, LVM_SETITEMCOUNT, 0, 0);
        update_detail(st);
        update_status(st);
    }

    let specs: Vec<(PathBuf, bool)> = st.dicts.iter().map(|d| (d.path.clone(), d.readonly)).collect();
    let (h, gen) = (hwnd.0 as usize, st.gen);
    std::thread::spawn(move || {
        let mut entries: Vec<Entry> = Vec::new();
        let mut dicts = Vec::new();
        for (i, (path, ro)) in specs.iter().enumerate() {
            let stamp = safe::file_stamp(path); // 読む前に取る(読んでいる間の変更を見逃さない)
            let r = if *ro { core::open_ro(path) } else { core::open(path) }.and_then(|c| core::load_all(&c));
            match r {
                Ok(mut v) => {
                    for e in &mut v {
                        e.dict = i;
                    }
                    entries.append(&mut v);
                    // 新規作成された場合は作成後の状態を記録する
                    let stamp = stamp.or_else(|| safe::file_stamp(path));
                    dicts.push(LoadedDict { error: None, corrupt: false, stamp });
                }
                Err(e) => dicts.push(LoadedDict { corrupt: safe::is_corrupt(&e), error: Some(e.to_string()), stamp }),
            }
        }
        core::sort_entries(&mut entries);
        let boxed = Box::into_raw(Box::new(Loaded { gen, entries, dicts }));
        let hwnd = HWND(h as *mut c_void);
        if PostMessageW(hwnd, WM_APP_LOADED, WPARAM(0), LPARAM(boxed as isize)).is_err() {
            drop(Box::from_raw(boxed));
        }
    });
}

/// 他の人(または別のPC)がファイルを変更していたら読み直す
unsafe fn reload_if_changed(hwnd: HWND, st: &mut State) {
    if !st.loaded || st.loading {
        return;
    }
    if st.dicts.iter().any(|d| safe::file_stamp(&d.path) != d.stamp) {
        remember_selection(st);
        start_load(hwnd, st, true);
    }
}

unsafe fn remember_selection(st: &mut State) {
    let sel = selected_entries(st);
    if sel.len() == 1 && st.pending_select.is_none() {
        let e = &st.entries[sel[0]];
        st.pending_select = Some((e.dict, e.id));
    }
}

/// 辞書ボタンの表示: すべて選択中なら「辞書: すべて」、絞っていれば「辞書: 2 / 3」
unsafe fn update_dict_button(st: &State) {
    let on = st.dict_mask.iter().filter(|&&b| b).count();
    let text = if on == st.dict_mask.len() { "辞書: すべて".to_string() } else { format!("辞書: {} / {}", on, st.dict_mask.len()) };
    set_text(st.btn_dict, &text);
    let _ = InvalidateRect(st.btn_dict, None, false);
}

unsafe fn do_select_dicts(hwnd: HWND, st: &mut State) {
    let mut dlg = SelectDlg {
        names: st.dicts.iter().map(|d| if d.readonly { format!("{}(参照のみ)", d.name) } else { d.name.clone() }).collect(),
        checked: st.dict_mask.clone(),
        ok: false,
    };
    if dialogs::select_dialog(hwnd, &mut dlg) {
        st.dict_mask = dlg.checked;
        update_dict_button(st);
        if st.loaded {
            refresh(st);
        }
    }
    let _ = SetFocus(st.edit);
}

// ---- コントロール作成・レイアウト --------------------------------------------

unsafe fn create_child(parent: HWND, class: PCWSTR, ex: u32, style: u32, id: usize) -> Option<HWND> {
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
    .ok()
}

unsafe fn owner_button(parent: HWND, id: usize, text: &str) -> HWND {
    let b = create_child(parent, w!("BUTTON"), 0, WS_TABSTOP.0 | 0x0B /* BS_OWNERDRAW */, id).unwrap();
    set_text(b, text);
    b
}

unsafe fn make_imglist(list: HWND, dpi: i32) -> HIMAGELIST {
    // 行の高さ(2行表示)を確保するため、高さだけ指定した空のイメージリストを割り当てる
    let il = ImageList_Create(1, scale(58, dpi), IMAGELIST_CREATION_FLAGS(0x20 /* ILC_COLOR32 */), 1, 1);
    send(list, LVM_SETIMAGELIST_, 1 /* LVSIL_SMALL */, il.0 as isize);
    il
}

unsafe fn on_create(hwnd: HWND, init: InitData) -> State {
    let dpi = GetDpiForWindow(hwnd).max(96) as i32;
    let fonts = Fonts::new(dpi);
    let mut fields: Vec<(RECT, HWND)> = Vec::new();

    let edit = ui::field(hwnd, ID_EDIT, (0, 0, 10, 10), dpi, false, &mut fields, fonts.base);
    let cue = wide("検索 — 見出し語・意味・タグ(スペースで AND、#タグ、* はワイルドカード)");
    send(edit, EM_SETCUEBANNER, 1, cue.as_ptr() as isize);
    let btn_dict = owner_button(hwnd, ID_DICTS, "辞書: すべて");
    let adv = owner_button(hwnd, ID_ADV, "絞り込み");
    let btn_new = owner_button(hwnd, ID_NEW, "＋ 新規登録");
    let btn_set = owner_button(hwnd, ID_SETTINGS, "設定");
    let mut adv_edit = |id: usize, cue: &str| {
        let h = ui::field(hwnd, id, (0, 0, 10, 10), dpi, false, &mut fields, fonts.base);
        let c = wide(cue);
        send(h, EM_SETCUEBANNER, 1, c.as_ptr() as isize);
        let _ = ShowWindow(h, SW_HIDE);
        h
    };
    let qw = adv_edit(ID_QW, "見出し語で絞り込み");
    let qm = adv_edit(ID_QM, "意味で絞り込み");
    let qt = adv_edit(ID_QT, "タグで絞り込み(複数はスペース区切りで AND)");

    // 候補リスト: 1列・見出し無し・行は自前で描画
    let list = create_child(
        hwnd,
        w!("SysListView32"),
        0,
        WS_TABSTOP.0 | LVS_REPORT as u32 | LVS_OWNERDATA as u32 | LVS_SHOWSELALWAYS as u32 | LVS_NOCOLUMNHEADER as u32,
        ID_LIST,
    )
    .unwrap();
    send(list, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize);
    let mut t = wide("見出し語");
    let col = LVCOLUMNW { mask: LVCF_TEXT | LVCF_WIDTH, cx: 100, pszText: PWSTR(t.as_mut_ptr()), ..Default::default() };
    send(list, LVM_INSERTCOLUMNW, 0, &col as *const _ as isize);
    let imglist = make_imglist(list, dpi);

    // 本文: RichEdit(書式付き)。使えない環境では通常の複数行エディットで代用
    let _ = LoadLibraryW(w!("Msftedit.dll"));
    let rich_style = WS_VSCROLL.0 | ES_MULTILINE as u32 | ES_READONLY as u32 | ES_AUTOVSCROLL as u32;
    let (rich, rich_ok) = match create_child(hwnd, w!("RICHEDIT50W"), 0, rich_style, ID_RICH) {
        Some(h) => {
            send(h, EM_SETBKGNDCOLOR, 0, ui::WHITE.0 as isize);
            // 本文中の URL を自動でリンクにし、クリックを EN_LINK で受け取る
            send(h, EM_AUTOURLDETECT, 1, 0);
            send(h, EM_SETEVENTMASK, 0, ENM_LINK as isize);
            (h, true)
        }
        None => (create_child(hwnd, w!("EDIT"), 0, rich_style, ID_RICH).unwrap(), false),
    };
    let btn_edit = owner_button(hwnd, ID_EDITBTN, "編集");
    let btn_del = owner_button(hwnd, ID_DEL, "削除");
    let status = create_child(hwnd, w!("STATIC"), 0, 0x0000000C | 0x80 /* SS_LEFTNOWORDWRAP | SS_NOPREFIX */, ID_STATUS).unwrap();
    let hint = create_child(hwnd, w!("STATIC"), 0, 0x2 | 0x80 /* SS_RIGHT | SS_NOPREFIX */, ID_HINT).unwrap();
    set_text(hint, "↑↓ 候補    Ctrl+N 新規    F2 編集    Del 削除    Ctrl+O 参照先を開く    Esc 消去/閉じる");

    let st = State {
        edit,
        btn_dict,
        adv,
        qw,
        qm,
        qt,
        btn_new,
        btn_set,
        list,
        imglist,
        rich,
        rich_ok,
        btn_edit,
        btn_del,
        status,
        hint,
        fonts,
        dpi,
        fields,
        list_box: RECT::default(),
        pane: RECT::default(),
        status_top: 0,
        white: CreateSolidBrush(ui::WHITE),
        warns: init.warns,
        cfg: init.cfg,
        cfg_path: init.cfg_path,
        dicts: Vec::new(),
        dict_mask: Vec::new(),
        links: RefCell::new(Vec::new()),
        entries: Vec::new(),
        matches: Vec::new(),
        last_query: None,
        marks: (Vec::new(), Vec::new(), Vec::new()),
        loaded: false,
        loading: false,
        gen: 0,
        advanced: false,
        notice: None,
        cell: Vec::new(),
        pending_select: None,
        asked_restore: HashSet::new(),
    };
    apply_fonts(&st);
    let _ = SetFocus(edit);
    st
}

unsafe fn apply_fonts(st: &State) {
    let f = &st.fonts;
    for (h, font) in [
        (st.edit, f.base),
        (st.qw, f.base),
        (st.qm, f.base),
        (st.qt, f.base),
        (st.list, f.base),
        (st.rich, f.base),
        (st.status, f.small),
        (st.hint, f.small),
    ] {
        send(h, WM_SETFONT, font.0 as usize, 1);
    }
}

unsafe fn layout(hwnd: HWND, st: &mut State) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let dpi = st.dpi;
    let s = |v: i32| scale(v, dpi);
    let (m, gap, box_h) = (s(16), s(8), s(CTL_H));
    let w = rc.right - 2 * m;

    // --- 上段: 検索欄 | 辞書 | 絞り込み | ＋新規登録 | 設定(ボタンはすべて同じ大きさ) ---
    let (combo_w, adv_w, new_w, set_w) = (s(BTN_W), s(BTN_W), s(BTN_W), s(BTN_W));
    let search_w = (w - combo_w - adv_w - new_w - set_w - 4 * gap).max(s(120));
    let y = s(14);
    ui::place_field(st.edit, (m, y, search_w, box_h), dpi, false, &mut st.fields);
    let mut x = m + search_w + gap;
    let _ = MoveWindow(st.btn_dict, x, y, combo_w, box_h, true);
    x += combo_w + gap;
    let _ = MoveWindow(st.adv, x, y, adv_w, box_h, true);
    x += adv_w + gap;
    let _ = MoveWindow(st.btn_new, x, y, new_w, box_h, true);
    x += new_w + gap;
    let _ = MoveWindow(st.btn_set, x, y, set_w, box_h, true);
    let mut top = y + box_h + s(12);

    // --- 絞り込み(列ごと) ---
    let adv_cmd = if st.advanced { SW_SHOW } else { SW_HIDE };
    for h in [st.qw, st.qm, st.qt] {
        let _ = ShowWindow(h, adv_cmd);
    }
    if st.advanced {
        let fh = s(CTL_H);
        let cw = (w - 2 * gap) / 3;
        ui::place_field(st.qw, (m, top, cw, fh), dpi, false, &mut st.fields);
        ui::place_field(st.qm, (m + cw + gap, top, cw, fh), dpi, false, &mut st.fields);
        ui::place_field(st.qt, (m + 2 * (cw + gap), top, w - 2 * (cw + gap), fh), dpi, false, &mut st.fields);
        top += fh + s(12);
    }

    // --- 下端: ステータス(左)とキー操作の案内(右) ---
    let status_h = s(30);
    st.status_top = rc.bottom - status_h;
    let hint_w = (w * 62 / 100).min(s(600));
    let _ = MoveWindow(st.status, m, st.status_top + s(6), w - hint_w - gap, s(20), true);
    let _ = MoveWindow(st.hint, m + w - hint_w, st.status_top + s(6), hint_w, s(20), true);
    let bottom = st.status_top - s(12);
    let h = (bottom - top).max(s(80));

    // --- 左: 候補リスト(枠の内側に置く) ---
    let list_w = (w * 36 / 100).clamp(s(280), s(440));
    st.list_box = RECT { left: m, top, right: m + list_w, bottom: top + h };
    let inset = s(2);
    let _ = MoveWindow(st.list, m + inset, top + inset, list_w - 2 * inset, h - 2 * inset, true);
    send(st.list, LVM_SETCOLUMNWIDTH_, 0, -2); // LVSCW_AUTOSIZE_USEHEADER: 幅いっぱい

    // --- 右: 本文 ---
    let px = m + list_w + s(12);
    st.pane = RECT { left: px, top, right: rc.right - m, bottom: top + h };
    let pad = s(26);
    let (ix, iw) = (px + pad, (st.pane.right - st.pane.left) - 2 * pad);
    let (btn_h, bw, bgap) = (s(CTL_H), s(BTN_W), s(10));
    let btn_y = top + h - s(16) - btn_h;
    let _ = MoveWindow(st.rich, ix, top + s(20), iw + pad - s(6), (btn_y - s(12) - (top + s(20))).max(s(30)), true);
    let _ = MoveWindow(st.btn_del, ix + iw - bw, btn_y, bw, btn_h, true);
    let _ = MoveWindow(st.btn_edit, ix + iw - 2 * bw - bgap, btn_y, bw, btn_h, true);
    // 絞り込み欄の出し入れで残像が出ないよう、背景ごと描き直す
    let _ = RedrawWindow(hwnd, None, None, RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN);
}

unsafe fn paint(hwnd: HWND, st: &State) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    ui::fill(hdc, &rc, ui::WHITE);
    ui::fill(hdc, &RECT { left: 0, top: st.status_top, right: rc.right, bottom: st.status_top + 1 }, ui::BORDER);
    let r = scale(12, st.dpi);
    ui::round_box(hdc, &st.list_box, ui::WHITE, ui::BORDER, r);
    ui::round_box(hdc, &st.pane, ui::WHITE, ui::BORDER, r);
    ui::paint_fields(hdc, &st.fields, st.dpi);
    let _ = EndPaint(hwnd, &ps);
}

// ---- 候補リストの行描画 -------------------------------------------------------

unsafe fn draw_row(st: &State, hdc: HDC, row: usize) {
    let mut rc = RECT::default(); // left = LVIR_BOUNDS(0)
    send(st.list, LVM_GETITEMRECT_, row, &mut rc as *mut _ as isize);
    let Some(&idx) = st.matches.get(row) else { return };
    let e = &st.entries[idx as usize];
    let s = |v: i32| scale(v, st.dpi);
    let selected = send(st.list, LVM_GETITEMSTATE_, row, LVIS_SELECTED.0 as isize).0 as u32 & LVIS_SELECTED.0 != 0;

    ui::fill(hdc, &rc, if selected { ui::ACCENT_SOFT } else { ui::WHITE });
    if selected {
        ui::fill(hdc, &RECT { left: rc.left, top: rc.top + s(6), right: rc.left + s(4), bottom: rc.bottom - s(6) }, ui::ACCENT);
    }
    ui::fill(hdc, &RECT { left: rc.left + s(14), top: rc.bottom - 1, right: rc.right, bottom: rc.bottom }, ui::HAIRLINE);

    let (x0, x1) = (rc.left + s(16), rc.right - s(12));
    let (y1, y2) = (rc.top + s(9), rc.top + s(33));
    let old = SelectObject(hdc, st.fonts.small);

    // 1行目の右: タグ / 2行目の右: 辞書名(複数辞書のとき)
    let tags = core::hash_tags(&e.tags);
    let tags_w = if tags.is_empty() { 0 } else { ui::text_width(hdc, &tags).min((x1 - x0) * 45 / 100) };
    if tags_w > 0 {
        let r = RECT { left: x1 - tags_w, top: y1 + s(3), right: x1, bottom: y2 };
        ui::draw_marked(hdc, &tags, &core::mark_ranges(&tags, &st.marks.2), &r, ui::MUTED, ui::MARK);
    }
    let dict = if st.dicts.len() > 1 { st.dicts.get(e.dict).map_or("", |d| d.name.as_str()) } else { "" };
    let dict_w = if dict.is_empty() { 0 } else { ui::text_width(hdc, dict).min((x1 - x0) * 40 / 100) };
    if dict_w > 0 {
        let r = RECT { left: x1 - dict_w, top: y2, right: x1, bottom: rc.bottom };
        ui::draw_marked(hdc, dict, &[], &r, ui::MUTED, ui::MARK);
    }

    // 2行目: 意味の抜粋
    let snippet: String = e.meaning.chars().take(200).map(|c| if c == '\r' || c == '\n' { ' ' } else { c }).collect();
    let r2 = RECT { left: x0, top: y2, right: x1 - dict_w - if dict_w > 0 { s(10) } else { 0 }, bottom: rc.bottom };
    ui::draw_marked(hdc, &snippet, &core::mark_ranges(&snippet, &st.marks.1), &r2, ui::MUTED, ui::MARK);

    // 1行目: 見出し語
    SelectObject(hdc, st.fonts.heading);
    let r1 = RECT { left: x0, top: y1, right: x1 - tags_w - if tags_w > 0 { s(10) } else { 0 }, bottom: y2 };
    ui::draw_marked(hdc, &e.word, &core::mark_ranges(&e.word, &st.marks.0), &r1, ui::TEXT, ui::MARK);
    SelectObject(hdc, old);
}

// ---- 表示更新 -------------------------------------------------------------

unsafe fn build_query(st: &State) -> Query {
    Query {
        all: get_text(st.edit),
        word: if st.advanced { get_text(st.qw) } else { String::new() },
        meaning: if st.advanced { get_text(st.qm) } else { String::new() },
        tag: if st.advanced { get_text(st.qt) } else { String::new() },
        dicts: if st.dict_mask.iter().all(|&b| b) { None } else { Some(st.dict_mask.clone()) },
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

/// 指定のレコードを選択して見える位置へ(見つからなければ何もしない)
unsafe fn select_entry(st: &mut State, dict: usize, id: i64) {
    let row = st.matches.iter().position(|&i| {
        let e = &st.entries[i as usize];
        e.dict == dict && e.id == id
    });
    if row.is_some() {
        select_row(st, row);
        update_detail(st);
    }
}

unsafe fn refresh(st: &mut State) {
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
    st.marks = q.mark_terms();
    st.last_query = Some(q);

    quiet(|| {
        send(st.list, LVM_SETITEMCOUNT, st.matches.len(), 0);
        send(st.list, LVM_SETCOLUMNWIDTH_, 0, -2); // スクロールバーの有無に合わせて幅を取り直す
    });
    select_row(st, if st.matches.is_empty() { None } else { Some(0) });
    let _ = InvalidateRect(st.list, None, true);
    update_detail(st);
    update_status(st);
}

unsafe fn update_status(st: &State) {
    let errs: Vec<String> = st
        .dicts
        .iter()
        .filter_map(|d| d.error.as_ref().map(|e| format!("辞書「{}」を読み込めません: {}", d.name, e)))
        .collect();
    let text = if !st.loaded {
        "読み込み中…".to_string()
    } else if st.matches.len() == st.entries.len() {
        format!("全 {} 件", st.entries.len())
    } else {
        format!("{} 件 / 全 {} 件", st.matches.len(), st.entries.len())
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

fn len16(s: &str) -> usize {
    s.encode_utf16().count()
}

unsafe fn rich_format(rich: HWND, range: (usize, usize), pt: Option<i32>, bold: bool, color: COLORREF) {
    if range.1 <= range.0 {
        return;
    }
    let cr = CHARRANGE { cpMin: range.0 as i32, cpMax: range.1 as i32 };
    send(rich, EM_EXSETSEL, 0, &cr as *const _ as isize);
    let mut cf = CHARFORMAT2W::default();
    cf.Base.cbSize = std::mem::size_of::<CHARFORMAT2W>() as u32;
    cf.Base.dwMask = CFM_COLOR | CFM_BOLD;
    cf.Base.dwEffects = if bold { CFE_BOLD } else { CFE_EFFECTS(0) };
    cf.Base.crTextColor = color;
    if let Some(pt) = pt {
        cf.Base.dwMask |= CFM_SIZE | CFM_FACE;
        cf.Base.yHeight = pt * 20;
        let mut face = [0u16; 32];
        for (i, c) in "Yu Gothic UI".encode_utf16().enumerate() {
            face[i] = c;
        }
        cf.Base.szFaceName = face;
    }
    send(rich, EM_SETCHARFORMAT, SCF_SELECTION as usize, &cf as *const _ as isize);
}

/// 範囲をリンク(クリックで EN_LINK が届く)にする
unsafe fn rich_link(rich: HWND, range: (usize, usize)) {
    let cr = CHARRANGE { cpMin: range.0 as i32, cpMax: range.1 as i32 };
    send(rich, EM_EXSETSEL, 0, &cr as *const _ as isize);
    let mut cf = CHARFORMAT2W::default();
    cf.Base.cbSize = std::mem::size_of::<CHARFORMAT2W>() as u32;
    cf.Base.dwMask = CFM_LINK;
    cf.Base.dwEffects = CFE_LINK;
    send(rich, EM_SETCHARFORMAT, SCF_SELECTION as usize, &cf as *const _ as isize);
}

/// 本文を描く: 見出し語(大)/ 補足(小・灰)/ 意味 / 参照先(末尾にまとめる)。
/// `*_marks` は各部分の中での強調範囲(UTF-16)。
unsafe fn render(st: &State, title: &str, meta: &str, body: &str, links: &[(String, String)], title_marks: &[(usize, usize)], meta_marks: &[(usize, usize)], body_marks: &[(usize, usize)]) {
    let body = body.replace("\r\n", "\n");
    // 文字位置は UTF-16 単位で数える。RichEdit 内部では改行は1文字なので、'\n' = 1 のままでよい
    let mut text = String::new();
    let push = |text: &mut String, s: &str| -> (usize, usize) {
        let a = len16(text);
        text.push_str(s);
        (a, a + len16(s))
    };
    let r_title = push(&mut text, title);
    let mut r_meta = (0, 0);
    if !meta.is_empty() {
        push(&mut text, "\n");
        r_meta = push(&mut text, meta);
    }
    let (mut r_gap, mut r_body) = ((0, 0), (0, 0));
    if !body.is_empty() {
        r_gap = push(&mut text, "\n\n");
        r_body = push(&mut text, &body);
    }
    // 参照先: 本文の最後に見出しを付けて並べる
    let mut r_gap2 = (0, 0);
    let mut r_head = (0, 0);
    let mut r_links: Vec<((usize, usize), (usize, usize))> = Vec::new(); // (行頭の記号, 表示名)
    if !links.is_empty() {
        r_gap2 = push(&mut text, "\n\n");
        r_head = push(&mut text, "参照先");
        for (name, _) in links {
            push(&mut text, "\n");
            let bullet = push(&mut text, "・");
            r_links.push((bullet, push(&mut text, name)));
        }
    }
    let mut store = st.links.borrow_mut();
    store.clear();
    if !st.rich_ok {
        // 書式なしの代替表示: 表示名だけでは開く先が分からないので、開く先も併記する
        let mut plain = text.clone();
        for (name, target) in links {
            if name != target {
                plain.push_str(&format!("\n   {target}"));
            }
        }
        set_text(st.rich, &plain.replace('\n', "\r\n"));
        return;
    }
    send(st.rich, WM_SETREDRAW, 0, 0);
    set_text(st.rich, &text.replace('\n', "\r\n"));
    rich_format(st.rich, r_title, Some(20), true, ui::TEXT);
    rich_format(st.rich, r_meta, Some(9), false, ui::MUTED);
    rich_format(st.rich, r_gap, Some(6), false, ui::MUTED); // 見出し部分と意味の間の空行は小さく
    rich_format(st.rich, r_body, Some(12), false, ui::TEXT);
    rich_format(st.rich, r_gap2, Some(8), false, ui::MUTED);
    rich_format(st.rich, r_head, Some(9), true, ui::MUTED);
    for (i, (bullet, name)) in r_links.iter().enumerate() {
        rich_format(st.rich, *bullet, Some(10), false, ui::MUTED);
        rich_format(st.rich, *name, Some(10), false, ui::ACCENT);
        rich_link(st.rich, *name);
        store.push((name.0 as i32, name.1 as i32, links[i].1.clone()));
    }
    for &(a, b) in title_marks {
        rich_format(st.rich, (r_title.0 + a, r_title.0 + b), None, true, ui::MARK);
    }
    for &(a, b) in meta_marks {
        rich_format(st.rich, (r_meta.0 + a, r_meta.0 + b), None, false, ui::MARK);
    }
    for &(a, b) in body_marks {
        rich_format(st.rich, (r_body.0 + a, r_body.0 + b), None, true, ui::MARK);
    }
    let cr = CHARRANGE { cpMin: 0, cpMax: 0 };
    send(st.rich, EM_EXSETSEL, 0, &cr as *const _ as isize);
    send(st.rich, WM_VSCROLL, 6 /* SB_TOP */, 0);
    send(st.rich, WM_SETREDRAW, 1, 0);
    let _ = InvalidateRect(st.rich, None, true);
}

/// 本文欄: 0件(案内)/ 1件(内容)/ 複数(件数)
unsafe fn update_detail(st: &State) {
    let sel = selected_entries(st);
    match sel.len() {
        0 => {
            let (t, m) = if !st.loaded {
                ("読み込み中…", "")
            } else if st.entries.is_empty() {
                ("まだ単語がありません", "右上の「＋ 新規登録」(Ctrl+N)から最初の単語を追加できます。")
            } else if st.matches.is_empty() {
                ("見つかりません", "検索語を変えるか、「＋ 新規登録」(Ctrl+N)でこの語を追加できます。")
            } else {
                ("", "")
            };
            render(st, t, m, "", &[], &[], &[], &[]);
        }
        1 => {
            let e = &st.entries[sel[0]];
            let tags = core::hash_tags(&e.tags);
            let mut parts: Vec<String> = Vec::new();
            if !tags.is_empty() {
                parts.push(tags.clone());
            }
            if st.dicts.len() > 1 {
                parts.push(st.dicts.get(e.dict).map_or("?".into(), |d| d.name.clone()));
            }
            parts.push(format!("更新 {}", core::format_jst(e.updated_at)));
            let meta = parts.join("    ·    ");
            let body = e.meaning.replace("\r\n", "\n");
            // タグは meta の先頭にあるので、強調位置はそのまま使える
            let meta_marks = if tags.is_empty() { Vec::new() } else { core::mark_ranges(&tags, &st.marks.2) };
            render(st, &e.word, &meta, &body, &core::parse_links(&e.link), &core::mark_ranges(&e.word, &st.marks.0), &meta_marks, &core::mark_ranges(&body, &st.marks.1));
        }
        n => render(st, &format!("{n} 件を選択中"), "Delete キーまたは「削除」でまとめて削除できます。", "", &[], &[], &[], &[]),
    }
    let can_edit = sel.len() == 1 && writable(&st.dicts[st.entries[sel[0]].dict]).is_ok();
    let can_del = !sel.is_empty() && sel.iter().all(|&i| writable(&st.dicts[st.entries[i].dict]).is_ok());
    let _ = EnableWindow(st.btn_edit, can_edit);
    let _ = EnableWindow(st.btn_del, can_del);
    set_text(st.btn_del, &if sel.len() > 1 { format!("{} 件を削除", sel.len()) } else { "削除".to_string() });
    let _ = InvalidateRect(st.btn_edit, None, false);
    let _ = InvalidateRect(st.btn_del, None, false);
}

// ---- 参照先を開く -----------------------------------------------------------

/// 参照先を開く。Web ページはそのまま、ファイル等は確認してから開く。
/// 共有の辞書には他の人が登録したリンクも入るため、実行ファイルには強い警告を出す。
unsafe fn open_link(hwnd: HWND, st: &mut State, link: &str) {
    let link = link.trim().trim_matches('"').to_string();
    if link.is_empty() {
        return;
    }
    match core::link_kind(&link) {
        core::LinkKind::Web => {}
        core::LinkKind::Other => {
            let text = format!("次の参照先を開きますか?\n\n{link}");
            if message_box(hwnd, &text, "参照先を開く", MB_YESNO | MB_ICONQUESTION) != IDYES {
                return;
            }
        }
        core::LinkKind::Executable => {
            let text = format!(
                "この参照先は、プログラムとして実行される可能性のあるファイルです。\n\n{link}\n\n登録した人と内容を信頼できる場合だけ開いてください。開きますか?"
            );
            if message_box(hwnd, &text, "参照先を開く(注意)", MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) != IDYES {
                return;
            }
        }
    }
    let w = wide(&link);
    let r = ShellExecuteW(hwnd, w!("open"), PCWSTR(w.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    if r.0 as isize <= 32 {
        notice(st, &format!("参照先を開けませんでした: {link}"));
    }
}

/// Ctrl+O: 選択中の語の参照先(複数あれば先頭)を開く
unsafe fn do_open_link(hwnd: HWND, st: &mut State) {
    let sel = selected_entries(st);
    let first = if sel.len() == 1 { core::parse_links(&st.entries[sel[0]].link).into_iter().next() } else { None };
    match first {
        Some((_, target)) => open_link(hwnd, st, &target),
        None => notice(st, "この語には参照先が登録されていません(「編集」から追加できます)"),
    }
}

// ---- 操作: 新規登録 / 編集 / 削除 / 設定 -----------------------------------------

unsafe fn message_box(hwnd: HWND, text: &str, caption: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    let (t, c) = (wide(text), wide(caption));
    MessageBoxW(hwnd, PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), style)
}

/// 安全な手順(バックアップ → 書き込み → 検査 → 必要ならロールバック)で書き込む。
/// 戻り値の bool は「書き込み前に他の人の変更があった(= メモリの内容が古い)」かどうか。
unsafe fn guarded<T>(st: &State, d: usize, f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>) -> (Result<T, WriteError>, bool) {
    let path = &st.dicts[d].path;
    let stale = safe::file_stamp(path) != st.dicts[d].stamp;
    let old = SetCursor(LoadCursorW(None, IDC_WAIT).unwrap());
    let r = safe::write(path, &backup_root(), f);
    SetCursor(old);
    (r, stale)
}

/// 壊れた辞書を、ローカルのバックアップから復元するか利用者にたずねる
unsafe fn offer_restore(hwnd: HWND, st: &mut State, d: usize) {
    let path = st.dicts[d].path.clone();
    let name = st.dicts[d].name.clone();
    match safe::latest_backup(&path, &backup_root()) {
        Some((bk, ts)) => {
            let text = format!(
                "辞書「{name}」のファイルが壊れています。\n\nこのPCに保存してあるバックアップ({} 時点)から復元しますか?\n\n・復元すると、その時点より後の変更(他の人の分を含む)は失われます。\n・ほかの人が使用中の場合は、いったん閉じてもらってから実行してください。",
                core::format_jst(ts)
            );
            if message_box(hwnd, &text, "辞書の復元", MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) == IDYES {
                match safe::restore(&path, &bk) {
                    Ok(()) => {
                        start_load(hwnd, st, false);
                        notice(st, &format!("辞書「{name}」をバックアップから復元しました"));
                    }
                    Err(e) => {
                        message_box(hwnd, &format!("復元に失敗しました: {e}"), "エラー", MB_OK | MB_ICONERROR);
                    }
                }
            }
        }
        None => {
            message_box(
                hwnd,
                &format!("辞書「{name}」のファイルが壊れています。\nこのPCには復元に使えるバックアップがありません。\n(バックアップは、このPCから書き込んだときに作られます)"),
                "辞書の破損",
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

unsafe fn report_write_error(hwnd: HWND, st: &mut State, d: usize, e: WriteError) {
    match e {
        WriteError::CorruptBefore => offer_restore(hwnd, st, d),
        WriteError::Conflict | WriteError::RolledBack => {
            message_box(hwnd, &e.to_string(), "保存できませんでした", MB_OK | MB_ICONWARNING);
            remember_selection(st);
            start_load(hwnd, st, true);
        }
        other => {
            message_box(hwnd, &other.to_string(), "保存できませんでした", MB_OK | MB_ICONERROR);
        }
    }
}

/// 書き込み成功後: メモリの内容が古ければ読み直し、そうでなければ変更検知用の印だけ更新する
unsafe fn after_write(hwnd: HWND, st: &mut State, d: usize, stale: bool, select: Option<(usize, i64)>) {
    if stale {
        st.pending_select = select;
        start_load(hwnd, st, true);
    } else {
        st.dicts[d].stamp = safe::file_stamp(&st.dicts[d].path);
        refresh(st);
        if let Some((dd, id)) = select {
            select_entry(st, dd, id);
        }
    }
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
    // 既定の登録先: 検索対象に選んでいる辞書のうち、最初に書き込めるもの
    let choice = targets.iter().position(|&t| st.dict_mask.get(t).copied().unwrap_or(true)).unwrap_or(0);
    // 検索して見つからなかった語を、見出し語の初期値にする
    let q = get_text(st.edit).trim().to_string();
    let word = if st.matches.is_empty() && !q.contains(char::is_whitespace) { q } else { String::new() };
    let mut dlg = EntryDlg {
        title: "新規登録".into(),
        word,
        tags: String::new(),
        meaning: String::new(),
        link: String::new(),
        choices: targets.iter().map(|&i| st.dicts[i].name.clone()).collect(),
        choice,
        show_choice: targets.len() > 1,
        ok: false,
    };
    if !dialogs::entry_dialog(hwnd, &mut dlg) {
        return;
    }
    let d = targets[dlg.choice];
    let (r, stale) = guarded(st, d, |c| core::insert(c, &dlg.word, &dlg.meaning, &dlg.tags, &dlg.link));
    match r {
        Ok(entry) => {
            let id = entry.id;
            st.entries.push(entry.with_dict(d));
            core::sort_entries(&mut st.entries);
            after_write(hwnd, st, d, stale, Some((d, id)));
            let msg = if st.dicts.len() > 1 {
                format!("「{}」を「{}」に登録しました", dlg.word, st.dicts[d].name)
            } else {
                format!("「{}」を登録しました", dlg.word)
            };
            notice(st, &msg);
        }
        Err(e) => report_write_error(hwnd, st, d, e),
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
        link: e.link.clone(),
        choices: Vec::new(),
        choice: 0,
        show_choice: false,
        ok: false,
    };
    if !dialogs::entry_dialog(hwnd, &mut dlg) {
        return;
    }
    // 読み込んだ時点の更新時刻と一致するときだけ更新する(他の人の変更を上書きしない)
    let (r, stale) = guarded(st, e.dict, |c| core::update(c, e.id, e.updated_at, &dlg.word, &dlg.meaning, &dlg.tags, &dlg.link));
    match r {
        Ok(ts) => {
            if let Some(x) = st.entries.iter_mut().find(|x| x.dict == e.dict && x.id == e.id) {
                *x = Entry::new(e.id, dlg.word.clone(), dlg.meaning.clone(), dlg.tags.clone(), ts).with_dict(e.dict).with_link(dlg.link.clone());
            }
            core::sort_entries(&mut st.entries);
            after_write(hwnd, st, e.dict, stale, Some((e.dict, e.id)));
            notice(st, &format!("「{}」を更新しました", dlg.word));
        }
        Err(err) => report_write_error(hwnd, st, e.dict, err),
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
    let mut any_stale = false;
    let mut failed: Option<(usize, WriteError)> = None;
    for (d, ids) in by_dict.iter().enumerate() {
        if ids.is_empty() {
            continue;
        }
        let (r, stale) = guarded(st, d, |c| core::delete_ids(c, ids));
        match r {
            Ok(_) => {
                removed.extend(ids.iter().map(|&id| (d, id)));
                any_stale |= stale;
                st.dicts[d].stamp = safe::file_stamp(&st.dicts[d].path);
            }
            Err(e) => {
                failed = Some((d, e));
                break;
            }
        }
    }
    st.entries.retain(|e| !removed.contains(&(e.dict, e.id)));
    if any_stale {
        start_load(hwnd, st, true);
    } else {
        refresh(st);
    }
    match failed {
        None => notice(st, &format!("{} 件を削除しました", removed.len())),
        Some((d, e)) => report_write_error(hwnd, st, d, e),
    }
}

unsafe fn do_settings(hwnd: HWND, st: &mut State) {
    let mut dlg = SettingsDlg { cfg: st.cfg.clone(), backup_root: backup_root(), imported: false, ok: false };
    if !dialogs::settings_dialog(hwnd, &mut dlg) {
        // キャンセルでも、インポートを実行していたら辞書を読み直す
        if dlg.imported {
            start_load(hwnd, st, true);
        }
        return;
    }
    let mut problems: Vec<String> = Vec::new();
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
        start_load(hwnd, st, false);
    } else if dlg.imported {
        start_load(hwnd, st, true);
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
    start_load(hwnd, st, false);
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
                update_detail(st);
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
            mmi.ptMinTrackSize.x = scale(900, dpi);
            mmi.ptMinTrackSize.y = scale(520, dpi);
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(st) = state(hwnd) {
                layout(hwnd, st);
            }
            LRESULT(0)
        }
        WM_ACTIVATE => {
            // 前面に出たとき、他の人がファイルを変更していたら読み直す
            if wparam.0 & 0xFFFF != 0 {
                if let Some(st) = state(hwnd) {
                    reload_if_changed(hwnd, st);
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_DPICHANGED => {
            let rc = &*(lparam.0 as *const RECT);
            if let Some(st) = state(hwnd) {
                st.dpi = (wparam.0 & 0xFFFF) as i32;
                st.fonts.destroy();
                st.fonts = Fonts::new(st.dpi);
                apply_fonts(st);
                send(st.list, LVM_SETIMAGELIST_, 1, 0);
                let _ = ImageList_Destroy(st.imglist);
                st.imglist = make_imglist(st.list, st.dpi);
            }
            let _ = SetWindowPos(hwnd, None, rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top, SWP_NOZORDER | SWP_NOACTIVATE);
            if let Some(st) = state(hwnd) {
                layout(hwnd, st);
                update_detail(st);
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
            let hdc = HDC(wparam.0 as *mut c_void);
            let id = GetDlgCtrlID(HWND(lparam.0 as *mut c_void)) as usize;
            match state(hwnd) {
                Some(st) => match id {
                    ID_STATUS | ID_HINT => ui::ctl_color(hdc, ui::MUTED, ui::WHITE, st.white),
                    _ => ui::ctl_color(hdc, ui::TEXT, ui::WHITE, st.white),
                },
                None => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
        WM_DRAWITEM => {
            let dis = &*(lparam.0 as *const DRAWITEMSTRUCT);
            if let Some(st) = state(hwnd) {
                let kind = match dis.CtlID as usize {
                    ID_NEW => Btn::Primary,
                    ID_ADV => Btn::Toggle(st.advanced),
                    ID_DICTS => Btn::Toggle(!st.dict_mask.iter().all(|&b| b)),
                    ID_DEL => Btn::Danger,
                    _ => Btn::Secondary,
                };
                ui::draw_button(dis, kind, st.fonts.base, ui::WHITE, st.dpi);
            }
            LRESULT(1)
        }
        WM_APP_LOADED => {
            let loaded = Box::from_raw(lparam.0 as *mut Loaded);
            if let Some(st) = state(hwnd) {
                if loaded.gen != st.gen {
                    return LRESULT(0); // 古い読み込み結果は捨てる
                }
                let loaded = *loaded;
                let mut corrupt: Vec<usize> = Vec::new();
                for (i, ld) in loaded.dicts.into_iter().enumerate() {
                    if let Some(d) = st.dicts.get_mut(i) {
                        d.error = ld.error;
                        d.stamp = ld.stamp;
                        if ld.corrupt && !d.readonly {
                            corrupt.push(i);
                        }
                    }
                }
                st.entries = loaded.entries;
                st.loaded = true;
                st.loading = false;
                st.last_query = None;
                refresh(st); // 読み込み中に入力された語句をここで反映
                if let Some((d, id)) = st.pending_select.take() {
                    select_entry(st, d, id);
                }
                if !st.warns.is_empty() {
                    let w = st.warns.join(" / ");
                    st.warns.clear();
                    notice(st, &w);
                }
                // 壊れている辞書は、ローカルのバックアップから復元するかたずねる(辞書ごとに1回だけ)
                for i in corrupt {
                    if st.asked_restore.insert(st.dicts[i].path.clone()) {
                        offer_restore(hwnd, st, i);
                        break;
                    }
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
                (ID_EDIT | ID_QW | ID_QM | ID_QT, EN_CHANGE_) => {
                    if st.loaded {
                        refresh(st);
                    }
                }
                (ID_DICTS, BN_CLICKED_) => do_select_dicts(hwnd, st),
                (ID_ADV, BN_CLICKED_) => {
                    st.advanced = !st.advanced;
                    if !st.advanced {
                        for h in [st.qw, st.qm, st.qt] {
                            set_text(h, "");
                        }
                    }
                    layout(hwnd, st);
                    if st.loaded {
                        refresh(st);
                    }
                    let _ = SetFocus(if st.advanced { st.qw } else { st.edit });
                }
                (ID_NEW, BN_CLICKED_) => do_new(hwnd, st),
                (ID_EDITBTN, BN_CLICKED_) => do_edit(hwnd, st),
                (ID_OPENLINK, BN_CLICKED_) => do_open_link(hwnd, st),
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
                            let _ = SetFocus(st.edit);
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
            let Some(st) = state(hwnd) else { return LRESULT(0) };
            if hdr.idFrom == ID_RICH {
                // 本文中のリンクのクリック
                if hdr.code == EN_LINK {
                    let en = &*(lparam.0 as *const ENLINK);
                    if en.msg == WM_LBUTTONUP {
                        let (a, b) = (en.chrg.cpMin, en.chrg.cpMax);
                        if b > a && b - a < 2048 {
                            let mut buf = vec![0u16; (b - a) as usize + 1];
                            let mut tr = TEXTRANGEW { chrg: CHARRANGE { cpMin: a, cpMax: b }, lpstrText: PWSTR(buf.as_mut_ptr()) };
                            let n = send(st.rich, EM_GETTEXTRANGE, 0, &mut tr as *mut _ as isize).0.max(0) as usize;
                            let text = String::from_utf16_lossy(&buf[..n.min(buf.len() - 1)]);
                            // 「参照先」に並べたリンクは、表示名ではなく登録された開く先を使う。
                            // それ以外(意味の文中の URL)は、表示されている文字列がそのまま開く先
                            let target = st.links.borrow().iter().find(|l| a >= l.0 && a < l.1).map(|l| l.2.clone());
                            let link = target.unwrap_or(text);
                            open_link(hwnd, st, &link);
                        }
                    }
                }
                return LRESULT(0);
            }
            if hdr.idFrom != ID_LIST {
                return LRESULT(0);
            }
            if hdr.code == NM_CUSTOMDRAW {
                let cd = &*(lparam.0 as *const NMLVCUSTOMDRAW);
                return match cd.nmcd.dwDrawStage.0 {
                    0x1 => LRESULT(0x20),     // CDDS_PREPAINT → CDRF_NOTIFYITEMDRAW
                    0x10001 => {
                        // CDDS_ITEMPREPAINT: 行を自前で描く → CDRF_SKIPDEFAULT
                        draw_row(st, cd.nmcd.hdc, cd.nmcd.dwItemSpec);
                        LRESULT(0x4)
                    }
                    _ => LRESULT(0),
                };
            }
            if hdr.code == LVN_GETDISPINFOW {
                // 描画は自前だが、読み上げ等のために見出し語を返しておく
                let di = &mut *(lparam.0 as *mut NMLVDISPINFOW);
                if di.item.mask.0 & LVIF_TEXT.0 != 0 {
                    let text = st.matches.get(di.item.iItem as usize).map(|&i| st.entries[i as usize].word.clone()).unwrap_or_default();
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
            LRESULT(0)
        }
        WM_APP_RELOAD => {
            // 別プロセス(--add-sample)が設定を書き換えたので読み直す
            if let Some(st) = state(hwnd) {
                reload_config(hwnd, st);
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
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
