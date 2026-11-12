//! Win32 GUI (windows-rs)。電子辞書風の2ペイン: 左に候補リスト、右に本文。
//! - 起動直後から検索欄に入力でき、↑↓で候補を移動できる
//! - 候補リストは仮想リスト + カスタム描画(見出し語と意味の抜粋、検索語を赤で強調)
//! - 本文は RichEdit(見出し語を大きく、検索語を赤で強調、選択してコピー可)
//! - 登録・編集・設定はモーダルダイアログ。書き込みは safe モジュール経由(バックアップ + 検査 + ロールバック)
//! - DBの読み込みは別スレッド。他の人の変更はウィンドウを前面に出したときに検知して読み直す

use crate::bookmarks::{Bookmark, Bookmarks};
use crate::config::{self, Config, Sort};
use crate::core::{self, Entry, Query};
use crate::dialogs::{self, EntryDlg, SelectDlg, SettingsDlg, TagAction, TagsDlg};
use crate::safe::{self, WriteError};
use crate::pics;
use crate::undo::{self, Snapshot, Undo};
use crate::ui::{self, quiet, scale, Btn, Fonts, BTN_W, CTL_H};
use crate::ui::{get_text, send, set_text, wide};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, LoadLibraryW};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Controls::RichEdit::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS_NAME: PCWSTR = w!("MyDictionaryMainWindow");
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
const ID_SORT: usize = 121; // 並び順
const ID_BMVIEW: usize = 122; // しおりだけ表示
const ID_BM: usize = 123; // しおりを付ける/外す
const ID_BACK: usize = 124; // リンクで移動する前の状態に戻る(Alt+←、マウスの戻るボタン)
/// 戻れる回数の上限
const BACK_MAX: usize = 50;
const ID_UNDO: usize = 125; // 下段の「元に戻す」のリンク
const ID_TAGS: usize = 126; // 絞り込みのタグ欄の横の「一覧…」
/// 本文のタグのリンクを表す開く先の接頭辞(例: `tag:IT`)。クリックすると、そのタグで検索する
const TAG_SCHEME: &str = "tag:";
const HOTKEY_ID: i32 = 1;
const WM_APP_LOADED: u32 = WM_APP + 1;
const WM_APP_TRAY: u32 = WM_APP + 2;
const WM_APP_SHOW: u32 = WM_APP + 3;
const WM_APP_LINK: u32 = WM_APP + 5; // 本文のリンクがクリックされた(lParam = Box<String>)
const WM_APP_IMAGE: u32 = WM_APP + 6; // 本文の画像を読み終えた(lParam = Box<PicMsg>)
const MENU_OPEN: usize = 1;
const MENU_SETTINGS: usize = 2;
const MENU_OPEN_CFG: usize = 3;
const MENU_RELOAD: usize = 4;
const MENU_EXIT: usize = 5;
// 右クリックメニュー
const CM_COPY: usize = 201;
const CM_SELECT_ALL: usize = 202;
const CM_OPEN_LINK: usize = 203;
const CM_COPY_LINK: usize = 204;
const CM_COPY_WORD: usize = 211;
const CM_COPY_MEANING: usize = 212;
const CM_COPY_BOTH: usize = 213;
const CM_EDIT: usize = 214;
const CM_DELETE: usize = 215;
const CM_BOOKMARK: usize = 216;
const CM_UNDO: usize = 217;
const CM_SORT_BASE: usize = 300;

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
    /// 辞書に記録されている、いちばん新しいアプリの版(`core::stored_version`。読めなければ 0)
    version: i64,
}

struct Loaded {
    gen: u32,
    entries: Vec<Entry>,
    dicts: Vec<LoadedDict>,
}

/// 検索の状態と、選んでいた語。リンクで別の語へ移動する前に控えておき、Alt+← で戻る
struct View {
    all: String,
    qw: String,
    qm: String,
    qt: String,
    advanced: bool,
    bm_view: bool,
    selected: Option<(usize, i64)>,
}

/// 直前の操作(登録・編集・削除・取り込み)を元に戻すための記録。起動中だけ持つ
struct UndoOp {
    /// 何を元に戻すのか(例:「3 件の削除」)
    label: String,
    /// 辞書ごとの記録。辞書は、設定が変わっても探せるようにパスで持つ
    parts: Vec<(PathBuf, Undo)>,
    /// 削除と一緒に外したしおり(元に戻すときに付け直す)
    bookmarks: Vec<Bookmark>,
}

/// 読み込んだ画像(ファイルの中身と、大きさ)
struct Pic {
    bytes: Vec<u8>,
    w: u32,
    h: u32,
}

/// 本文の中の画像1つ。はじめは説明の文字(リンク)を表示しておき、画像を読み終えたら、その範囲を画像に置き換える
struct ImgSlot {
    /// いまの本文欄の上での範囲(文字位置)。画像に置き換えた後は、画像の1文字ぶん
    start: i32,
    end: i32,
    /// 画像のパス(相対パスは、辞書のフォルダを基準に直したもの)
    path: String,
    alt: String,
    /// 画像を表示しているときの、表示の大きさ(ピクセル)。クリックの判定に使う
    shown: Option<(i32, i32)>,
}

/// 画像を読むスレッドからの結果
struct PicMsg {
    /// どの表示のための画像か(`State::detail_gen`。表示が変わっていたら捨てる)
    gen: u32,
    index: usize,
    path: String,
    stamp: Option<SystemTime>,
    pic: Option<Arc<Pic>>,
}

/// 覚えておく画像の数(同じ語を見直したときや、ウィンドウの大きさを変えたときに、読み直さない)
const PIC_CACHE: usize = 24;

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
    btn_bmview: HWND,
    btn_sort: HWND,
    list_sep: i32,
    list: HWND,
    imglist: HIMAGELIST,
    // 右: 本文
    rich: HWND,
    rich_ok: bool,
    btn_bm: HWND,
    btn_edit: HWND,
    btn_del: HWND,
    status: HWND,
    hint: HWND,
    /// 下段の左端の「元に戻す」(元に戻せる操作があるときだけ表示)
    undo_link: HWND,
    undo_w: i32,
    /// 下段を、幅いっぱいに使っているか(操作の結果などの案内を出している間は、右のキー操作の案内を隠す)
    status_wide: Cell<bool>,

    fonts: Fonts,
    dpi: i32,
    fields: Vec<(RECT, HWND)>,
    list_box: RECT,
    pane: RECT,
    status_top: i32,

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

    /// マウスが乗っているリンクの開く先(左下に表示する)
    hover_link: Option<String>,
    /// リンクで移動する前の状態(新しいものが後ろ)。起動中だけ持ち、保存はしない
    back: Vec<View>,
    /// 直前の操作を元に戻すための記録
    undo: Option<UndoOp>,
    /// 本文を表示し直すたびに増える番号(前の表示のための画像を捨てるため。画像を読むスレッドも見る)
    detail_gen: Arc<AtomicU32>,
    /// いま表示している語の画像
    img_slots: RefCell<Vec<ImgSlot>>,
    /// 最近読んだ画像 (パス, ファイルの更新時刻, 画像)
    pic_cache: RefCell<Vec<(String, Option<SystemTime>, Arc<Pic>)>>,
    /// 画像を本文に入れる方法: 0 = まだ試していない、1 = EM_INSERTIMAGE、2 = DIB に変換して RTF で入れる、3 = 入れられない(説明の文字のまま)
    img_mode: Cell<u8>,
    /// ウィンドウの大きさを変えている途中か(終わってから、画像の大きさを合わせ直す)
    in_sizemove: Cell<bool>,
    /// 画像の大きさを決めたときの、本文欄の幅
    img_width: Cell<i32>,
    /// 「新しい版のアプリでも使われている」と知らせた辞書(同じ知らせを繰り返さない)
    warned_version: HashSet<PathBuf>,
    /// 絞り込みのタグ欄の横の「一覧…」
    btn_tags: HWND,

    /// しおり(各自の設定フォルダに保存)
    bookmarks: Bookmarks,
    bm_path: PathBuf,
    /// (辞書の添字, id) → しおりの追加日時。描画・絞り込み用の索引
    bm_map: HashMap<(usize, i64), i64>,
    /// しおりだけを表示中
    bm_view: bool,
    sort: Sort,
    /// しおり表示に入る前の並び順
    prev_sort: Sort,
}

fn app_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("MyDictionary")
}

/// バックアップ置き場(必ずローカル。ローミングされない LocalAppData を使う)
fn backup_root() -> PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(p) => PathBuf::from(p).join("MyDictionary").join("backup"),
        None => app_dir().join("backup"),
    }
}

/// 設定ファイルの場所: exe と同じフォルダにあればそれ(ポータブル)、なければ %AppData%\MyDictionary
fn config_location() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("MyDictionary.ini");
            if p.exists() {
                return p;
            }
        }
    }
    app_dir().join("MyDictionary.ini")
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
        let _mutex = CreateMutexW(None, true, w!("Local\\MyDictionarySingleInstance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(existing) = FindWindowW(CLASS_NAME, PCWSTR::null()) {
                let _ = PostMessageW(existing, WM_APP_SHOW, WPARAM(0), LPARAM(0));
            }
            return;
        }

        let init = load_config();
        // 配色は、ウィンドウを作る前に決める(設定を変えても、次に起動するまでは変わらない)
        ui::init_theme(init.cfg.theme);
        // 本文に画像を入れる処理が COM を使う
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES | ICC_HOTKEY_CLASS | ICC_LINK_CLASS,
        };
        let _ = InitCommonControlsEx(&icc);

        let hinst = GetModuleHandleW(None).unwrap();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap(),
            hbrBackground: CreateSolidBrush(ui::pal().bg),
            lpszClassName: CLASS_NAME,
            hIcon: ui::app_icon(),
            hIconSm: ui::app_icon(),
            ..Default::default()
        };
        RegisterClassExW(&wc);

        let init_ptr = Box::into_raw(Box::new(init));
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            w!("MyDictionary"),
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
        ui::apply_dark(hwnd);

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
            // Alt+← / マウスの戻るボタン: リンクで移動する前の状態に戻る
            let back_key = msg.message == WM_SYSKEYDOWN && msg.wParam.0 == VK_LEFT.0 as usize;
            let back_button = msg.message == WM_XBUTTONUP && (msg.wParam.0 >> 16) & 0xFFFF == 1 /* XBUTTON1 */;
            if (back_key || back_button) && GetActiveWindow() == hwnd {
                let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_BACK | BN_CLICKED_ << 16), LPARAM(0));
                continue;
            }
            if msg.message == WM_KEYDOWN && GetActiveWindow() == hwnd {
                let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
                let vk = msg.wParam.0 as u16;
                let cmd = if ctrl && vk == 'N' as u16 {
                    Some(ID_NEW)
                } else if ctrl && vk == VK_OEM_COMMA.0 {
                    Some(ID_SETTINGS)
                } else if ctrl && vk == 'O' as u16 {
                    Some(ID_OPENLINK)
                } else if ctrl && vk == 'D' as u16 {
                    Some(ID_BM)
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
            if ui::select_all_if_edit(&msg) {
                continue;
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
        let (t, c) = (wide(&text), wide("MyDictionary ベンチマーク"));
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
        hIcon: ui::app_icon(),
        ..Default::default()
    };
    for (i, c) in tip.encode_utf16().take(120).enumerate() {
        nid.szTip[i] = c;
    }
    nid
}

unsafe fn add_tray_icon(hwnd: HWND) {
    let _ = Shell_NotifyIconW(NIM_ADD, &tray_data(hwnd, ui::APP_NAME));
}

unsafe fn set_tray_tip(hwnd: HWND, st: &State) {
    let tip = match &st.cfg.hotkey {
        Some(h) => format!("{} ({})", config::window_title(&st.cfg, ui::APP_NAME), h.text),
        None => config::window_title(&st.cfg, ui::APP_NAME),
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
        st.back.clear(); // 辞書の添字が変わるので、戻る先は持ち越さない
        update_dict_button(st);
        rebuild_bm_map(st);
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
            let r = if *ro { core::open_ro(path) } else { core::open(path) }.and_then(|c| Ok((core::load_all(&c)?, core::stored_version(&c).unwrap_or(0))));
            match r {
                Ok((mut v, version)) => {
                    for e in &mut v {
                        e.dict = i;
                    }
                    entries.append(&mut v);
                    // 新規作成された場合は作成後の状態を記録する
                    let stamp = stamp.or_else(|| safe::file_stamp(path));
                    dicts.push(LoadedDict { error: None, corrupt: false, stamp, version });
                }
                Err(e) => dicts.push(LoadedDict { corrupt: safe::is_corrupt(&e), error: Some(e.to_string()), stamp, version: 0 }),
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
    let cue = wide("検索 — 見出し語・読み・意味・タグ(スペースで AND、#タグ、* はワイルドカード)");
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
    let btn_tags = owner_button(hwnd, ID_TAGS, "一覧…");
    let _ = ShowWindow(btn_tags, SW_HIDE);
    let qw = adv_edit(ID_QW, "見出し語・読みで絞り込み");
    let qm = adv_edit(ID_QM, "意味で絞り込み");
    let qt = adv_edit(ID_QT, "タグで絞り込み(複数はスペース区切りで AND)");

    let btn_bmview = owner_button(hwnd, ID_BMVIEW, "★ しおり");
    let btn_sort = owner_button(hwnd, ID_SORT, "並び: 見出し語順");
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
            send(h, EM_SETBKGNDCOLOR, 0, ui::pal().bg.0 as isize);
            if ui::pal().dark {
                // 書式を付けていない文字(既定は黒)が、暗い地の上で見えなくならないように、既定の文字の色を決めておく
                let mut cf = CHARFORMAT2W::default();
                cf.Base.cbSize = std::mem::size_of::<CHARFORMAT2W>() as u32;
                cf.Base.dwMask = CFM_COLOR;
                cf.Base.crTextColor = ui::pal().text;
                send(h, EM_SETCHARFORMAT, 0 /* SCF_DEFAULT */, &cf as *const _ as isize);
            }
            // リンクは自前で範囲を管理し、マウス操作(EN_MSGFILTER)から判定する。
            // RichEdit の URL 自動検出は、URL の形をしていない表示名のリンク属性を消してしまうので使わない
            send(h, EM_AUTOURLDETECT, 0, 0);
            send(h, EM_SETEVENTMASK, 0, 0x0002_0000 /* ENM_MOUSEEVENTS */);
            // リンクの上ではマウスカーソルを指のマークにする
            let _ = SetWindowSubclass(h, Some(rich_subclass), 1, hwnd.0 as usize);
            (h, true)
        }
        None => (create_child(hwnd, w!("EDIT"), 0, rich_style, ID_RICH).unwrap(), false),
    };
    let btn_bm = owner_button(hwnd, ID_BM, "☆ しおり");
    let btn_edit = owner_button(hwnd, ID_EDITBTN, "編集");
    let btn_del = owner_button(hwnd, ID_DEL, "削除");
    let status = create_child(hwnd, w!("STATIC"), 0, 0x0000000C | 0x80 /* SS_LEFTNOWORDWRAP | SS_NOPREFIX */, ID_STATUS).unwrap();
    let hint = create_child(hwnd, w!("STATIC"), 0, 0x2 | 0x80 /* SS_RIGHT | SS_NOPREFIX */, ID_HINT).unwrap();
    let undo_link = create_child(hwnd, w!("SysLink"), 0, 0, ID_UNDO).unwrap_or_default();
    let _ = ShowWindow(undo_link, SW_HIDE);
    set_text(hint, "↑↓ 候補    Ctrl+N 新規    F2 編集    Ctrl+D しおり    Esc 閉じる");

    let bm_path = init.cfg_path.with_file_name("bookmarks.tsv");
    let sort = init.cfg.sort;
    let init_shared = init.cfg.shared;
    let st = State {
        edit,
        btn_dict,
        adv,
        qw,
        qm,
        qt,
        btn_new,
        btn_set,
        btn_bmview,
        btn_sort,
        list_sep: 0,
        list,
        imglist,
        rich,
        rich_ok,
        btn_bm,
        btn_edit,
        btn_del,
        status,
        hint,
        undo_link,
        undo_w: 0,
        status_wide: Cell::new(false),
        fonts,
        dpi,
        fields,
        list_box: RECT::default(),
        pane: RECT::default(),
        status_top: 0,
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
        hover_link: None,
        back: Vec::new(),
        undo: None,
        detail_gen: Arc::new(AtomicU32::new(0)),
        img_slots: RefCell::new(Vec::new()),
        pic_cache: RefCell::new(Vec::new()),
        img_mode: Cell::new(0),
        in_sizemove: Cell::new(false),
        img_width: Cell::new(0),
        warned_version: HashSet::new(),
        btn_tags,
        bookmarks: if init_shared { Bookmarks::default() } else { Bookmarks::load(&bm_path) },
        bm_path,
        bm_map: HashMap::new(),
        bm_view: false,
        sort,
        prev_sort: sort,
    };
    apply_fonts(&st);
    update_sort_button(&st);
    apply_mode(hwnd, &st);
    let _ = SetFocus(edit);
    st
}

unsafe fn apply_fonts(st: &State) {
    send(st.undo_link, WM_SETFONT, st.fonts.small.0 as usize, 1);
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
    for h in [st.qw, st.qm, st.qt, st.btn_tags] {
        let _ = ShowWindow(h, adv_cmd);
    }
    if st.advanced {
        let fh = s(CTL_H);
        let cw = (w - 2 * gap) / 3;
        // タグ欄の右に、タグの一覧を開くボタン
        let tw = s(84);
        let (tx, tag_w) = (m + 2 * (cw + gap), w - 2 * (cw + gap));
        ui::place_field(st.qw, (m, top, cw, fh), dpi, false, &mut st.fields);
        ui::place_field(st.qm, (m + cw + gap, top, cw, fh), dpi, false, &mut st.fields);
        ui::place_field(st.qt, (tx, top, tag_w - tw - gap, fh), dpi, false, &mut st.fields);
        let _ = MoveWindow(st.btn_tags, tx + tag_w - tw, top, tw, fh, true);
        top += fh + s(12);
    }

    // --- 下端: ステータス(左)とキー操作の案内(右) ---
    let status_h = s(30);
    st.status_top = rc.bottom - status_h;
    layout_status(st);
    let bottom = st.status_top - s(12);
    let h = (bottom - top).max(s(80));

    // --- 左: 候補リスト(枠の内側に置く) ---
    let list_w = s(320); // 候補リストの幅は固定(ウィンドウを広げた分は、本文の側が広がる)
    st.list_box = RECT { left: m, top, right: m + list_w, bottom: top + h };
    let inset = s(2);
    // 枠の上部: しおり表示の切替と並び順
    let hp = s(8);
    if st.cfg.shared {
        // 共用フォルダでの運用: しおりは使わないので、並び順のボタンを幅いっぱいにする
        let _ = MoveWindow(st.btn_sort, m + hp, top + hp, list_w - 2 * hp, s(CTL_H), true);
    } else {
        let _ = MoveWindow(st.btn_bmview, m + hp, top + hp, s(BTN_W), s(CTL_H), true);
        let _ = MoveWindow(st.btn_sort, m + hp + s(BTN_W) + gap, top + hp, list_w - 2 * hp - s(BTN_W) - gap, s(CTL_H), true);
    }
    st.list_sep = top + hp + s(CTL_H) + hp;
    let list_top = st.list_sep + 1;
    let _ = MoveWindow(st.list, m + inset, list_top, list_w - 2 * inset, top + h - inset - list_top, true);
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
    let _ = MoveWindow(st.btn_bm, ix, btn_y, bw, btn_h, true);
    // 絞り込み欄の出し入れで残像が出ないよう、背景ごと描き直す
    let _ = RedrawWindow(hwnd, None, None, RDW_INVALIDATE | RDW_ERASE | RDW_ALLCHILDREN);
}

/// 下段の配置: 左端に「元に戻す」(元に戻せる操作があるときだけ)、その右に件数と操作の結果、右端にキー操作の案内。
/// 操作の結果などの案内を出している間(`status_wide`)は、キー操作の案内を隠して、幅いっぱいに表示する
unsafe fn layout_status(st: &State) {
    let mut rc = RECT::default();
    let _ = GetClientRect(GetParent(st.status).unwrap_or_default(), &mut rc);
    let s = |v: i32| scale(v, st.dpi);
    let (m, gap) = (s(16), s(8));
    let w = rc.right - 2 * m;
    let wide = st.status_wide.get();
    let hint_w = if wide { 0 } else { (w * 62 / 100).min(s(600)) };
    let y = st.status_top + s(6);
    let undo_w = if st.undo.is_some() { st.undo_w.min(w - hint_w - gap - s(80)).max(0) } else { 0 };
    let _ = ShowWindow(st.undo_link, if undo_w > 0 { SW_SHOW } else { SW_HIDE });
    let _ = MoveWindow(st.undo_link, m, y, undo_w, s(20), true);
    let sx = m + undo_w + if undo_w > 0 { s(14) } else { 0 };
    let _ = MoveWindow(st.status, sx, y, (m + w - hint_w - if wide { 0 } else { gap } - sx).max(0), s(20), true);
    let _ = ShowWindow(st.hint, if wide { SW_HIDE } else { SW_SHOW });
    let _ = MoveWindow(st.hint, m + w - hint_w, y, hint_w, s(20), true);
}

unsafe fn paint(hwnd: HWND, st: &State) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    ui::fill(hdc, &rc, ui::pal().bg);
    ui::fill(hdc, &RECT { left: 0, top: st.status_top, right: rc.right, bottom: st.status_top + 1 }, ui::pal().border);
    let r = scale(12, st.dpi);
    ui::round_box(hdc, &st.list_box, ui::pal().bg, ui::pal().border, r);
    ui::round_box(hdc, &st.pane, ui::pal().bg, ui::pal().border, r);
    ui::fill(hdc, &RECT { left: st.list_box.left + 1, top: st.list_sep, right: st.list_box.right - 1, bottom: st.list_sep + 1 }, ui::pal().hairline);
    ui::paint_fields(hdc, &st.fields, st.dpi);
    let _ = EndPaint(hwnd, &ps);
}

// ---- 候補リストの行描画 -------------------------------------------------------

unsafe fn draw_row(st: &State, hdc: HDC, row: usize) {
    let mut rc = RECT::default(); // left = LVIR_BOUNDS(0)
    send(st.list, LVM_GETITEMRECT_, row, &mut rc as *mut _ as isize);
    // 行は、欄の左端から描く(環境によって、行の左端が 2 ピクセルずれて返ることがあり、選択の印が消え残るため)
    rc.left = 0;
    let Some(&idx) = st.matches.get(row) else { return };
    let e = &st.entries[idx as usize];
    let s = |v: i32| scale(v, st.dpi);
    let selected = send(st.list, LVM_GETITEMSTATE_, row, LVIS_SELECTED.0 as isize).0 as u32 & LVIS_SELECTED.0 != 0;

    ui::fill(hdc, &rc, if selected { ui::pal().accent_soft } else { ui::pal().bg });
    if selected {
        ui::fill(hdc, &RECT { left: rc.left, top: rc.top + s(6), right: rc.left + s(4), bottom: rc.bottom - s(6) }, ui::pal().accent);
    }
    ui::fill(hdc, &RECT { left: rc.left + s(14), top: rc.bottom - 1, right: rc.right, bottom: rc.bottom }, ui::pal().hairline);

    let (x0, x1) = (rc.left + s(16), rc.right - s(12));
    let (y1, y2) = (rc.top + s(9), rc.top + s(33));
    let old = SelectObject(hdc, st.fonts.small);

    // 1行目の右: タグ / 2行目の右: 辞書名(複数辞書のとき)
    let tags = core::hash_tags(&e.tags);
    let tags_w = if tags.is_empty() { 0 } else { ui::text_width(hdc, &tags).min((x1 - x0) * 45 / 100) };
    if tags_w > 0 {
        let r = RECT { left: x1 - tags_w, top: y1 + s(3), right: x1, bottom: y2 };
        ui::draw_marked(hdc, &tags, &core::mark_ranges(&tags, &st.marks.2), &r, ui::pal().muted, ui::pal().mark);
    }
    let dict = if st.dicts.len() > 1 { st.dicts.get(e.dict).map_or("", |d| d.name.as_str()) } else { "" };
    let dict_w = if dict.is_empty() { 0 } else { ui::text_width(hdc, dict).min((x1 - x0) * 40 / 100) };
    if dict_w > 0 {
        let r = RECT { left: x1 - dict_w, top: y2, right: x1, bottom: rc.bottom };
        ui::draw_marked(hdc, dict, &[], &r, ui::pal().muted, ui::pal().mark);
    }

    // 2行目: 意味の抜粋
    // 先頭の一部だけを使う。本文と同じく [[語]] の括弧は外し、改行は空白にする
    let head: String = e.meaning.chars().take(240).collect();
    let snippet: String = core::body_links(&head).0.chars().map(|c| if c == '\r' || c == '\n' { ' ' } else { c }).collect();
    let r2 = RECT { left: x0, top: y2, right: x1 - dict_w - if dict_w > 0 { s(10) } else { 0 }, bottom: rc.bottom };
    ui::draw_marked(hdc, &snippet, &core::mark_ranges(&snippet, &st.marks.1), &r2, ui::pal().muted, ui::pal().mark);

    // しおり付きは、タグの左に ★
    let star_w = if st.bm_map.contains_key(&(e.dict, e.id)) {
        let sw = ui::text_width(hdc, "★");
        let sx = x1 - tags_w - if tags_w > 0 { s(8) } else { 0 } - sw;
        ui::draw_marked(hdc, "★", &[], &RECT { left: sx, top: y1 + s(3), right: sx + sw, bottom: y2 }, ui::pal().star, ui::pal().mark);
        sw + s(8)
    } else {
        0
    };

    // 1行目: 見出し語
    SelectObject(hdc, st.fonts.heading);
    let r1 = RECT { left: x0, top: y1, right: x1 - tags_w - star_w - if tags_w > 0 { s(10) } else { 0 }, bottom: y2 };
    ui::draw_marked(hdc, &e.word, &core::mark_ranges(&e.word, &st.marks.0), &r1, ui::pal().text, ui::pal().mark);
    // 見出し語の右に、読みを小さく添える(かなの種類を区別せずに強調)
    if !e.reading.is_empty() {
        let word_w = ui::text_width(hdc, &e.word);
        SelectObject(hdc, st.fonts.small);
        let rr = RECT { left: r1.left + word_w + s(10), top: y1 + s(3), right: r1.right, bottom: y2 };
        if rr.right - rr.left > s(24) {
            ui::draw_marked(hdc, &e.reading, &reading_marks(&e.reading, &st.marks.0), &rr, ui::pal().muted, ui::pal().mark);
        }
    }
    SelectObject(hdc, old);
}

/// 読みの中の強調範囲(かなの種類などは、検索と同じ規則でそろえて照合される)
fn reading_marks(reading: &str, word_terms: &[String]) -> Vec<(usize, usize)> {
    core::mark_ranges(reading, word_terms)
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
    // しおり表示: しおり付きの語だけ残す(検索はその中で効く)
    if st.bm_view {
        let (entries, map) = (&st.entries, &st.bm_map);
        st.matches.retain(|&i| map.contains_key(&(entries[i as usize].dict, entries[i as usize].id)));
    }
    // 並び順(安定ソートなので、同じ日時の語は見出し語順のまま)
    {
        let (entries, map) = (&st.entries, &st.bm_map);
        match st.sort {
            Sort::Word => {}
            Sort::UpdatedAsc => st.matches.sort_by_key(|&i| entries[i as usize].updated_at),
            Sort::UpdatedDesc => st.matches.sort_by_key(|&i| std::cmp::Reverse(entries[i as usize].updated_at)),
            Sort::Bookmark => st.matches.sort_by_key(|&i| map.get(&(entries[i as usize].dict, entries[i as usize].id)).copied().unwrap_or(i64::MAX)),
        }
    }

    quiet(|| {
        send(st.list, LVM_SETITEMCOUNT, st.matches.len(), 0);
        send(st.list, LVM_SETCOLUMNWIDTH_, 0, -2); // スクロールバーの有無に合わせて幅を取り直す
    });
    select_row(st, if st.matches.is_empty() { None } else { Some(0) });
    let _ = InvalidateRect(st.list, None, true);
    update_detail(st);
    update_status(st);
}

/// リンクの開く先を、左下に表示するための文字列にする
fn link_label(target: &str) -> String {
    if let Some(tag) = target.strip_prefix(TAG_SCHEME) {
        return format!("クリックすると、タグ「{tag}」の語を表示します");
    }
    if let Some(path) = target.strip_prefix(core::IMAGE_SCHEME) {
        return format!("画像: {path}(クリックすると開きます)");
    }
    match target.strip_prefix(core::WORD_SCHEME) {
        Some(word) => format!("リンク先: この辞書の「{}」", word.trim()),
        None => format!("リンク先: {target}"),
    }
}

/// マウスが乗っているリンクが変わったら、左下の表示を切り替える
unsafe fn set_hover_link(st: &mut State, target: Option<String>) {
    if st.hover_link != target {
        st.hover_link = target;
        update_status(st);
    }
}

unsafe fn update_status(st: &State) {
    let errs: Vec<String> = st
        .dicts
        .iter()
        .filter_map(|d| d.error.as_ref().map(|e| format!("辞書「{}」を読み込めません: {}", d.name, e)))
        .collect();
    // 案内(リンクの開く先、操作の結果、エラー)を出している間は、下段を幅いっぱいに使う
    let wide = st.hover_link.is_some() || st.notice.is_some() || !errs.is_empty();
    if st.status_wide.replace(wide) != wide {
        layout_status(st);
    }
    // リンクの上にマウスがある間は、開く先を表示する(クリックする前に確かめられるように)
    if let Some(target) = &st.hover_link {
        set_text(st.status, &link_label(target));
        return;
    }
    let text = if !st.loaded {
        "読み込み中…".to_string()
    } else if st.bm_view {
        // しおり表示中は、しおりの件数を基準にする
        let total = st.entries.iter().filter(|e| st.bm_map.contains_key(&(e.dict, e.id))).count();
        if st.matches.len() == total {
            format!("しおり {total} 件")
        } else {
            format!("{} 件 / しおり {} 件", st.matches.len(), total)
        }
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

/// 範囲を、リンクの見た目にする(クリックは、自前の表 `State::links` で判定する)。
/// 明るい配色では、RichEdit のリンクの書式(青い文字に下線)を使う。
/// 暗い配色では、その青が暗い地の上で読みにくいので、色と下線を自分で付ける
unsafe fn rich_link(rich: HWND, range: (usize, usize)) {
    let cr = CHARRANGE { cpMin: range.0 as i32, cpMax: range.1 as i32 };
    send(rich, EM_EXSETSEL, 0, &cr as *const _ as isize);
    let mut cf = CHARFORMAT2W::default();
    cf.Base.cbSize = std::mem::size_of::<CHARFORMAT2W>() as u32;
    if ui::pal().dark {
        cf.Base.dwMask = CFM_COLOR | CFM_UNDERLINE;
        cf.Base.dwEffects = CFE_UNDERLINE;
        cf.Base.crTextColor = ui::pal().accent;
    } else {
        cf.Base.dwMask = CFM_LINK;
        cf.Base.dwEffects = CFE_LINK;
    }
    send(rich, EM_SETCHARFORMAT, SCF_SELECTION as usize, &cf as *const _ as isize);
}

/// 本文を描く: 見出し語(大)/ 補足(小・灰)/ 意味 / 参照先(末尾にまとめる)。
/// `*_marks` は各部分の中での強調範囲(UTF-16)。
#[allow(clippy::too_many_arguments)]
unsafe fn render(
    st: &State,
    title: &str,
    reading: &str,
    meta: &str,
    body: &str,
    body_links: &[(usize, usize, String)],
    links: &[(String, String)],
    title_marks: &[(usize, usize)], meta_marks: &[(usize, usize)], body_marks: &[(usize, usize)],
    meta_links: &[(usize, usize, String)],
    images: &[(usize, usize, String)],
    base: &Path,
) {
    // 前の表示のための画像は、もう使わない
    let gen = st.detail_gen.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    st.img_slots.borrow_mut().clear();
    let body = body.replace("\r\n", "\n");
    // 文字位置は UTF-16 単位で数える。RichEdit 内部では改行は1文字なので、'\n' = 1 のままでよい
    let mut text = String::new();
    let push = |text: &mut String, s: &str| -> (usize, usize) {
        let a = len16(text);
        text.push_str(s);
        (a, a + len16(s))
    };
    let r_title = push(&mut text, title);
    let mut r_reading = (0, 0);
    if !reading.is_empty() {
        push(&mut text, "\n");
        r_reading = push(&mut text, reading);
    }
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
    rich_format(st.rich, r_title, Some(20), true, ui::pal().text);
    rich_format(st.rich, r_reading, Some(11), false, ui::pal().muted);
    for (a, b) in reading_marks(reading, &st.marks.0) {
        rich_format(st.rich, (r_reading.0 + a, r_reading.0 + b), None, false, ui::pal().mark);
    }
    rich_format(st.rich, r_meta, Some(9), false, ui::pal().muted);
    rich_format(st.rich, r_gap, Some(6), false, ui::pal().muted); // 見出し部分と意味の間の空行は小さく
    rich_format(st.rich, r_body, Some(12), false, ui::pal().text);
    rich_format(st.rich, r_gap2, Some(8), false, ui::pal().muted);
    rich_format(st.rich, r_head, Some(9), true, ui::pal().muted);
    for (i, (bullet, name)) in r_links.iter().enumerate() {
        rich_format(st.rich, *bullet, Some(10), false, ui::pal().muted);
        rich_format(st.rich, *name, Some(10), false, ui::pal().accent);
        rich_link(st.rich, *name);
        store.push((name.0 as i32, name.1 as i32, links[i].1.clone()));
    }
    // 意味の文中のリンク([[語]] と URL)
    for (a, b, target) in body_links {
        let r = (r_body.0 + a, r_body.0 + b);
        rich_format(st.rich, r, None, false, ui::pal().accent);
        rich_link(st.rich, r);
        store.push((r.0 as i32, r.1 as i32, target.clone()));
    }
    // 補足の行のリンク(タグ。クリックすると、そのタグで検索する)
    for (a, b, target) in meta_links {
        let r = (r_meta.0 + a, r_meta.0 + b);
        rich_link(st.rich, r);
        store.push((r.0 as i32, r.1 as i32, target.clone()));
    }
    for &(a, b) in title_marks {
        rich_format(st.rich, (r_title.0 + a, r_title.0 + b), None, true, ui::pal().mark);
    }
    for &(a, b) in meta_marks {
        rich_format(st.rich, (r_meta.0 + a, r_meta.0 + b), None, false, ui::pal().mark);
    }
    for &(a, b) in body_marks {
        rich_format(st.rich, (r_body.0 + a, r_body.0 + b), None, true, ui::pal().mark);
    }
    let cr = CHARRANGE { cpMin: 0, cpMax: 0 };
    send(st.rich, EM_EXSETSEL, 0, &cr as *const _ as isize);
    send(st.rich, WM_VSCROLL, 6 /* SB_TOP */, 0);
    send(st.rich, WM_SETREDRAW, 1, 0);
    let _ = InvalidateRect(st.rich, None, true);
    drop(store);
    // 画像: まず説明の文字(リンク)を表示してあるので、別スレッドで読んでから、その範囲を画像に置き換える
    if !images.is_empty() && st.img_mode.get() != 3 {
        let mut slots = st.img_slots.borrow_mut();
        // 自動で読むのは、ファイルのパスだけ(URL などは、説明の文字のリンクのままにする)
        for (a, b, path) in images.iter().filter(|i| pics::loadable(&i.2)).take(pics::MAX_IMAGES) {
            let alt: String = body.encode_utf16().skip(*a).take(b - a).map(|u| char::from_u32(u as u32).unwrap_or(' ')).collect();
            slots.push(ImgSlot { start: (r_body.0 + a) as i32, end: (r_body.0 + b) as i32, path: core::resolve_path(path, base), alt, shown: None });
        }
        drop(slots);
        start_pics(st, gen);
    }
}

/// 本文欄の、文字を並べる領域の幅
unsafe fn rich_width(st: &State) -> i32 {
    let mut rc = RECT::default();
    send(st.rich, EM_GETRECT, 0, &mut rc as *mut _ as isize);
    rc.right - rc.left
}

/// いま表示している語の画像を、別スレッドで読む(遅い共有フォルダでも、画面を止めない)。
/// 読み終えた画像は、1枚ずつ `WM_APP_IMAGE` で届く
unsafe fn start_pics(st: &State, gen: u32) {
    st.img_width.set(rich_width(st));
    let wanted: Vec<(usize, String)> = st.img_slots.borrow().iter().enumerate().map(|(i, s)| (i, s.path.clone())).collect();
    // すでに読んである画像(ファイルが変わっていなければ、読み直さない)
    let cached: Vec<(String, Option<SystemTime>, Arc<Pic>)> = st.pic_cache.borrow().iter().filter(|c| wanted.iter().any(|w| w.1 == c.0)).cloned().collect();
    let hwnd = GetParent(st.rich).unwrap_or_default().0 as usize;
    let live = st.detail_gen.clone();
    std::thread::spawn(move || {
        for (index, path) in wanted {
            // ほかの語に移っていたら、残りは読まない
            if live.load(Ordering::Relaxed) != gen {
                return;
            }
            let meta = std::fs::metadata(&path).ok();
            let stamp = meta.as_ref().and_then(|m| m.modified().ok());
            let pic = match cached.iter().find(|c| c.0 == path && c.1 == stamp && stamp.is_some()) {
                Some(c) => Some(c.2.clone()),
                None => meta.filter(|m| m.is_file() && m.len() <= pics::MAX_BYTES).and_then(|_| std::fs::read(&path).ok()).and_then(|bytes| {
                    let (_, w, h) = pics::probe(&bytes)?;
                    Some(Arc::new(Pic { bytes, w, h }))
                }),
            };
            let boxed = Box::into_raw(Box::new(PicMsg { gen, index, path, stamp, pic }));
            if PostMessageW(HWND(hwnd as *mut c_void), WM_APP_IMAGE, WPARAM(0), LPARAM(boxed as isize)).is_err() {
                drop(Box::from_raw(boxed));
                return;
            }
        }
    });
}

/// 本文欄の、その位置の文字(UTF-16 の1単位)。画像などの埋め込みは U+FFFC
unsafe fn rich_char_at(rich: HWND, pos: i32) -> u16 {
    let mut buf = [0u16; 4];
    let tr = TEXTRANGEW { chrg: CHARRANGE { cpMin: pos, cpMax: pos + 1 }, lpstrText: PWSTR(buf.as_mut_ptr()) };
    send(rich, EM_GETTEXTRANGE, 0, &tr as *const _ as isize);
    buf[0]
}

unsafe fn rich_text_len(rich: HWND) -> i32 {
    let g = GETTEXTLENGTHEX { flags: GTL_NUMCHARS, codepage: 1200 };
    send(rich, EM_GETTEXTLENGTHEX, &g as *const _ as usize, 0).0 as i32
}

/// 選択範囲を、画像に置き換える(Windows 8 以降の RichEdit の機能)。ピクセルの大きさは、1/100 mm に直して渡す
unsafe fn insert_image_native(rich: HWND, pic: &Pic, w: i32, h: i32, dpi: i32, alt: &str) -> bool {
    let Some(stream) = SHCreateMemStream(Some(&pic.bytes)) else { return false };
    let alt_w = wide(alt);
    let hm = |px: i32| (px as i64 * 2540 / dpi.max(1) as i64) as i32;
    let params = RICHEDIT_IMAGE_PARAMETERS {
        xWidth: hm(w),
        yHeight: hm(h),
        Ascent: hm(h),
        Type: TA_BASELINE.0 as i32,
        pwszAlternateText: PCWSTR(alt_w.as_ptr()),
        pIStream: std::mem::ManuallyDrop::new(Some(stream)),
    };
    let hr = send(rich, EM_INSERTIMAGE, 0, &params as *const _ as isize).0;
    let RICHEDIT_IMAGE_PARAMETERS { pIStream, .. } = params;
    drop(std::mem::ManuallyDrop::into_inner(pIStream));
    hr == 0
}

/// RTF を読ませるときに、データを渡す関数(`cookie` は、(データ, 読んだ位置) へのポインタ)
unsafe extern "system" fn rtf_read(cookie: usize, buf: *mut u8, cb: i32, pcb: *mut i32) -> u32 {
    let cur = &mut *(cookie as *mut (Vec<u8>, usize));
    let n = (cur.0.len() - cur.1).min(cb.max(0) as usize);
    std::ptr::copy_nonoverlapping(cur.0.as_ptr().add(cur.1), buf, n);
    cur.1 += n;
    *pcb = n as i32;
    0
}

/// 画像を、表示する大きさ `w` x `h` の DIB に変換する(Windows の画像の部品 WIC で読んで、縮小する)。
/// 透明な部分は、本文の背景色の上に重ねる
unsafe fn decode_dib(pic: &Pic, w: i32, h: i32) -> Option<Vec<u8>> {
    let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
    let stream = SHCreateMemStream(Some(&pic.bytes))?;
    let decoder = factory.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
    let frame = decoder.GetFrame(0).ok()?;
    // 不透明度つきの形にそろえてから縮小する
    let conv = factory.CreateFormatConverter().ok()?;
    conv.Initialize(&frame, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
    let scaler = factory.CreateBitmapScaler().ok()?;
    scaler.Initialize(&conv, w as u32, h as u32, WICBitmapInterpolationModeFant).ok()?;
    let mut pixels = vec![0u8; w as usize * h as usize * 4];
    scaler.CopyPixels(std::ptr::null(), w as u32 * 4, &mut pixels).ok()?;
    let bg = ui::pal().bg.0;
    pics::dib_from_pbgra(&pixels, w as usize, h as usize, ((bg & 0xFF) as u8, ((bg >> 8) & 0xFF) as u8, ((bg >> 16) & 0xFF) as u8))
}

/// 選択範囲を、画像に置き換える(画像を DIB に変換して、RTF の形で渡す方法)。
/// `EM_INSERTIMAGE` が使えない環境のための方法。大きさは、1/1440 インチに直して渡す
unsafe fn insert_image_rtf(rich: HWND, pic: &Pic, w: i32, h: i32, dpi: i32) -> bool {
    let Some(mut dib) = decode_dib(pic, w, h) else { return false };
    // RTF の `\dibitmap` は「情報ヘッダ(40 バイト)のすぐ後ろに画素」という並び。
    // Wine の RichEdit は、ヘッダを 44 バイトとして読む(色の表が1つあるものとして数える)ので、
    // Wine の上では 4 バイト空けておく。空けないと、色が1バイトぶんずれて表示される
    if ui::on_wine() {
        dib.splice(40..40, [0u8; 4]);
    }
    let tw = |px: i32| px as i64 * 1440 / dpi.max(1) as i64;
    let mut rtf = format!("{{\\rtf1{{\\pict\\dibitmap0\\picw{w}\\pich{h}\\picwgoal{}\\pichgoal{} ", tw(w), tw(h)).into_bytes();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    rtf.reserve(dib.len() * 2 + 4);
    for &b in &dib {
        rtf.push(HEX[(b >> 4) as usize]);
        rtf.push(HEX[(b & 15) as usize]);
    }
    rtf.extend_from_slice(b"}}");
    let mut cur = (rtf, 0usize);
    let es = EDITSTREAM { dwCookie: &mut cur as *mut _ as usize, dwError: 0, pfnCallback: Some(rtf_read) };
    send(rich, EM_STREAMIN, (SF_RTF | SFF_SELECTION) as usize, &es as *const _ as isize);
    let err = es.dwError;
    err == 0
}

/// 読み終えた画像を、本文の説明の文字と置き換える。
/// 置き換えると文字位置がずれるので、後ろにあるリンクと画像の位置を直す
unsafe fn show_pic(st: &State, index: usize, pic: &Pic) {
    let Some((start, end, alt)) = st.img_slots.borrow().get(index).filter(|s| s.shown.is_none()).map(|s| (s.start, s.end, s.alt.clone())) else { return };
    let mode = st.img_mode.get();
    if mode == 3 {
        return;
    }
    let (w, h) = pics::fit(pic.w, pic.h, st.img_width.get() - scale(8, st.dpi), st.dpi);
    // 選択とスクロールの位置を控えて、置き換えの後で戻す(画像は、読んでいる途中にも届く)
    let mut sel = CHARRANGE::default();
    send(st.rich, EM_EXGETSEL, 0, &mut sel as *mut _ as isize);
    let mut scroll = POINT::default();
    send(st.rich, EM_GETSCROLLPOS, 0, &mut scroll as *mut _ as isize);
    send(st.rich, WM_SETREDRAW, 0, 0);
    send(st.rich, EM_SETREADONLY, 0, 0);
    let before = rich_text_len(st.rich);
    let range = CHARRANGE { cpMin: start, cpMax: end };
    // 入った文字数(画像は1文字ぶん)。0 なら、画像は入っていない。
    // 「成功」が返っても何も起きない環境があるので、その位置の文字が画像になったかで確かめる
    // (画像などの埋め込みは U+FFFC。空白で表す実装もある。説明の文字は、どちらでも始まらない)
    let inserted_by = |ok: bool| {
        let n = rich_text_len(st.rich) - (before - (end - start));
        if ok && (1..=3).contains(&n) && matches!(rich_char_at(st.rich, start), 0xFFFC | 0x20) {
            n
        } else {
            0
        }
    };
    let mut inserted = 0;
    if mode <= 1 {
        send(st.rich, EM_EXSETSEL, 0, &range as *const _ as isize);
        inserted = inserted_by(insert_image_native(st.rich, pic, w, h, st.dpi, &alt));
        if inserted >= 1 {
            st.img_mode.set(1);
        }
    }
    // EM_INSERTIMAGE が使えない環境では、RTF の形で入れてみる
    if inserted == 0 && mode != 1 && rich_text_len(st.rich) == before {
        send(st.rich, EM_EXSETSEL, 0, &range as *const _ as isize);
        inserted = inserted_by(insert_image_rtf(st.rich, pic, w, h, st.dpi));
        if inserted >= 1 {
            st.img_mode.set(2);
        } else if mode == 0 && rich_text_len(st.rich) == before {
            st.img_mode.set(3); // どちらの方法でも入らない: 以後は試さない
        }
    }
    send(st.rich, EM_SETREADONLY, 1, 0);
    let delta = rich_text_len(st.rich) - before;
    if (1..=3).contains(&inserted) {
        // 説明の文字の範囲を、画像の範囲に直す。後ろにあるものは、ずれた分だけ動かす
        let new_end = start + inserted;
        for (a, b, target) in st.links.borrow_mut().iter_mut() {
            if *a == start && *b == end && target.starts_with(core::IMAGE_SCHEME) {
                *b = new_end;
            } else if *a >= end {
                *a += delta;
                *b += delta;
            }
        }
        for (i, s) in st.img_slots.borrow_mut().iter_mut().enumerate() {
            if i == index {
                s.end = new_end;
                s.shown = Some((w, h));
            } else if s.start >= end {
                s.start += delta;
                s.end += delta;
            }
        }
        let fix = |p: i32| if p >= end { p + delta } else { p.min(start) };
        sel = CHARRANGE { cpMin: fix(sel.cpMin), cpMax: fix(sel.cpMax) };
    }
    send(st.rich, EM_EXSETSEL, 0, &sel as *const _ as isize);
    send(st.rich, EM_SETSCROLLPOS, 0, &scroll as *const _ as isize);
    send(st.rich, WM_SETREDRAW, 1, 0);
    let _ = InvalidateRect(st.rich, None, true);
    if delta != 0 && !(1..=3).contains(&inserted) {
        // 思わぬ形で文字が変わった: 位置が合わなくなるので、画像なしで表示し直す
        st.img_mode.set(3);
        update_detail(st);
    }
}

/// ウィンドウの大きさが変わったとき: 画像を表示している語は、幅に合わせて表示し直す
unsafe fn refit_pics(st: &State) {
    if !st.img_slots.borrow().is_empty() && rich_width(st) != st.img_width.get() {
        update_detail(st);
    }
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
            let m = if st.bm_view && st.loaded && !st.entries.is_empty() && st.bm_map.is_empty() {
                "しおりはまだありません。語を選んで「☆ しおり」(Ctrl+D)で付けられます。"
            } else {
                m
            };
            render(st, t, "", m, "", &[], &[], &[], &[], &[], &[], &[], Path::new(""));
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
            // 意味の文中の [[語]]・[表示名](開く先)・URL をリンクに、![説明](パス) を画像にする(表示は、記法を外した形)
            let parsed = core::parse_body(&e.meaning.replace("\r\n", "\n"));
            let (body, body_links) = (parsed.text, parsed.links);
            // 相対パスの基準は、その語の辞書ファイルのあるフォルダ
            let base = st.dicts.get(e.dict).and_then(|d| d.path.parent()).map(Path::to_path_buf).unwrap_or_default();
            // タグは meta の先頭にあるので、強調位置はそのまま使える
            let meta_marks = if tags.is_empty() { Vec::new() } else { core::mark_ranges(&tags, &st.marks.2) };
            // タグ1つずつをリンクにする(「#a #b」の中の位置。UTF-16 単位)
            let mut meta_links: Vec<(usize, usize, String)> = Vec::new();
            let mut pos = 0;
            for t in tags.split(' ').filter(|t| !t.is_empty()) {
                let n = len16(t);
                meta_links.push((pos, pos + n, format!("{TAG_SCHEME}{}", t.trim_start_matches('#'))));
                pos += n + 1;
            }
            render(
                st,
                &e.word,
                &e.reading,
                &meta,
                &body,
                &body_links,
                &core::parse_links(&e.link),
                &core::mark_ranges(&e.word, &st.marks.0),
                &meta_marks,
                &core::mark_ranges(&body, &st.marks.1),
                &meta_links,
                &parsed.images,
                &base,
            );
        }
        n => render(st, &format!("{n} 件を選択中"), "", "Delete キーまたは「削除」でまとめて削除できます。", "", &[], &[], &[], &[], &[], &[], &[], Path::new("")),
    }
    let marked = sel.len() == 1 && st.bm_map.contains_key(&(st.entries[sel[0]].dict, st.entries[sel[0]].id));
    set_text(st.btn_bm, if marked { "★ しおり" } else { "☆ しおり" });
    let _ = EnableWindow(st.btn_bm, sel.len() == 1);
    let _ = InvalidateRect(st.btn_bm, None, false);
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
    if let Some(tag) = link.strip_prefix(TAG_SCHEME) {
        search_tag(st, tag);
        return;
    }
    // 画像: 拡張子が画像のものなら、確認なしで開く(それ以外は、ふつうのリンクと同じ確認をする)
    let (link, plain_image) = match link.strip_prefix(core::IMAGE_SCHEME) {
        Some(p) => (p.to_string(), pics::has_image_ext(p) && pics::loadable(p)),
        None => (link, false),
    };
    match core::link_kind(&link) {
        core::LinkKind::Word => {
            let word = link[core::WORD_SCHEME.len()..].trim().to_string();
            jump_to_word(st, &word);
            return;
        }
        core::LinkKind::Web => {}
        core::LinkKind::Other if plain_image => {}
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
    // 相対パスは、選んでいる語の辞書ファイルのあるフォルダを基準にする
    let base = selected_entries(st).first().and_then(|&i| st.dicts.get(st.entries[i].dict)).and_then(|d| d.path.parent()).map(Path::to_path_buf);
    let target = match &base {
        Some(b) => core::resolve_path(&link, b),
        None => link.clone(),
    };
    let w = wide(&target);
    let r = ShellExecuteW(hwnd, w!("open"), PCWSTR(w.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    if r.0 as isize <= 32 {
        notice(st, &format!("参照先を開けませんでした: {link}"));
    }
}

/// いまの検索の状態と、選んでいる語
unsafe fn current_view(st: &State) -> View {
    let sel = selected_entries(st);
    View {
        all: get_text(st.edit),
        qw: get_text(st.qw),
        qm: get_text(st.qm),
        qt: get_text(st.qt),
        advanced: st.advanced,
        bm_view: st.bm_view,
        selected: if sel.len() == 1 { Some((st.entries[sel[0]].dict, st.entries[sel[0]].id)) } else { None },
    }
}

/// リンクから別の語へ移動する。検索欄をその語にして、完全一致する語を選ぶ。
/// 移動する前の状態は控えておき、Alt+← で戻れるようにする
unsafe fn jump_to_word(st: &mut State, word: &str) {
    let from = current_view(st);
    st.back.push(from);
    if st.back.len() > BACK_MAX {
        st.back.remove(0);
    }
    if st.bm_view {
        set_bm_view(st, false);
    }
    for h in [st.qw, st.qm, st.qt] {
        set_text(h, "");
    }
    set_text(st.edit, &core::search_text_for(word));
    refresh(st);
    // 検索と同じ規則でそろえて、見出し語が一致する語を選ぶ
    let key = core::fold(word.trim());
    let row = st.matches.iter().position(|&i| st.entries[i as usize].word_norm() == key);
    match row {
        Some(r) => {
            select_row(st, Some(r));
            update_detail(st);
            notice(st, &format!("「{word}」へ移動しました(Alt+← で戻る)"));
        }
        None => notice(st, &format!("「{word}」は登録されていません(Ctrl+N で登録できます。Alt+← で戻る)")),
    }
}

/// タグ `tag` が付いた語だけを表示する(本文のタグをクリックしたとき)。Alt+← で、前の表示に戻れる
unsafe fn search_tag(st: &mut State, tag: &str) {
    let from = current_view(st);
    st.back.push(from);
    if st.back.len() > BACK_MAX {
        st.back.remove(0);
    }
    if st.bm_view {
        set_bm_view(st, false);
    }
    for h in [st.qw, st.qm, st.qt] {
        set_text(h, "");
    }
    set_text(st.edit, &core::tag_query(tag));
    refresh(st);
    notice(st, &format!("タグ「{tag}」の語を表示しました(Alt+← で戻る)"));
}

/// タグ欄に `"名前"`(タグの完全一致)の形で書かれている名前
fn quoted_tags(field: &str) -> Vec<String> {
    field
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '，' | '、'))
        .filter_map(|t| {
            let t = t.trim_start_matches(['#', '＃']);
            let inner = t.strip_prefix(['"', '＂'])?.strip_suffix(['"', '＂'])?;
            (!inner.is_empty()).then(|| inner.to_string())
        })
        .collect()
}

/// タグの一覧を開く(絞り込みのタグ欄の横の「一覧…」)。
/// チェックを入れて OK を押すと、そのタグのすべてが付いた語に絞り込む。名前の変更・削除もここから行う
unsafe fn do_tags(hwnd: HWND, st: &mut State) {
    if !st.loaded {
        notice(st, "データベースの読み込みが完了していません");
        return;
    }
    // 検索する辞書に入っている語のタグ
    let mask = st.dict_mask.clone();
    let tags = core::tag_counts(st.entries.iter().filter(|e| mask.get(e.dict).copied().unwrap_or(true)));
    // タグ欄に、すでに "名前" の形で入っているタグには、チェックを入れておく(書き方の違いは区別しない)
    let current: Vec<String> = quoted_tags(&get_text(st.qt)).iter().map(|t| core::fold(t)).collect();
    let checked: Vec<String> = tags.iter().filter(|(t, _)| current.contains(&core::fold(t))).map(|(t, _)| t.clone()).collect();
    let mut dlg = TagsDlg {
        title: "タグの一覧".into(),
        note: "チェックを入れたタグの、すべてが付いた語に絞り込みます。\n名前の変更と削除は、タグの行を選んでから押します。".into(),
        tags,
        checked,
        manage: true,
        action: None,
        ok: false,
    };
    if dialogs::tags_dialog(hwnd, &mut dlg) {
        // 書き方だけが違うタグ(IT と it)は、検索では同じなので、1つにまとめて入れる
        let mut seen: Vec<String> = Vec::new();
        let mut out: Vec<String> = Vec::new();
        for t in &dlg.checked {
            let k = core::fold(t);
            if !seen.contains(&k) {
                seen.push(k);
                out.push(format!("\"{t}\""));
            }
        }
        set_text(st.qt, &out.join(" "));
        refresh(st);
        let _ = SetFocus(st.qt);
    } else if let Some(action) = dlg.action.take() {
        do_retag(hwnd, st, action);
    }
}

/// タグの名前の変更・削除を、書き込める辞書のすべての語に行う(検索する辞書に入っているものだけ)。
/// 直後なら「元に戻す」で取り消せる
unsafe fn do_retag(hwnd: HWND, st: &mut State, action: TagAction) {
    // (変える前のタグ, 変えた後のタグ, 「元に戻す」に出す名前)
    let (from, to, label) = match &action {
        TagAction::Rename { from, to } => (from.clone(), Some(to.clone()), "タグの名前の変更"),
        TagAction::Delete { tag } => (tag.clone(), None, "タグの削除"),
    };
    let has_tag = |e: &Entry| e.tags.split(' ').any(|t| t == from);
    let mut parts: Vec<(PathBuf, Undo)> = Vec::new();
    let (mut changed, mut readonly) = (0usize, 0usize);
    for d in 0..st.dicts.len() {
        if !st.dict_mask.get(d).copied().unwrap_or(true) {
            continue;
        }
        if writable(&st.dicts[d]).is_err() {
            // 参照のみの辞書の語は変えない(件数だけ知らせる)
            readonly += st.entries.iter().filter(|e| e.dict == d && has_tag(e)).count();
            continue;
        }
        let (r, _) = guarded(st, d, |c| undo::retag(c, &from, to.as_deref()));
        match r {
            Ok(list) => {
                changed += list.len();
                if !list.is_empty() {
                    parts.push((st.dicts[d].path.clone(), Undo { updated: list, ..Default::default() }));
                }
            }
            Err(e) => {
                report_write_error(hwnd, st, d, e);
                break;
            }
        }
    }
    if changed > 0 {
        set_undo(hwnd, st, Some(UndoOp { label: label.to_string(), parts, bookmarks: Vec::new() }));
        st.warns.push(match &to {
            Some(to) => format!("タグ「{}」を「{}」に変更しました({changed} 語)", short(&from), short(to)),
            None => format!("タグ「{}」を {changed} 語から外しました", short(&from)),
        });
    } else {
        st.warns.push("変更した語はありません".to_string());
    }
    // 内容を読み直す(結果の案内は、読み終わったときに下段へ出す)
    remember_selection(st);
    start_load(hwnd, st, true);
    if readonly > 0 {
        let text = format!("参照のみの辞書の {readonly} 語は、変えていません。");
        message_box(hwnd, &text, "タグの変更", MB_OK | MB_ICONINFORMATION);
    }
}

/// リンクで移動する前の状態(検索欄・絞り込み・しおり表示・選んでいた語)に戻る
unsafe fn go_back(hwnd: HWND, st: &mut State) {
    let Some(v) = st.back.pop() else {
        notice(st, "戻る先がありません");
        return;
    };
    if !st.loaded {
        return;
    }
    st.hover_link = None;
    if st.bm_view != v.bm_view {
        set_bm_view(st, v.bm_view);
    }
    if st.advanced != v.advanced {
        st.advanced = v.advanced;
        layout(hwnd, st);
        let _ = InvalidateRect(st.adv, None, false);
    }
    for (h, t) in [(st.edit, &v.all), (st.qw, &v.qw), (st.qm, &v.qm), (st.qt, &v.qt)] {
        set_text(h, t);
    }
    refresh(st);
    if let Some((d, id)) = v.selected {
        select_entry(st, d, id);
    }
    notice(st, "リンクで移動する前の表示に戻りました");
}

/// 指定の語を表示する(検索欄をその見出し語にして、その語を選ぶ)
unsafe fn show_entry(st: &mut State, dict: usize, id: i64) {
    let Some(word) = st.entries.iter().find(|e| e.dict == dict && e.id == id).map(|e| e.word.clone()) else { return };
    if st.bm_view {
        set_bm_view(st, false);
    }
    // 検索する辞書から外していた場合は、含める
    if let Some(m) = st.dict_mask.get_mut(dict) {
        if !*m {
            *m = true;
            update_dict_button(st);
        }
    }
    for h in [st.qw, st.qm, st.qt] {
        set_text(h, "");
    }
    set_text(st.edit, &core::search_text_for(&word));
    refresh(st);
    select_entry(st, dict, id);
}

/// 同じ見出し語の語を調べるための索引: そろえた見出し語 → (辞書の添字, id)。`skip` の語(編集中の語)は含めない
fn same_word_index(st: &State, skip: Option<(usize, i64)>) -> HashMap<String, Vec<(usize, i64)>> {
    let mut map: HashMap<String, Vec<(usize, i64)>> = HashMap::with_capacity(st.entries.len());
    for e in &st.entries {
        if skip != Some((e.dict, e.id)) {
            map.entry(e.word_norm().to_string()).or_default().push((e.dict, e.id));
        }
    }
    map
}

// ---- しおり・並び順 ---------------------------------------------------------

/// 設定に応じて、ウィンドウのタイトルと、しおりの部品の表示を切り替える
unsafe fn apply_mode(hwnd: HWND, st: &State) {
    let t = wide(&config::window_title(&st.cfg, ui::APP_NAME));
    let _ = SetWindowTextW(hwnd, PCWSTR(t.as_ptr()));
    // 共用フォルダでの運用では、しおりを使わない(設定の場所に個人の情報を書き込まないため)
    let show = if st.cfg.shared { SW_HIDE } else { SW_SHOW };
    let _ = ShowWindow(st.btn_bmview, show);
    let _ = ShowWindow(st.btn_bm, show);
    set_text(
        st.hint,
        if st.cfg.shared {
            "↑↓ 候補    Ctrl+N 新規    F2 編集    Del 削除    Esc 閉じる"
        } else {
            "↑↓ 候補    Ctrl+N 新規    F2 編集    Ctrl+D しおり    Esc 閉じる"
        },
    );
}

/// しおりの索引を作り直す(辞書の構成やしおりが変わったとき)
unsafe fn rebuild_bm_map(st: &mut State) {
    st.bm_map.clear();
    if st.cfg.shared {
        return; // 共用フォルダでの運用では、しおりを使わない
    }
    let paths: Vec<String> = st.dicts.iter().map(|d| d.path.to_string_lossy().to_lowercase()).collect();
    for b in &st.bookmarks.items {
        let k = b.dict.to_lowercase();
        if let Some(d) = paths.iter().position(|p| *p == k) {
            st.bm_map.insert((d, b.id), b.added);
        }
    }
}

unsafe fn save_bookmarks(st: &mut State) {
    if let Err(e) = st.bookmarks.save(&st.bm_path) {
        notice(st, &format!("しおりを保存できません: {e}"));
    }
}

unsafe fn toggle_bookmark(st: &mut State) {
    if st.cfg.shared {
        notice(st, "共用フォルダでの運用では、しおりは使えません");
        return;
    }
    let sel = selected_entries(st);
    if sel.len() != 1 {
        notice(st, "しおりを付ける語を1つ選択してください");
        return;
    }
    let (d, id, word) = (st.entries[sel[0]].dict, st.entries[sel[0]].id, st.entries[sel[0]].word.clone());
    let path = st.dicts[d].path.to_string_lossy().to_string();
    let on = st.bookmarks.toggle(&path, id, &word, core::now_unix());
    rebuild_bm_map(st);
    if st.bm_view {
        refresh(st);
        select_entry(st, d, id);
    } else {
        let _ = InvalidateRect(st.list, None, false);
        update_detail(st);
    }
    notice(st, &if on { format!("「{word}」にしおりを付けました") } else { format!("「{word}」のしおりを外しました") });
    save_bookmarks(st);
}

fn sort_label(sort: Sort) -> &'static str {
    match sort {
        Sort::Word => "見出し語順",
        Sort::UpdatedAsc => "更新が古い順",
        Sort::UpdatedDesc => "更新が新しい順",
        Sort::Bookmark => "しおり追加順",
    }
}

unsafe fn update_sort_button(st: &State) {
    set_text(st.btn_sort, &format!("並び: {} ▼", sort_label(st.sort)));
    let _ = InvalidateRect(st.btn_sort, None, false);
    let _ = InvalidateRect(st.btn_bmview, None, false);
}

/// しおり表示の切替。入るときは「しおりに追加した順」にし、出るときは元の並び順に戻す
unsafe fn set_bm_view(st: &mut State, on: bool) {
    if st.bm_view == on {
        return;
    }
    st.bm_view = on;
    if on {
        st.prev_sort = st.sort;
        st.sort = Sort::Bookmark;
    } else if st.sort == Sort::Bookmark {
        st.sort = if st.prev_sort == Sort::Bookmark { Sort::Word } else { st.prev_sort };
    }
    st.last_query = None;
    update_sort_button(st);
}

unsafe fn set_sort(st: &mut State, sort: Sort) {
    st.sort = sort;
    update_sort_button(st);
    if sort != Sort::Bookmark && st.cfg.sort != sort && !st.cfg.shared {
        // 次回起動時も同じ並び順にする(共用の設定には書き込まない)
        st.cfg.sort = sort;
        let base = st.cfg_path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let _ = std::fs::write(&st.cfg_path, config::to_ini(&st.cfg, &base));
    }
    if st.loaded {
        remember_selection(st);
        refresh(st);
        if let Some((d, id)) = st.pending_select.take() {
            select_entry(st, d, id);
        }
    }
}

unsafe fn sort_menu(hwnd: HWND, st: &mut State) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let mut items = vec![Sort::Word, Sort::UpdatedAsc, Sort::UpdatedDesc];
    if st.bm_view {
        items.push(Sort::Bookmark);
    }
    for (i, &sort) in items.iter().enumerate() {
        let t = wide(sort_label(sort));
        let flags = if sort == st.sort { MF_STRING | MF_CHECKED } else { MF_STRING };
        let _ = AppendMenuW(menu, flags, CM_SORT_BASE + i, PCWSTR(t.as_ptr()));
    }
    let mut rc = RECT::default();
    let _ = GetWindowRect(st.btn_sort, &mut rc);
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_LEFTBUTTON, rc.left, rc.bottom, 0, hwnd, None);
    let _ = DestroyMenu(menu);
    if let Some(&sort) = (cmd.0 as usize).checked_sub(CM_SORT_BASE).and_then(|i| items.get(i)) {
        set_sort(st, sort);
    }
}

// ---- コピー・右クリックメニュー ------------------------------------------------

/// 選択中の語を、指定の形でクリップボードへコピーする
unsafe fn copy_selected(hwnd: HWND, st: &mut State, what: usize) {
    let sel = selected_entries(st);
    if sel.is_empty() {
        return;
    }
    let lines: Vec<String> = sel
        .iter()
        .map(|&i| {
            let e = &st.entries[i];
            match what {
                CM_COPY_WORD => e.word.clone(),
                CM_COPY_MEANING => e.meaning.clone(),
                _ => format!("{}\t{}", e.word, e.meaning.replace("\r\n", "\n").replace('\n', " ")),
            }
        })
        .collect();
    if ui::set_clipboard(hwnd, &lines.join("\n")) {
        notice(st, &if sel.len() == 1 { "コピーしました".to_string() } else { format!("{} 件をコピーしました", sel.len()) });
    } else {
        notice(st, "コピーできませんでした(クリップボードを他のアプリが使用中です)");
    }
}

/// 候補リストの右クリックメニュー
unsafe fn list_menu(hwnd: HWND, st: &mut State) {
    let sel = selected_entries(st);
    if sel.is_empty() {
        return;
    }
    let Ok(menu) = CreatePopupMenu() else { return };
    let single = sel.len() == 1;
    let editable = sel.iter().all(|&i| writable(&st.dicts[st.entries[i].dict]).is_ok());
    let gray = |on: bool| if on { MF_STRING } else { MF_STRING | MF_GRAYED };
    let marked = single && st.bm_map.contains_key(&(st.entries[sel[0]].dict, st.entries[sel[0]].id));
    let _ = AppendMenuW(menu, MF_STRING, CM_COPY_WORD, w!("見出し語をコピー"));
    let _ = AppendMenuW(menu, MF_STRING, CM_COPY_MEANING, w!("意味をコピー"));
    let _ = AppendMenuW(menu, MF_STRING, CM_COPY_BOTH, w!("見出し語と意味をコピー\tCtrl+C"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    if !st.cfg.shared {
        let _ = AppendMenuW(menu, gray(single), CM_BOOKMARK, if marked { w!("しおりを外す\tCtrl+D") } else { w!("しおりを付ける\tCtrl+D") });
    }
    let _ = AppendMenuW(menu, gray(single && editable), CM_EDIT, w!("編集…\tF2"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, gray(editable), CM_DELETE, w!("削除\tDel"));
    if let Some(op) = &st.undo {
        let t = wide(&format!("元に戻す({})\tCtrl+Z", op.label));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, CM_UNDO, PCWSTR(t.as_ptr()));
    }
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
    let _ = DestroyMenu(menu);
    match cmd.0 as usize {
        c @ (CM_COPY_WORD | CM_COPY_MEANING | CM_COPY_BOTH) => copy_selected(hwnd, st, c),
        CM_BOOKMARK => toggle_bookmark(st),
        CM_EDIT => do_edit(hwnd, st),
        CM_DELETE => do_delete(hwnd, st),
        CM_UNDO => do_undo(hwnd, st),
        _ => {}
    }
}

unsafe fn rich_pos(rich: HWND, index: i32) -> POINTL {
    let mut p = POINTL::default();
    send(rich, EM_POSFROMCHAR, &mut p as *mut _ as usize, index as isize);
    p
}

/// 本文欄の座標 (x, y) にあるリンクの開く先
unsafe fn link_at(st: &State, x: i32, y: i32) -> Option<String> {
    let pt = POINTL { x, y };
    // 画像は、行の高さではなく、表示している大きさで判定する
    for s in st.img_slots.borrow().iter() {
        if let Some((w, h)) = s.shown {
            let p = rich_pos(st.rich, s.start);
            if x >= p.x && x <= p.x + w && y >= p.y && y <= p.y + h {
                return Some(format!("{}{}", core::IMAGE_SCHEME, s.path));
            }
        }
    }
    let idx = send(st.rich, EM_CHARFROMPOS, 0, &pt as *const _ as isize).0 as i32;
    let line_h = scale(26, st.dpi);
    for (a, b, target) in st.links.borrow().iter() {
        if idx < *a || idx > *b {
            continue;
        }
        // 文字位置だけだと、行の右側の余白でも最後の文字とみなされるので、座標でも確かめる
        let (ps, pe) = (rich_pos(st.rich, *a), rich_pos(st.rich, *b));
        let hit = if ps.y == pe.y {
            x >= ps.x - 2 && x <= pe.x + 2 && y >= ps.y && y <= ps.y + line_h
        } else {
            y >= ps.y && y <= pe.y + line_h // 折り返して複数行になったリンク
        };
        if hit {
            return Some(target.clone());
        }
    }
    None
}

/// 本文欄のサブクラス: リンクの上にあるとき、カーソルを指のマークにし、左下に開く先を表示する
unsafe extern "system" fn rich_subclass(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, parent: usize) -> LRESULT {
    let st = state(HWND(parent as *mut c_void));
    match msg {
        WM_SETCURSOR if (lparam.0 & 0xFFFF) as u32 == HTCLIENT => {
            if let Some(st) = st {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let _ = ScreenToClient(hwnd, &mut pt);
                let target = link_at(st, pt.x, pt.y);
                let on_link = target.is_some();
                set_hover_link(st, target);
                if on_link {
                    SetCursor(LoadCursorW(None, IDC_HAND).unwrap());
                    return LRESULT(1);
                }
            }
        }
        WM_MOUSEMOVE => {
            // マウスが本文欄から出たことを知るために、WM_MOUSELEAVE を依頼しておく
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
        }
        0x02A3 /* WM_MOUSELEAVE */ => {
            if let Some(st) = st {
                set_hover_link(st, None);
            }
        }
        _ => {}
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

/// 本文欄の右クリックメニュー(コピー / すべて選択 / リンク)
unsafe fn rich_menu(hwnd: HWND, st: &mut State, x: i32, y: i32) {
    let Ok(menu) = CreatePopupMenu() else { return };
    let mut cr = CHARRANGE::default();
    send(st.rich, EM_EXGETSEL, 0, &mut cr as *mut _ as isize);
    let has_sel = cr.cpMax > cr.cpMin;
    let link = link_at(st, x, y);
    if link.is_some() {
        let _ = AppendMenuW(menu, MF_STRING, CM_OPEN_LINK, w!("リンクを開く"));
        let _ = AppendMenuW(menu, MF_STRING, CM_COPY_LINK, w!("リンク先をコピー"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    }
    let _ = AppendMenuW(menu, if has_sel { MF_STRING } else { MF_STRING | MF_GRAYED }, CM_COPY, w!("コピー\tCtrl+C"));
    let _ = AppendMenuW(menu, MF_STRING, CM_SELECT_ALL, w!("すべて選択\tCtrl+A"));
    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
    let _ = DestroyMenu(menu);
    match cmd.0 as usize {
        CM_COPY => {
            send(st.rich, WM_COPY, 0, 0);
        }
        CM_SELECT_ALL => {
            let all = CHARRANGE { cpMin: 0, cpMax: -1 };
            send(st.rich, EM_EXSETSEL, 0, &all as *const _ as isize);
            let _ = SetFocus(st.rich);
        }
        CM_OPEN_LINK => {
            if let Some(l) = link {
                open_link(hwnd, st, &l);
            }
        }
        CM_COPY_LINK => {
            if let Some(l) = link {
                let shown = [core::WORD_SCHEME, core::IMAGE_SCHEME, TAG_SCHEME].iter().find_map(|p| l.strip_prefix(p)).unwrap_or(&l).to_string();
                ui::set_clipboard(hwnd, &shown);
            }
        }
        _ => {}
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
    // 語の並び(添字)が変わったので、前回の検索結果は再利用しない
    st.last_query = None;
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

/// 見出し語を、案内の文に入れられる長さにする
fn short(word: &str) -> String {
    if word.chars().count() > 20 {
        format!("{}…", word.chars().take(20).collect::<String>())
    } else {
        word.to_string()
    }
}

/// 元に戻せる操作を控える(None なら消す)。下段の「元に戻す」の表示も合わせる
unsafe fn set_undo(hwnd: HWND, st: &mut State, op: Option<UndoOp>) {
    st.undo = op.filter(|o| o.parts.iter().any(|p| !p.1.is_empty()));
    let text = match &st.undo {
        // SysLink は <a> を記号として読むので、語の中の < > は全角にしておく
        Some(o) => {
            let label = o.label.replace('<', "＜").replace('>', "＞");
            if ui::pal().dark {
                // 暗い配色では、全体をリンクにする(リンクでない部分の文字の色は、部品まかせで、暗い地の上で読めないことがある)
                format!("<a>元に戻す({label})</a>")
            } else {
                format!("<a>元に戻す</a>({label})")
            }
        }
        None => String::new(),
    };
    set_text(st.undo_link, &text);
    ui::dark_link(st.undo_link);
    let mut ideal = SIZE::default();
    send(st.undo_link, LM_GETIDEALSIZE, scale(400, st.dpi) as usize, &mut ideal as *mut _ as isize);
    st.undo_w = ideal.cx;
    layout(hwnd, st);
}

/// 直前の操作を元に戻す。ほかの人がその後に変更した語は戻さず、件数を知らせる
unsafe fn do_undo(hwnd: HWND, st: &mut State) {
    let Some(op) = st.undo.take() else {
        notice(st, "元に戻せる操作がありません");
        return;
    };
    let text = format!("直前の操作({})を元に戻しますか?", op.label);
    if message_box(hwnd, &text, "元に戻す", MB_YESNO | MB_ICONQUESTION) != IDYES {
        st.undo = Some(op);
        return;
    }
    let mut total = undo::Reverted::default();
    let mut left: Vec<(PathBuf, Undo)> = Vec::new(); // 戻せなかった分(あとでやり直せるように残す)
    let mut errors: Vec<String> = Vec::new();
    for (path, u) in op.parts {
        let Some(d) = st.dicts.iter().position(|x| x.path == path) else {
            errors.push(format!("辞書 {} は、いまの設定にありません", path.display()));
            left.push((path, u));
            continue;
        };
        if let Err(m) = writable(&st.dicts[d]) {
            errors.push(m);
            left.push((path, u));
            continue;
        }
        let (r, _) = guarded(st, d, |c| undo::revert(c, &u));
        match r {
            Ok(rv) => {
                total.done += rv.done;
                total.skipped += rv.skipped;
            }
            Err(e) => {
                errors.push(format!("辞書「{}」: {}", st.dicts[d].name, e));
                left.push((path, u));
            }
        }
    }
    // 削除と一緒に外したしおりを、付け直す
    if !op.bookmarks.is_empty() && !st.cfg.shared && left.is_empty() {
        for b in op.bookmarks {
            if st.bookmarks.added(&b.dict, b.id).is_none() {
                st.bookmarks.items.push(b);
            }
        }
        rebuild_bm_map(st);
        save_bookmarks(st);
    }
    let retry = if left.is_empty() { None } else { Some(UndoOp { label: op.label.clone(), parts: left, bookmarks: Vec::new() }) };
    set_undo(hwnd, st, retry);
    // 内容を読み直す(読み終わるまで、今の表示のまま)。結果の案内は、読み終わったときに下段へ出す
    if total.done > 0 {
        st.warns.push(format!("元に戻しました({})", op.label));
    }
    remember_selection(st);
    start_load(hwnd, st, true);
    if !errors.is_empty() {
        message_box(hwnd, &format!("元に戻せなかった操作があります。\n\n{}", errors.join("\n")), "元に戻す", MB_OK | MB_ICONWARNING);
    } else if total.skipped > 0 {
        let text = format!(
            "{} 件を元に戻しました。\n\n{} 件は、ほかの人がその後に変更または削除していたため、元に戻していません。",
            total.done, total.skipped
        );
        message_box(hwnd, &text, "元に戻す", MB_OK | MB_ICONINFORMATION);
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
        reading: String::new(),
        tags: String::new(),
        meaning: String::new(),
        link: String::new(),
        choices: targets.iter().map(|&i| st.dicts[i].name.clone()).collect(),
        choice,
        show_choice: targets.len() > 1,
        same_words: same_word_index(st, None),
        dict_names: st.dicts.iter().map(|d| d.name.clone()).collect(),
        targets: targets.clone(),
        orig_word: String::new(),
        open_existing: None,
        all_tags: core::tag_counts(st.entries.iter()),
        dict_paths: st.dicts.iter().map(|d| d.path.clone()).collect(),
        ok: false,
    };
    if !dialogs::entry_dialog(hwnd, &mut dlg) {
        // 「その語を表示」: 登録をやめて、すでにある語を表示する
        if let Some((d, id)) = dlg.open_existing {
            show_entry(st, d, id);
        }
        return;
    }
    let d = targets[dlg.choice];
    let fields = core::Fields { word: dlg.word.clone(), reading: dlg.reading.clone(), meaning: dlg.meaning.clone(), tags: dlg.tags.clone(), link: dlg.link.clone() };
    let (r, stale) = guarded(st, d, |c| core::insert_entry(c, &fields, None));
    match r {
        Ok(entry) => {
            let (id, ts) = (entry.id, entry.updated_at);
            st.entries.push(entry.with_dict(d));
            core::sort_entries(&mut st.entries);
            let op = UndoOp {
                label: format!("「{}」の登録", short(&dlg.word)),
                parts: vec![(st.dicts[d].path.clone(), Undo { inserted: vec![(id, ts)], ..Default::default() })],
                bookmarks: Vec::new(),
            };
            set_undo(hwnd, st, Some(op));
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
        reading: e.reading.clone(),
        tags: e.tags.clone(),
        meaning: e.meaning.clone(),
        link: e.link.clone(),
        choices: Vec::new(),
        choice: 0,
        show_choice: false,
        same_words: same_word_index(st, Some((e.dict, e.id))),
        dict_names: st.dicts.iter().map(|d| d.name.clone()).collect(),
        targets: vec![e.dict],
        orig_word: e.word.clone(),
        open_existing: None,
        all_tags: core::tag_counts(st.entries.iter()),
        dict_paths: st.dicts.iter().map(|d| d.path.clone()).collect(),
        ok: false,
    };
    if !dialogs::entry_dialog(hwnd, &mut dlg) {
        if let Some((d, id)) = dlg.open_existing {
            show_entry(st, d, id);
        }
        return;
    }
    // 読み込んだ時点の更新時刻と一致するときだけ更新する(他の人の変更を上書きしない)
    let fields = core::Fields { word: dlg.word.clone(), reading: dlg.reading.clone(), meaning: dlg.meaning.clone(), tags: dlg.tags.clone(), link: dlg.link.clone() };
    let (r, stale) = guarded(st, e.dict, |c| core::update(c, e.id, e.updated_at, &fields));
    match r {
        Ok(ts) => {
            let op = UndoOp {
                label: format!("「{}」の編集", short(&e.word)),
                parts: vec![(st.dicts[e.dict].path.clone(), Undo { updated: vec![(Snapshot::of(&e), ts)], ..Default::default() })],
                bookmarks: Vec::new(),
            };
            set_undo(hwnd, st, Some(op));
            if let Some(x) = st.entries.iter_mut().find(|x| x.dict == e.dict && x.id == e.id) {
                *x = Entry::new(e.id, dlg.word.clone(), dlg.meaning.clone(), dlg.tags.clone(), ts).with_dict(e.dict).with_link(dlg.link.clone()).with_reading(dlg.reading.clone());
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
    text.push_str("\n削除した直後なら、下段の「元に戻す」で取り消せます。");
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
    // 元に戻せるように、消す前の内容を控える
    let mut undo_parts: Vec<(PathBuf, Undo)> = Vec::new();
    for (d, ids) in by_dict.iter().enumerate() {
        if ids.is_empty() {
            continue;
        }
        let (r, stale) = guarded(st, d, |c| undo::delete_with_snapshots(c, ids));
        match r {
            Ok(snaps) => {
                undo_parts.push((st.dicts[d].path.clone(), Undo { deleted: snaps, ..Default::default() }));
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
    st.last_query = None; // 添字が変わったので、前回の検索結果は再利用しない
    // 削除した語のしおりも外す(元に戻すときに付け直せるよう、控えておく)
    let mut dropped: Vec<Bookmark> = Vec::new();
    for (d, ids) in by_dict.iter().enumerate() {
        let gone: Vec<i64> = ids.iter().copied().filter(|&id| removed.contains(&(d, id))).collect();
        if !gone.is_empty() {
            let path = st.dicts[d].path.to_string_lossy().to_string();
            let key = path.to_lowercase();
            dropped.extend(st.bookmarks.items.iter().filter(|b| gone.contains(&b.id) && b.dict.to_lowercase() == key).cloned());
            st.bookmarks.remove_ids(&path, &gone);
        }
    }
    if !dropped.is_empty() {
        rebuild_bm_map(st);
        save_bookmarks(st);
    }
    if !removed.is_empty() {
        let op = UndoOp { label: format!("{} 件の削除", removed.len()), parts: undo_parts, bookmarks: dropped };
        set_undo(hwnd, st, Some(op));
    }
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
    let mut dlg = SettingsDlg { cfg: st.cfg.clone(), backup_root: backup_root(), cfg_path: st.cfg_path.clone(), imported: false, undo: None, ok: false };
    let saved = dialogs::settings_dialog(hwnd, &mut dlg);
    // 設定の画面で取り込みを行っていたら、元に戻せるようにする
    if let Some((path, u, label)) = dlg.undo.take() {
        set_undo(hwnd, st, Some(UndoOp { label, parts: vec![(path, u)], bookmarks: Vec::new() }));
    }
    if !saved {
        // キャンセルでも、インポートを実行していたら辞書を読み直す
        if dlg.imported {
            start_load(hwnd, st, true);
        }
        return;
    }
    // 共用の設定を書き換えると、同じ設定ファイルを使う全員に影響する
    if st.cfg.shared {
        let text = format!(
            "この設定は、共用フォルダで運用しています。\n保存すると、同じ設定ファイルを使う全員に反映されます。\n\n{}\n\n保存しますか?",
            st.cfg_path.display()
        );
        if message_box(hwnd, &text, "共用の設定の変更", MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) != IDYES {
            if dlg.imported {
                start_load(hwnd, st, true);
            }
            return;
        }
    }
    let mut problems: Vec<String> = Vec::new();
    let dicts_changed = dlg.cfg.dicts != st.cfg.dicts;
    let was_shared = st.cfg.shared;
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
    // 共用 / 個人の切替: しおりの読み込みと、部品の表示を切り替える
    if was_shared != st.cfg.shared {
        if st.bm_view {
            set_bm_view(st, false);
        }
        st.bookmarks = if st.cfg.shared { Bookmarks::default() } else { Bookmarks::load(&st.bm_path) };
        rebuild_bm_map(st);
    }
    apply_mode(hwnd, st);
    layout(hwnd, st);
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
    if st.bm_view {
        set_bm_view(st, false);
    }
    st.bookmarks = if st.cfg.shared { Bookmarks::default() } else { Bookmarks::load(&st.bm_path) };
    apply_mode(hwnd, st);
    layout(hwnd, st);
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
        WM_NEXTDLGCTL => {
            ui::next_dlg_ctl(hwnd, wparam, lparam);
            LRESULT(0)
        }
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
            mmi.ptMinTrackSize.x = scale(960, dpi);
            mmi.ptMinTrackSize.y = scale(520, dpi);
            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(st) = state(hwnd) {
                layout(hwnd, st);
                // 大きさを変えている途中は、終わってから(WM_EXITSIZEMOVE)画像を合わせ直す
                if !st.in_sizemove.get() && wparam.0 != 1 /* SIZE_MINIMIZED */ {
                    refit_pics(st);
                }
            }
            LRESULT(0)
        }
        WM_ENTERSIZEMOVE => {
            if let Some(st) = state(hwnd) {
                st.in_sizemove.set(true);
            }
            LRESULT(0)
        }
        WM_EXITSIZEMOVE => {
            if let Some(st) = state(hwnd) {
                st.in_sizemove.set(false);
                refit_pics(st);
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
            let id = GetDlgCtrlID(HWND(lparam.0 as *mut c_void)) as usize;
            let color = if matches!(id, ID_STATUS | ID_HINT | ID_UNDO) { ui::pal().muted } else { ui::pal().text };
            ui::ctl_color(hwnd, msg, wparam, lparam, color)
        }
        WM_DRAWITEM => {
            let dis = &*(lparam.0 as *const DRAWITEMSTRUCT);
            if let Some(st) = state(hwnd) {
                let kind = match dis.CtlID as usize {
                    ID_NEW => Btn::Primary,
                    ID_ADV => Btn::Toggle(st.advanced),
                    ID_DICTS => Btn::Toggle(!st.dict_mask.iter().all(|&b| b)),
                    ID_DEL => Btn::Danger,
                    ID_BMVIEW => Btn::Toggle(st.bm_view),
                    ID_BM => Btn::Toggle(dis.itemState.0 & 0x4 == 0 && get_text(st.btn_bm).starts_with('★')),
                    _ => Btn::Secondary,
                };
                ui::draw_button(dis, kind, st.fonts.base, ui::pal().bg, st.dpi);
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
                        // 自分より新しい版のアプリが書き込んだ辞書は、起動中に1回だけ知らせる(全員が同じ版を使うため)
                        if ld.version > core::app_version_num() && st.warned_version.insert(d.path.clone()) {
                            st.warns.push(format!(
                                "辞書「{}」は、新しい版({})のアプリでも使われています。アプリを更新してください",
                                d.name,
                                core::version_label(ld.version)
                            ));
                        }
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
                (ID_BM, BN_CLICKED_) => toggle_bookmark(st),
                (ID_BACK, BN_CLICKED_) => go_back(hwnd, st),
                (ID_TAGS, BN_CLICKED_) => do_tags(hwnd, st),
                (ID_SORT, BN_CLICKED_) => sort_menu(hwnd, st),
                (ID_BMVIEW, BN_CLICKED_) => {
                    let on = !st.bm_view;
                    set_bm_view(st, on);
                    if on {
                        // まず全部のしおりが見えるよう、検索語は空にする(この後、しおりの中を検索できる)
                        for h in [st.edit, st.qw, st.qm, st.qt] {
                            set_text(h, "");
                        }
                    }
                    if st.loaded {
                        refresh(st);
                    }
                    let _ = SetFocus(st.edit);
                }
                _ => {
                    if id == IDCANCEL.0 as usize {
                        // Esc: 入力があれば消す。なければ閉じるボタンと同じ動作(設定に応じて終了 / トレイに隠す)
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
                            let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
                        }
                    }
                }
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            let hdr = &*(lparam.0 as *const NMHDR);
            let Some(st) = state(hwnd) else { return LRESULT(0) };
            if hdr.idFrom == ID_UNDO {
                if hdr.code == NM_CLICK || hdr.code == NM_RETURN {
                    do_undo(hwnd, st);
                }
                return LRESULT(0);
            }
            if hdr.idFrom == ID_RICH {
                if hdr.code == EN_MSGFILTER {
                    let mf = &*(lparam.0 as *const MSGFILTER);
                    let (m, lp) = (mf.msg, mf.lParam.0);
                    let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
                    if m == WM_LBUTTONUP {
                        // 文字を選択するドラッグではなく、単なるクリックのときだけリンクを開く
                        let mut cr = CHARRANGE::default();
                        send(st.rich, EM_EXGETSEL, 0, &mut cr as *mut _ as isize);
                        if cr.cpMin == cr.cpMax {
                            if let Some(target) = link_at(st, x, y) {
                                // RichEdit がマウス処理を終えてから開く(処理の途中で本文を書き換えない)
                                let boxed = Box::into_raw(Box::new(target));
                                if PostMessageW(hwnd, WM_APP_LINK, WPARAM(0), LPARAM(boxed as isize)).is_err() {
                                    drop(Box::from_raw(boxed));
                                }
                            }
                        }
                    } else if m == WM_RBUTTONUP {
                        rich_menu(hwnd, st, x, y);
                        return LRESULT(1); // 既定の処理はさせない
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
                    if kd.wVKey == 'C' as u16 && GetKeyState(VK_CONTROL.0 as i32) < 0 {
                        copy_selected(hwnd, st, CM_COPY_BOTH);
                    } else if kd.wVKey == 'Z' as u16 && GetKeyState(VK_CONTROL.0 as i32) < 0 {
                        do_undo(hwnd, st);
                    } else if kd.wVKey == VK_DELETE.0 {
                        do_delete(hwnd, st);
                    } else if kd.wVKey == VK_RETURN.0 {
                        do_edit(hwnd, st);
                    }
                } else if hdr.code == LVN_ITEMCHANGED {
                    let nm = &*(lparam.0 as *const NMLISTVIEW);
                    if (nm.uOldState ^ nm.uNewState) & LVIS_SELECTED.0 != 0 {
                        // 行の左端まで描き直す(部品が描き直す範囲が、行の左端の 2 ピクセルを含まない環境があり、
                        // 選択の印が消え残るため)。iItem が -1 のときは、すべての行
                        if nm.iItem >= 0 {
                            let mut rc = RECT::default();
                            send(st.list, LVM_GETITEMRECT_, nm.iItem as usize, &mut rc as *mut _ as isize);
                            rc.left = 0;
                            let _ = InvalidateRect(st.list, Some(&rc), false);
                        } else {
                            let _ = InvalidateRect(st.list, None, false);
                        }
                        update_detail(st);
                    }
                } else if hdr.code == LVN_ODSTATECHANGED {
                    // 仮想リストでは、範囲選択(Shift+クリック / Ctrl+A)はこの通知で届く
                    let _ = InvalidateRect(st.list, None, false);
                    update_detail(st);
                } else if hdr.code == NM_DBLCLK {
                    do_edit(hwnd, st);
                } else if hdr.code == NM_RCLICK {
                    list_menu(hwnd, st);
                }
            }
            LRESULT(0)
        }
        WM_APP_IMAGE => {
            let m = *Box::from_raw(lparam.0 as *mut PicMsg);
            if let Some(st) = state(hwnd) {
                if let Some(pic) = &m.pic {
                    // 読んだ画像を覚えておく(古いものから捨てる)
                    let mut cache = st.pic_cache.borrow_mut();
                    cache.retain(|c| c.0 != m.path);
                    cache.push((m.path.clone(), m.stamp, pic.clone()));
                    if cache.len() > PIC_CACHE {
                        cache.remove(0);
                    }
                    drop(cache);
                    if m.gen == st.detail_gen.load(Ordering::Relaxed) {
                        show_pic(st, m.index, pic);
                    }
                }
            }
            LRESULT(0)
        }
        WM_APP_LINK => {
            let target = *Box::from_raw(lparam.0 as *mut String);
            if let Some(st) = state(hwnd) {
                st.hover_link = None; // 移動や表示の切替で、リンクの位置が変わる
                open_link(hwnd, st, &target);
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
                        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("MyDictionary を開く"));
                        let _ = AppendMenuW(menu, MF_STRING, MENU_SETTINGS, w!("設定"));
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
            let hide = state(hwnd).map_or(false, |st| st.cfg.close_to_tray);
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
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
