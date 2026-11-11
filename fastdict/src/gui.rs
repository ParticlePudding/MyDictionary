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
const ID_WORD: usize = 104;
const ID_TAGS: usize = 105;
const ID_MEANING: usize = 106;
const ID_ADD: usize = 107;
const ID_DEL: usize = 108;
const ID_LABEL: usize = 109;
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
    word: HWND,
    tags: HWND,
    meaning: HWND,
    label: HWND,
    btn_add: HWND,
    btn_del: HWND,
    font: HFONT,
    notice: Option<String>,
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
            // 登録欄で Ctrl+Enter → 登録
            if msg.message == WM_KEYDOWN && msg.wParam.0 == VK_RETURN.0 as usize && GetKeyState(VK_CONTROL.0 as i32) < 0 {
                if let Some(st) = state(hwnd) {
                    let f = GetFocus();
                    if f == st.word || f == st.tags || f == st.meaning {
                        let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_ADD | (BN_CLICKED as usize) << 16), LPARAM(0));
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
        WS_VSCROLL.0 | ES_MULTILINE as u32 | ES_READONLY as u32 | ES_AUTOVSCROLL as u32,
        ID_DETAIL,
    );
    let word = create_child(hwnd, w!("EDIT"), WS_EX_CLIENTEDGE.0, WS_TABSTOP.0 | ES_AUTOHSCROLL as u32, ID_WORD);
    let cue = wide("見出し語");
    send(word, EM_SETCUEBANNER, 1, cue.as_ptr() as isize);
    let tags = create_child(hwnd, w!("EDIT"), WS_EX_CLIENTEDGE.0, WS_TABSTOP.0 | ES_AUTOHSCROLL as u32, ID_TAGS);
    let cue = wide("タグ(スペース区切り・任意)");
    send(tags, EM_SETCUEBANNER, 1, cue.as_ptr() as isize);
    let label = create_child(hwnd, w!("STATIC"), 0, 0, ID_LABEL);
    set_text(label, "意味(Ctrl+Enter で登録)");
    let meaning = create_child(
        hwnd,
        w!("EDIT"),
        WS_EX_CLIENTEDGE.0,
        WS_TABSTOP.0 | WS_VSCROLL.0 | ES_MULTILINE as u32 | ES_WANTRETURN as u32 | ES_AUTOVSCROLL as u32,
        ID_MEANING,
    );
    let btn_add = create_child(hwnd, w!("BUTTON"), 0, WS_TABSTOP.0, ID_ADD);
    set_text(btn_add, "登録");
    let btn_del = create_child(hwnd, w!("BUTTON"), 0, WS_TABSTOP.0, ID_DEL);
    set_text(btn_del, "選択を削除");
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
    for h in [edit, list, detail, status, word, tags, label, meaning, btn_add, btn_del] {
        send(h, WM_SETFONT, font.0 as usize, 1);
    }
    let _ = SetFocus(edit);

    State {
        edit,
        list,
        detail,
        status,
        word,
        tags,
        meaning,
        label,
        btn_add,
        btn_del,
        font,
        notice: None,
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
    let (m, row_h, status_h, detail_h, label_h, meaning_h, btn_w) =
        (s(8), s(28), s(20), s(90), s(18), s(64), s(100));
    let w = rc.right - 2 * m;
    // 下から積む: ステータス / 意味+ボタン / ラベル / 見出し語+タグ / 詳細 、残りが一覧
    let status_y = rc.bottom - m - status_h;
    let meaning_y = status_y - m - meaning_h;
    let label_y = meaning_y - label_h;
    let word_y = label_y - m / 2 - row_h;
    let detail_y = word_y - m - detail_h;
    let list_y = m + row_h + m;
    let list_h = (detail_y - m - list_y).max(s(40));
    let _ = MoveWindow(st.edit, m, m, w, row_h, true);
    let _ = MoveWindow(st.list, m, list_y, w, list_h, true);
    let _ = MoveWindow(st.detail, m, detail_y, w, detail_h, true);
    let word_w = w * 6 / 10;
    let _ = MoveWindow(st.word, m, word_y, word_w, row_h, true);
    let _ = MoveWindow(st.tags, m + word_w + m, word_y, w - word_w - m, row_h, true);
    let _ = MoveWindow(st.label, m, label_y, w, label_h, true);
    let _ = MoveWindow(st.meaning, m, meaning_y, w - btn_w - m, meaning_h, true);
    let bh = (meaning_h - s(4)) / 2;
    let _ = MoveWindow(st.btn_add, m + w - btn_w, meaning_y, btn_w, bh, true);
    let _ = MoveWindow(st.btn_del, m + w - btn_w, meaning_y + bh + s(4), btn_w, bh, true);
    let _ = MoveWindow(st.status, m, status_y, w, status_h, true);
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
    st.notice = None;
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
        let last = core::last_update(&st.entries).map(core::format_jst).unwrap_or_else(|| "-".into());
        format!("{} 件 / 全 {} 件  最終更新: {}", st.matches.len(), st.entries.len(), last)
    };
    let text = match &st.notice {
        Some(n) => format!("{n}  |  {text}"),
        None => text,
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

unsafe fn notice(st: &mut State, msg: &str) {
    st.notice = Some(msg.to_string());
    update_status(st);
}

unsafe fn message_box(hwnd: HWND, text: &str, caption: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    let (t, c) = (wide(text), wide(caption));
    MessageBoxW(hwnd, PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), style)
}

unsafe fn do_add(hwnd: HWND, st: &mut State) {
    if !st.loaded || st.load_error.is_some() {
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
    match core::open(&db_path()).and_then(|c| core::insert(&c, &word, &meaning, &tags)) {
        Ok(entry) => {
            st.entries.push(entry);
            core::sort_entries(&mut st.entries);
            set_text(st.word, "");
            set_text(st.tags, "");
            set_text(st.meaning, "");
            refresh(st);
            notice(st, &format!("「{word}」を登録しました"));
            let _ = SetFocus(st.word);
        }
        Err(e) => {
            message_box(hwnd, &format!("登録に失敗しました: {e}"), "エラー", MB_OK | MB_ICONERROR);
        }
    }
}

unsafe fn do_delete(hwnd: HWND, st: &mut State) {
    let sel = send(st.list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize).0;
    let Some(&idx) = (if sel >= 0 { st.matches.get(sel as usize) } else { None }) else {
        notice(st, "削除する行を選択してください");
        return;
    };
    let idx = idx as usize;
    let (id, word) = (st.entries[idx].id, st.entries[idx].word.clone());
    let ask = message_box(
        hwnd,
        &format!("「{word}」を削除しますか?\nこの操作は元に戻せません。"),
        "削除の確認",
        MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
    );
    if ask != IDYES {
        return;
    }
    match core::open(&db_path()).and_then(|c| core::delete(&c, id)) {
        Ok(_) => {
            st.entries.remove(idx);
            refresh(st);
            notice(st, &format!("「{word}」を削除しました"));
        }
        Err(e) => {
            message_box(hwnd, &format!("削除に失敗しました: {e}"), "エラー", MB_OK | MB_ICONERROR);
        }
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
            } else if id == ID_ADD && code == BN_CLICKED {
                if let Some(st) = state(hwnd) {
                    do_add(hwnd, st);
                }
            } else if id == ID_DEL && code == BN_CLICKED {
                if let Some(st) = state(hwnd) {
                    do_delete(hwnd, st);
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
                    } else if hdr.code == LVN_KEYDOWN {
                        let kd = &*(lparam.0 as *const NMLVKEYDOWN);
                        if kd.wVKey == VK_DELETE.0 {
                            do_delete(hwnd, st);
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
