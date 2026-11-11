//! Win32 GUI (windows-rs)。標準コントロールのみ使用。
//! - ウィンドウを先に表示し、DBの読み込みは別スレッドで行う
//! - 検索欄の変更ごとにメモリ上で絞り込み、仮想リストビューへ反映
//! - トレイ常駐 + グローバルホットキー(Ctrl+Alt+D)、二重起動時は既存ウィンドウを呼び出す

use crate::core::{self, Entry};
use std::ffi::c_void;
use std::path::PathBuf;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CLASS_NAME: PCWSTR = w!("FastDictMainWindow");
const ID_EDIT: usize = 100;
const ID_LIST: usize = 101;
const ID_DETAIL: usize = 102;
const ID_STATUS: usize = 103;
const HOTKEY_ID: i32 = 1;
const WM_APP_LOADED: u32 = WM_APP + 1;
const WM_APP_TRAY: u32 = WM_APP + 2;
const WM_APP_SHOW: u32 = WM_APP + 3;
const MENU_EXIT: usize = 1;

struct State {
    edit: HWND,
    list: HWND,
    detail: HWND,
    status: HWND,
    font: HFONT,
    entries: Vec<Entry>,
    matches: Vec<u32>,
    loaded: bool,
    load_error: Option<String>,
    cell: Vec<u16>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn db_path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("fastdict").join("dictionary.db")
}

unsafe fn state<'a>(hwnd: HWND) -> Option<&'a mut State> {
    let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
    if p.is_null() { None } else { Some(&mut *p) }
}

unsafe fn send(h: HWND, msg: u32, w: usize, l: isize) -> LRESULT {
    SendMessageW(h, msg, WPARAM(w), LPARAM(l))
}

pub fn run() {
    unsafe {
        let background = std::env::args().any(|a| a == "--background");

        // 二重起動: 既存ウィンドウを呼び出して終了
        let _mutex = windows::Win32::System::Threading::CreateMutexW(None, true, w!("Local\\FastDictSingleInstance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(existing) = FindWindowW(CLASS_NAME, PCWSTR::null()) {
                let _ = PostMessageW(existing, WM_APP_SHOW, WPARAM(0), LPARAM(0));
            }
            return;
        }

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

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            w!("辞書"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            900,
            640,
            None,
            None,
            hinst,
            None,
        )
        .unwrap();

        // ウィンドウを先に出す。読み込みは裏で。
        if !background {
            show_window(hwnd);
        }
        let _ = RegisterHotKey(hwnd, HOTKEY_ID, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 'D' as u32);
        add_tray_icon(hwnd);

        let h = hwnd.0 as usize;
        std::thread::spawn(move || {
            let result: Result<Vec<Entry>, String> = core::open(&db_path())
                .and_then(|c| core::load_all(&c))
                .map_err(|e| e.to_string());
            let boxed = Box::into_raw(Box::new(result));
            let hwnd = HWND(h as *mut c_void);
            if PostMessageW(hwnd, WM_APP_LOADED, WPARAM(0), LPARAM(boxed as isize)).is_err() {
                drop(Box::from_raw(boxed));
            }
        });

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
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

unsafe fn add_tray_icon(hwnd: HWND) {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_APP_TRAY,
        hIcon: LoadIconW(None, IDI_APPLICATION).unwrap(),
        ..Default::default()
    };
    for (i, c) in "辞書 (Ctrl+Alt+D)".encode_utf16().enumerate() {
        nid.szTip[i] = c;
    }
    let _ = Shell_NotifyIconW(NIM_ADD, &nid);
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

unsafe fn create_child(
    parent: HWND,
    class: PCWSTR,
    ex: u32,
    style: u32,
    id: usize,
) -> HWND {
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

unsafe fn on_create(hwnd: HWND) -> State {
    let edit = create_child(
        hwnd,
        w!("EDIT"),
        WS_EX_CLIENTEDGE.0,
        (WS_TABSTOP | WS_BORDER).0 & !WS_BORDER.0 | ES_AUTOHSCROLL as u32,
        ID_EDIT,
    );
    let cue = wide("検索(スペース区切りでAND、大文字小文字は区別しません)");
    send(edit, EM_SETCUEBANNER, 1, cue.as_ptr() as isize);

    let list = create_child(
        hwnd,
        w!("SysListView32"),
        WS_EX_CLIENTEDGE.0,
        WS_TABSTOP.0 | LVS_REPORT as u32 | LVS_OWNERDATA as u32 | LVS_SINGLESEL as u32 | LVS_SHOWSELALWAYS as u32,
        ID_LIST,
    );
    send(
        list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        0,
        (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP) as isize,
    );
    let _ = SetWindowTheme(list, w!("Explorer"), PCWSTR::null());
    for (i, (title, width)) in [("見出し語", 220), ("意味", 460), ("タグ", 120)].iter().enumerate() {
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
        WS_TABSTOP.0 | WS_VSCROLL.0 | ES_MULTILINE as u32 | ES_READONLY as u32 | ES_AUTOVSCROLL as u32,
        ID_DETAIL,
    );
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
    for h in [edit, list, detail, status] {
        send(h, WM_SETFONT, font.0 as usize, 1);
    }
    let _ = SetFocus(edit);

    State {
        edit,
        list,
        detail,
        status,
        font,
        entries: Vec::new(),
        matches: Vec::new(),
        loaded: false,
        load_error: None,
        cell: Vec::new(),
    }
}

unsafe fn layout(hwnd: HWND, st: &State) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let dpi = GetDpiForWindow(hwnd).max(96) as i32;
    let s = |v: i32| v * dpi / 96;
    let (m, edit_h, status_h, detail_h) = (s(8), s(28), s(20), s(130));
    let w = rc.right - 2 * m;
    let mut y = m;
    let _ = MoveWindow(st.edit, m, y, w, edit_h, true);
    y += edit_h + m;
    let list_h = (rc.bottom - y - m - detail_h - m - status_h - m).max(s(40));
    let _ = MoveWindow(st.list, m, y, w, list_h, true);
    y += list_h + m;
    let _ = MoveWindow(st.detail, m, y, w, detail_h, true);
    y += detail_h + m;
    let _ = MoveWindow(st.status, m, y, w, status_h, true);
}

unsafe fn get_text(h: HWND) -> String {
    let len = GetWindowTextLengthW(h);
    let mut buf = vec![0u16; len as usize + 1];
    let n = GetWindowTextW(h, &mut buf);
    String::from_utf16_lossy(&buf[..n as usize])
}

unsafe fn set_text(h: HWND, s: &str) {
    let w = wide(s);
    let _ = SetWindowTextW(h, PCWSTR(w.as_ptr()));
}

unsafe fn refresh(st: &mut State) {
    let q = get_text(st.edit);
    st.matches = core::filter(&st.entries, &q);
    send(st.list, LVM_SETITEMCOUNT, st.matches.len(), 0);
    // 先頭行を選択して詳細に表示
    let item = LVITEMW {
        stateMask: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
        state: if st.matches.is_empty() { LIST_VIEW_ITEM_STATE_FLAGS(0) } else { LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0) },
        ..Default::default()
    };
    send(st.list, LVM_SETITEMSTATE, 0, &item as *const _ as isize);
    let _ = InvalidateRect(st.list, None, true);
    if st.matches.is_empty() {
        set_text(st.detail, "");
    }
    update_status(st);
}

unsafe fn update_status(st: &State) {
    let text = if let Some(e) = &st.load_error {
        format!("読み込みエラー: {e}")
    } else if !st.loaded {
        "読み込み中…".to_string()
    } else {
        format!("{} 件 / 全 {} 件", st.matches.len(), st.entries.len())
    };
    set_text(st.status, &text);
}

unsafe fn show_detail(st: &State, row: usize) {
    if let Some(&i) = st.matches.get(row) {
        let e = &st.entries[i as usize];
        let mut s = format!("{}\r\n", e.word);
        if !e.tags.is_empty() {
            s.push_str(&format!("タグ: {}\r\n", e.tags));
        }
        s.push_str(&format!("更新: {}\r\n\r\n", core::format_jst(e.updated_at)));
        s.push_str(&e.meaning.replace("\r\n", "\n").replace('\n', "\r\n"));
        set_text(st.detail, &s);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            let st = Box::new(on_create(hwnd));
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
            let result = Box::from_raw(lparam.0 as *mut Result<Vec<Entry>, String>);
            if let Some(st) = state(hwnd) {
                match *result {
                    Ok(v) => st.entries = v,
                    Err(e) => st.load_error = Some(e),
                }
                st.loaded = true;
                refresh(st); // 読み込み中に入力された語句をここで反映
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = wparam.0 & 0xFFFF;
            let code = (wparam.0 >> 16) as u32;
            if id == ID_EDIT && code == EN_CHANGE {
                if let Some(st) = state(hwnd) {
                    if st.loaded {
                        refresh(st);
                    }
                }
            } else if id == IDCANCEL.0 as usize {
                let _ = ShowWindow(hwnd, SW_HIDE); // Esc で隠す
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
                                        _ => e.tags.clone(),
                                    }
                                })
                                .unwrap_or_default();
                            st.cell = wide(&text);
                            di.item.pszText = PWSTR(st.cell.as_mut_ptr());
                        }
                    } else if hdr.code == LVN_ITEMCHANGED {
                        let nm = &*(lparam.0 as *const NMLISTVIEW);
                        if nm.uChanged.0 & LVIF_STATE.0 != 0 && nm.uNewState & LVIS_SELECTED.0 != 0 {
                            show_detail(st, nm.iItem as usize);
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
                        let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("終了"));
                        let mut pt = POINT::default();
                        let _ = GetCursorPos(&mut pt);
                        let _ = SetForegroundWindow(hwnd);
                        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
                        let _ = DestroyMenu(menu);
                        if cmd.0 as usize == MENU_EXIT {
                            let _ = DestroyWindow(hwnd);
                        }
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // 閉じるボタンは常駐のため隠すだけ(終了はトレイメニュー)
            let _ = ShowWindow(hwnd, SW_HIDE);
            LRESULT(0)
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
