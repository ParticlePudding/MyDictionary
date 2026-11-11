//! モーダルダイアログ: 登録/編集、設定。

use crate::config::{self, Config, DictConf, Hotkey};
use crate::ui::{self, *};
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Once;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::Dialogs::*;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const ID_OK: usize = 1;
const ID_CANCEL: usize = 2;
const EN_SETFOCUS_: usize = 0x0100;
const EN_KILLFOCUS_: usize = 0x0200;
const EN_CHANGE_: usize = 0x0300;
const CB_ADDSTRING: u32 = 0x0143;
const CB_GETCURSEL: u32 = 0x0147;
const CB_SETCURSEL: u32 = 0x014E;
const BM_GETCHECK_: u32 = 0x00F0;
const BM_SETCHECK_: u32 = 0x00F1;
const LVM_DELETEALLITEMS_: u32 = 0x1009;
const LVM_SETITEMTEXTW_: u32 = 0x1074;
const HKM_SETHOTKEY: u32 = 0x0401;
const HKM_GETHOTKEY: u32 = 0x0402;
const HKM_SETRULES: u32 = 0x0403;

static REGISTER: Once = Once::new();

unsafe fn register_classes() {
    REGISTER.call_once(|| {
        let hinst = GetModuleHandleW(None).unwrap();
        for (name, proc) in [
            (w!("FastDictEntry"), entry_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT),
            (w!("FastDictSettings"), settings_proc),
        ] {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(proc),
                hInstance: hinst.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap(),
                hbrBackground: CreateSolidBrush(WHITE),
                lpszClassName: name,
                hIcon: LoadIconW(None, IDI_APPLICATION).unwrap(),
                ..Default::default()
            };
            RegisterClassExW(&wc);
        }
    });
}

/// モーダルダイアログを作って、閉じるまでメッセージを処理する
unsafe fn run_modal(owner: HWND, class: PCWSTR, title: &str, client_w: i32, client_h: i32, param: *const c_void) {
    register_classes();
    let dpi = GetDpiForWindow(owner).max(96) as i32;
    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
    let mut rc = RECT { left: 0, top: 0, right: scale(client_w, dpi), bottom: scale(client_h, dpi) };
    let _ = AdjustWindowRectExForDpi(&mut rc, style, false, WS_EX_DLGMODALFRAME, dpi as u32);
    let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);
    let mut orc = RECT::default();
    let _ = GetWindowRect(owner, &mut orc);
    let x = (orc.left + (orc.right - orc.left - ww) / 2).max(0);
    let y = (orc.top + (orc.bottom - orc.top - wh) / 2).max(0);
    let t = wide(title);
    let hinst = GetModuleHandleW(None).unwrap();
    let hwnd = CreateWindowExW(WS_EX_DLGMODALFRAME, class, PCWSTR(t.as_ptr()), style, x, y, ww, wh, owner, None, hinst, Some(param));
    let Ok(hwnd) = hwnd else { return };

    let _ = EnableWindow(owner, false);
    let _ = ShowWindow(hwnd, SW_SHOW);
    let mut msg = MSG::default();
    while IsWindow(hwnd).as_bool() {
        let r = GetMessageW(&mut msg, None, 0, 0);
        if r.0 == 0 {
            PostQuitMessage(msg.wParam.0 as i32); // WM_QUIT は外側のループへ
            break;
        }
        if r.0 == -1 {
            break;
        }
        // Ctrl+Enter = 保存(複数行欄の中でも)
        if msg.message == WM_KEYDOWN && msg.wParam.0 == VK_RETURN.0 as usize && GetKeyState(VK_CONTROL.0 as i32) < 0 {
            let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_OK), LPARAM(0));
            continue;
        }
        if !IsDialogMessageW(hwnd, &msg).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    let _ = EnableWindow(owner, true);
    let _ = SetForegroundWindow(owner);
}

unsafe fn label(parent: HWND, text: &str, (x, y, w, h): (i32, i32, i32, i32), fnt: HFONT, id: usize) -> HWND {
    let hinst = GetModuleHandleW(None).unwrap();
    let t = wide(text);
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("STATIC"),
        PCWSTR(t.as_ptr()),
        WS_CHILD | WS_VISIBLE,
        x,
        y,
        w,
        h,
        parent,
        HMENU(id as *mut c_void),
        hinst,
        None,
    )
    .unwrap();
    send(hwnd, WM_SETFONT, fnt.0 as usize, 1);
    hwnd
}

unsafe fn button(parent: HWND, text: &str, id: usize, (x, y, w, h): (i32, i32, i32, i32), tab: bool) -> HWND {
    let hinst = GetModuleHandleW(None).unwrap();
    let t = wide(text);
    let style = WS_CHILD.0 | WS_VISIBLE.0 | 0x0B /* BS_OWNERDRAW */ | if tab { WS_TABSTOP.0 } else { 0 };
    CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("BUTTON"),
        PCWSTR(t.as_ptr()),
        WINDOW_STYLE(style),
        x,
        y,
        w,
        h,
        parent,
        HMENU(id as *mut c_void),
        hinst,
        None,
    )
    .unwrap()
}

unsafe fn checkbox(parent: HWND, text: &str, id: usize, (x, y, w, h): (i32, i32, i32, i32), fnt: HFONT, checked: bool) -> HWND {
    let hinst = GetModuleHandleW(None).unwrap();
    let t = wide(text);
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("BUTTON"),
        PCWSTR(t.as_ptr()),
        WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | 0x03 /* BS_AUTOCHECKBOX */),
        x,
        y,
        w,
        h,
        parent,
        HMENU(id as *mut c_void),
        hinst,
        None,
    )
    .unwrap();
    send(hwnd, WM_SETFONT, fnt.0 as usize, 1);
    send(hwnd, BM_SETCHECK_, checked as usize, 0);
    hwnd
}

fn white_brush() -> HBRUSH {
    // クラスの背景ブラシと同じ色。ダイアログの存続中だけ使うので都度作らず static で保持
    use std::sync::OnceLock;
    static B: OnceLock<usize> = OnceLock::new();
    HBRUSH(*B.get_or_init(|| unsafe { CreateSolidBrush(WHITE).0 as usize }) as *mut c_void)
}

// ============================================================================
// 登録 / 編集ダイアログ
// ============================================================================

pub struct EntryDlg {
    pub title: String,
    pub word: String,
    pub tags: String,
    pub meaning: String,
    /// 登録先の候補(書き込み可能な辞書名)。表示は choices.len() > 1 のときだけ
    pub choices: Vec<String>,
    pub choice: usize,
    pub show_choice: bool,
    pub ok: bool,
}

const E_WORD: usize = 301;
const E_TAGS: usize = 302;
const E_MEANING: usize = 303;
const E_COMBO: usize = 304;
const E_ERR: usize = 305;
const E_HINT: usize = 306;

struct EntryCtl {
    data: *mut EntryDlg,
    fonts: Fonts,
    dpi: i32,
    word: HWND,
    tags: HWND,
    meaning: HWND,
    combo: HWND,
    err: HWND,
    fields: Vec<(RECT, HWND)>,
}

fn entry_height(show_choice: bool) -> i32 {
    // 20 + [見出し語] + [タグ] + [辞書] + [意味] + エラー + ボタン
    20 + 66 + 66 + if show_choice { 62 } else { 0 } + (22 + 150 + 12) + 24 + 36 + 20
}

/// 登録・編集ダイアログを表示する。保存されたら true(結果は `data` に入る)
pub unsafe fn entry_dialog(owner: HWND, data: &mut EntryDlg) -> bool {
    data.ok = false;
    let h = entry_height(data.show_choice);
    let title = data.title.clone();
    run_modal(owner, w!("FastDictEntry"), &title, 540, h, data as *mut EntryDlg as *const c_void);
    data.ok
}

unsafe extern "system" fn entry_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ctl = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut EntryCtl;
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let data = cs.lpCreateParams as *mut EntryDlg;
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            let s = |v: i32| scale(v, dpi);
            let fonts = Fonts::new(dpi);
            let d = &*data;
            let (m, fw) = (20, 540 - 40);
            let mut fields = Vec::new();
            let mut y = 20;

            label(hwnd, "見出し語", (s(m), s(y), s(fw), s(20)), fonts.small, 0);
            let word = ui::field(hwnd, E_WORD, (s(m), s(y + 22), s(fw), s(38)), dpi, false, &mut fields, fonts.search);
            y += 66;
            label(hwnd, "タグ(スペースまたはカンマ区切り・任意)", (s(m), s(y), s(fw), s(20)), fonts.small, 0);
            let tags = ui::field(hwnd, E_TAGS, (s(m), s(y + 22), s(fw), s(38)), dpi, false, &mut fields, fonts.base);
            y += 66;

            label(hwnd, "意味", (s(m), s(y), s(fw), s(20)), fonts.small, 0);
            let meaning = ui::field(hwnd, E_MEANING, (s(m), s(y + 22), s(fw), s(150)), dpi, true, &mut fields, fonts.body);
            y += 22 + 150 + 12;

            let hinst = GetModuleHandleW(None).unwrap();
            let combo = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("COMBOBOX"),
                PCWSTR::null(),
                WINDOW_STYLE(WS_CHILD.0 | WS_TABSTOP.0 | WS_VSCROLL.0 | 3 /* CBS_DROPDOWNLIST */),
                s(m),
                s(y + 22),
                s(260),
                s(240),
                hwnd,
                HMENU(E_COMBO as *mut c_void),
                hinst,
                None,
            )
            .unwrap();
            send(combo, WM_SETFONT, fonts.base.0 as usize, 1);
            if d.show_choice {
                label(hwnd, "登録先の辞書", (s(m), s(y), s(fw), s(20)), fonts.small, 0);
                for c in &d.choices {
                    let t = wide(c);
                    send(combo, CB_ADDSTRING, 0, t.as_ptr() as isize);
                }
                send(combo, CB_SETCURSEL, d.choice, 0);
                let _ = ShowWindow(combo, SW_SHOW);
                y += 62;
            }

            let err = label(hwnd, "", (s(m), s(y), s(fw), s(20)), fonts.small, E_ERR);
            y += 24;
            label(hwnd, "Ctrl+Enter でも保存できます", (s(m), s(y + 9), s(220), s(20)), fonts.small, E_HINT);
            button(hwnd, "保存", ID_OK, (s(m + fw - 110 - 12 - 110), s(y), s(110), s(36)), true);
            button(hwnd, "キャンセル", ID_CANCEL, (s(m + fw - 110), s(y), s(110), s(36)), true);

            set_text(word, &d.word);
            set_text(tags, &d.tags);
            set_text(meaning, &d.meaning.replace("\r\n", "\n").replace('\n', "\r\n"));
            let _ = SetWindowTextW(hwnd, PCWSTR(wide(&d.title).as_ptr()));

            let c = Box::new(EntryCtl { data, fonts, dpi, word, tags, meaning, combo, err, fields });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(c) as isize);
            let _ = SetFocus(word);
            send(word, EM_SETSEL, 0, -1);
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            if !ctl.is_null() {
                ui::paint_fields(hdc, &(*ctl).fields, (*ctl).dpi);
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT => {
            let hdc = HDC(wparam.0 as *mut c_void);
            let is_err = !ctl.is_null() && HWND(lparam.0 as *mut c_void) == (*ctl).err;
            let is_hint = GetDlgCtrlID(HWND(lparam.0 as *mut c_void)) == E_HINT as i32;
            let color = if is_err { DANGER } else if is_hint || msg == WM_CTLCOLORSTATIC { MUTED } else { TEXT };
            let color = if msg == WM_CTLCOLOREDIT { TEXT } else { color };
            ui::ctl_color(hdc, color, WHITE, white_brush())
        }
        WM_DRAWITEM => {
            let dis = &*(lparam.0 as *const DRAWITEMSTRUCT);
            if !ctl.is_null() {
                let kind = if dis.CtlID as usize == ID_OK { Btn::Primary } else { Btn::Secondary };
                ui::draw_button(dis, kind, (*ctl).fonts.base, WHITE, (*ctl).dpi);
            }
            LRESULT(1)
        }
        WM_COMMAND => {
            let id = wparam.0 & 0xFFFF;
            let code = wparam.0 >> 16;
            if ctl.is_null() {
                return LRESULT(0);
            }
            let c = &mut *ctl;
            match (id, code) {
                (E_WORD | E_TAGS | E_MEANING, EN_SETFOCUS_ | EN_KILLFOCUS_) => {
                    ui::invalidate_field(hwnd, &c.fields, HWND(lparam.0 as *mut c_void));
                }
                (E_WORD | E_TAGS | E_MEANING, EN_CHANGE_) => {
                    if !ui::is_busy() {
                        set_text(c.err, "");
                    }
                }
                (ID_OK, _) => {
                    let d = &mut *c.data;
                    let word = get_text(c.word).trim().to_string();
                    let meaning = get_text(c.meaning).replace("\r\n", "\n").trim().to_string();
                    if word.is_empty() {
                        set_text(c.err, "見出し語を入力してください");
                        let _ = SetFocus(c.word);
                    } else if meaning.is_empty() {
                        set_text(c.err, "意味を入力してください");
                        let _ = SetFocus(c.meaning);
                    } else {
                        d.word = word;
                        d.meaning = meaning;
                        d.tags = crate::core::normalize_tags(&get_text(c.tags));
                        if d.show_choice {
                            let sel = send(c.combo, CB_GETCURSEL, 0, 0).0;
                            if sel >= 0 {
                                d.choice = sel as usize;
                            }
                        }
                        d.ok = true;
                        let _ = DestroyWindow(hwnd);
                    }
                }
                (ID_CANCEL, _) => {
                    let _ = DestroyWindow(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            if !ctl.is_null() {
                let c = Box::from_raw(ctl);
                c.fonts.destroy();
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// ============================================================================
// 設定ダイアログ
// ============================================================================

pub struct SettingsDlg {
    pub cfg: Config,
    pub ok: bool,
}

const S_HOTKEY: usize = 401;
const S_TRAY: usize = 403;
const S_LIST: usize = 410;
const S_ADD: usize = 411;
const S_DEL: usize = 412;
const S_UP: usize = 413;
const S_DOWN: usize = 414;
const S_NAME: usize = 420;
const S_PATH: usize = 421;
const S_BROWSE: usize = 422;
const S_RO: usize = 423;
const S_ERR: usize = 430;
const S_MUTED: usize = 431;

struct SettingsCtl {
    data: *mut SettingsDlg,
    fonts: Fonts,
    dpi: i32,
    hotkey: HWND,
    tray: HWND,
    list: HWND,
    name: HWND,
    path: HWND,
    ro: HWND,
    err: HWND,
    fields: Vec<(RECT, HWND)>,
    sel: Option<usize>,
    orig_hotkey: Option<Hotkey>,
}

fn to_hk_ctrl(h: &Hotkey) -> isize {
    // 当方: ALT=1 CTRL=2 SHIFT=4 / コントロール: SHIFT=1 CTRL=2 ALT=4(Win は表現不可)
    let mut m = 0;
    if h.mods & config::MOD_SHIFT != 0 {
        m |= 1;
    }
    if h.mods & config::MOD_CONTROL != 0 {
        m |= 2;
    }
    if h.mods & config::MOD_ALT != 0 {
        m |= 4;
    }
    ((m << 8) | h.vk) as isize
}

fn from_hk_ctrl(v: isize) -> (u32, u32) {
    let (vk, m) = ((v & 0xFF) as u32, ((v >> 8) & 0xFF) as u32);
    let mut mods = 0;
    if m & 1 != 0 {
        mods |= config::MOD_SHIFT;
    }
    if m & 2 != 0 {
        mods |= config::MOD_CONTROL;
    }
    if m & 4 != 0 {
        mods |= config::MOD_ALT;
    }
    (mods, vk)
}

/// 設定ダイアログ。保存されたら true(結果は `data` に入る)
pub unsafe fn settings_dialog(owner: HWND, data: &mut SettingsDlg) -> bool {
    data.ok = false;
    run_modal(owner, w!("FastDictSettings"), "設定", 640, 600, data as *mut SettingsDlg as *const c_void);
    data.ok
}

unsafe fn lv_set_row(list: HWND, i: usize, d: &DictConf) {
    let cells = [d.name.clone(), d.path.to_string_lossy().to_string(), if d.readonly { "参照のみ".into() } else { String::new() }];
    for (col, text) in cells.iter().enumerate() {
        let mut t = wide(text);
        if col == 0 {
            let item = LVITEMW { mask: LVIF_TEXT, iItem: i as i32, pszText: PWSTR(t.as_mut_ptr()), ..Default::default() };
            send(list, LVM_INSERTITEMW, 0, &item as *const _ as isize);
        } else {
            let item = LVITEMW { iSubItem: col as i32, pszText: PWSTR(t.as_mut_ptr()), ..Default::default() };
            send(list, LVM_SETITEMTEXTW_, i, &item as *const _ as isize);
        }
    }
}

unsafe fn lv_set_cell(list: HWND, i: usize, col: i32, text: &str) {
    let mut t = wide(text);
    let item = LVITEMW { iSubItem: col, pszText: PWSTR(t.as_mut_ptr()), ..Default::default() };
    send(list, LVM_SETITEMTEXTW_, i, &item as *const _ as isize);
}

unsafe fn reload_list(c: &mut SettingsCtl, select: Option<usize>) {
    let d = &(*c.data).cfg.dicts;
    ui::quiet(|| {
        send(c.list, LVM_DELETEALLITEMS_, 0, 0);
        for (i, dict) in d.iter().enumerate() {
            lv_set_row(c.list, i, dict);
        }
    });
    select_row(c, select);
}

unsafe fn select_row(c: &mut SettingsCtl, row: Option<usize>) {
    ui::quiet(|| {
        if let Some(r) = row {
            let it = LVITEMW {
                stateMask: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
                state: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
                ..Default::default()
            };
            send(c.list, LVM_SETITEMSTATE, r, &it as *const _ as isize);
        }
    });
    load_selected(c, row);
}

unsafe fn load_selected(c: &mut SettingsCtl, row: Option<usize>) {
    c.sel = row;
    let d = &(*c.data).cfg.dicts;
    match row.and_then(|r| d.get(r)) {
        Some(dict) => {
            set_text(c.name, &dict.name);
            set_text(c.path, &dict.path.to_string_lossy());
            send(c.ro, BM_SETCHECK_, dict.readonly as usize, 0);
            for h in [c.name, c.path, c.ro] {
                let _ = EnableWindow(h, true);
            }
        }
        None => {
            set_text(c.name, "");
            set_text(c.path, "");
            send(c.ro, BM_SETCHECK_, 0, 0);
            for h in [c.name, c.path, c.ro] {
                let _ = EnableWindow(h, false);
            }
        }
    }
}

unsafe fn pick_db_path(owner: HWND, initial: &str) -> Option<String> {
    let mut buf = [0u16; 1024];
    for (i, ch) in initial.encode_utf16().take(1000).enumerate() {
        buf[i] = ch;
    }
    let filter: Vec<u16> = "辞書ファイル (*.db)\0*.db\0すべてのファイル\0*.*\0\0".encode_utf16().collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrDefExt: w!("db"),
        lpstrTitle: w!("辞書ファイルを選択(新しいファイル名を入力すると新規作成されます)"),
        Flags: OFN_PATHMUSTEXIST | OFN_HIDEREADONLY,
        ..Default::default()
    };
    if GetSaveFileNameW(&mut ofn).as_bool() {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
        Some(String::from_utf16_lossy(&buf[..len]))
    } else {
        None
    }
}

unsafe extern "system" fn settings_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ctl = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut SettingsCtl;
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let data = cs.lpCreateParams as *mut SettingsDlg;
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            let s = |v: i32| scale(v, dpi);
            let fonts = Fonts::new(dpi);
            let d = &*data;
            let (m, fw) = (24, 640 - 48);
            let mut fields = Vec::new();
            let hinst = GetModuleHandleW(None).unwrap();

            // --- 全般 ---
            let mut y = 20;
            label(hwnd, "全般", (s(m), s(y), s(fw), s(24)), fonts.heading, 0);
            y += 34;
            label(hwnd, "呼び出しホットキー", (s(m), s(y + 5), s(180), s(24)), fonts.base, 0);
            let hotkey = CreateWindowExW(
                WS_EX_CLIENTEDGE,
                w!("msctls_hotkey32"),
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                s(m + 190),
                s(y),
                s(240),
                s(32),
                hwnd,
                HMENU(S_HOTKEY as *mut c_void),
                hinst,
                None,
            )
            .unwrap();
            send(hotkey, WM_SETFONT, fonts.base.0 as usize, 1);
            send(hotkey, HKM_SETRULES, 0x3, 0x6); // 修飾キー無し/Shiftのみは不可(代替: Ctrl+Alt)
            send(hotkey, HKM_SETHOTKEY, d.cfg.hotkey.as_ref().map_or(0, to_hk_ctrl) as usize, 0);
            y += 36;
            label(
                hwnd,
                "枠を選んでキーを押します。BackSpace で無効。Win キーを使う場合は設定ファイルで指定します。",
                (s(m + 190), s(y), s(fw - 190), s(36)),
                fonts.small,
                S_MUTED,
            );
            y += 46;
            let tray = checkbox(hwnd, "閉じるボタンで終了せず、トレイに隠す", S_TRAY, (s(m), s(y), s(fw), s(26)), fonts.base, d.cfg.close_to_tray);
            y += 44;

            // --- 辞書 ---
            label(hwnd, "辞書", (s(m), s(y), s(fw), s(24)), fonts.heading, 0);
            y += 32;
            let list = CreateWindowExW(
                WS_EX_CLIENTEDGE,
                w!("SysListView32"),
                PCWSTR::null(),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | LVS_REPORT as u32 | LVS_SINGLESEL as u32 | LVS_SHOWSELALWAYS as u32),
                s(m),
                s(y),
                s(fw - 112),
                s(150),
                hwnd,
                HMENU(S_LIST as *mut c_void),
                hinst,
                None,
            )
            .unwrap();
            send(list, WM_SETFONT, fonts.base.0 as usize, 1);
            send(list, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize);
            let _ = SetWindowTheme(list, w!("Explorer"), PCWSTR::null());
            for (i, (t, wd)) in [("名前", 120), ("パス", 270), ("参照のみ", 80)].iter().enumerate() {
                let mut tt = wide(t);
                let col = LVCOLUMNW {
                    mask: LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM,
                    cx: s(*wd),
                    pszText: PWSTR(tt.as_mut_ptr()),
                    iSubItem: i as i32,
                    ..Default::default()
                };
                send(list, LVM_INSERTCOLUMNW, i, &col as *const _ as isize);
            }
            let bx = m + fw - 100;
            button(hwnd, "追加…", S_ADD, (s(bx), s(y), s(100), s(34)), true);
            button(hwnd, "削除", S_DEL, (s(bx), s(y + 40), s(100), s(34)), true);
            button(hwnd, "↑ 上へ", S_UP, (s(bx), s(y + 80), s(100), s(34)), true);
            button(hwnd, "↓ 下へ", S_DOWN, (s(bx), s(y + 116), s(100), s(34)), true);
            y += 162;

            label(hwnd, "名前", (s(m), s(y + 8), s(60), s(24)), fonts.base, 0);
            let name = ui::field(hwnd, S_NAME, (s(m + 70), s(y), s(fw - 70), s(38)), dpi, false, &mut fields, fonts.base);
            y += 46;
            label(hwnd, "パス", (s(m), s(y + 8), s(60), s(24)), fonts.base, 0);
            let path = ui::field(hwnd, S_PATH, (s(m + 70), s(y), s(fw - 70 - 110), s(38)), dpi, false, &mut fields, fonts.base);
            button(hwnd, "参照…", S_BROWSE, (s(m + fw - 100), s(y + 1), s(100), s(36)), true);
            y += 46;
            let ro = checkbox(hwnd, "参照のみ(この辞書では登録・編集・削除をしない)", S_RO, (s(m + 70), s(y), s(fw - 70), s(26)), fonts.base, false);
            y += 40;

            let err = label(hwnd, "", (s(m), s(y), s(fw), s(22)), fonts.small, S_ERR);
            y += 26;
            button(hwnd, "保存", ID_OK, (s(m + fw - 110 - 12 - 110), s(y), s(110), s(36)), true);
            button(hwnd, "キャンセル", ID_CANCEL, (s(m + fw - 110), s(y), s(110), s(36)), true);

            let c = Box::new(SettingsCtl {
                data,
                fonts,
                dpi,
                hotkey,
                tray,
                list,
                name,
                path,
                ro,
                err,
                fields,
                sel: None,
                orig_hotkey: d.cfg.hotkey.clone(),
            });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(c) as isize);
            let c = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut SettingsCtl);
            reload_list(c, if d.cfg.dicts.is_empty() { None } else { Some(0) });
            let _ = SetFocus(hotkey);
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            if !ctl.is_null() {
                let c = &*ctl;
                ui::paint_fields(hdc, &c.fields, c.dpi);
                // セクション区切り線
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                let s = |v: i32| scale(v, c.dpi);
                let ly = s(20 + 34 + 36 + 46 + 44 - 9);
                ui::fill(hdc, &RECT { left: s(24), top: ly, right: rc.right - s(24), bottom: ly + 1 }, BORDER);
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT => {
            let hdc = HDC(wparam.0 as *mut c_void);
            let id = GetDlgCtrlID(HWND(lparam.0 as *mut c_void));
            let color = match id as usize {
                S_ERR => DANGER,
                S_MUTED => MUTED,
                _ => TEXT,
            };
            ui::ctl_color(hdc, color, WHITE, white_brush())
        }
        WM_DRAWITEM => {
            let dis = &*(lparam.0 as *const DRAWITEMSTRUCT);
            if !ctl.is_null() {
                let kind = match dis.CtlID as usize {
                    ID_OK => Btn::Primary,
                    S_DEL => Btn::Danger,
                    _ => Btn::Secondary,
                };
                ui::draw_button(dis, kind, (*ctl).fonts.base, WHITE, (*ctl).dpi);
            }
            LRESULT(1)
        }
        WM_NOTIFY => {
            let hdr = &*(lparam.0 as *const NMHDR);
            if !ctl.is_null() && hdr.idFrom == S_LIST && hdr.code == LVN_ITEMCHANGED && !ui::is_busy() {
                let nm = &*(lparam.0 as *const NMLISTVIEW);
                if nm.uNewState & LVIS_SELECTED.0 != 0 {
                    load_selected(&mut *ctl, Some(nm.iItem as usize));
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = wparam.0 & 0xFFFF;
            let code = wparam.0 >> 16;
            if ctl.is_null() {
                return LRESULT(0);
            }
            let c = &mut *ctl;
            let d = &mut *c.data;
            match (id, code) {
                (S_NAME | S_PATH, EN_SETFOCUS_ | EN_KILLFOCUS_) => {
                    ui::invalidate_field(hwnd, &c.fields, HWND(lparam.0 as *mut c_void));
                }
                (S_NAME, EN_CHANGE_) if !ui::is_busy() => {
                    if let Some(r) = c.sel {
                        let t = get_text(c.name);
                        d.cfg.dicts[r].name = t.clone();
                        lv_set_cell(c.list, r, 0, &t);
                    }
                }
                (S_PATH, EN_CHANGE_) if !ui::is_busy() => {
                    if let Some(r) = c.sel {
                        let t = get_text(c.path);
                        d.cfg.dicts[r].path = PathBuf::from(&t);
                        lv_set_cell(c.list, r, 1, &t);
                    }
                }
                (S_RO, _) => {
                    if let Some(r) = c.sel {
                        let on = send(c.ro, BM_GETCHECK_, 0, 0).0 == 1;
                        d.cfg.dicts[r].readonly = on;
                        lv_set_cell(c.list, r, 2, if on { "参照のみ" } else { "" });
                    }
                }
                (S_ADD, _) => {
                    if let Some(p) = pick_db_path(hwnd, "") {
                        let path = PathBuf::from(&p);
                        let name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "新しい辞書".into());
                        d.cfg.dicts.push(DictConf { name, path, readonly: false });
                        let n = d.cfg.dicts.len() - 1;
                        reload_list(c, Some(n));
                        set_text(c.err, "");
                    }
                }
                (S_DEL, _) => {
                    if let Some(r) = c.sel {
                        if d.cfg.dicts.len() <= 1 {
                            set_text(c.err, "辞書は最低1つ必要です");
                        } else {
                            d.cfg.dicts.remove(r);
                            let n = r.min(d.cfg.dicts.len() - 1);
                            reload_list(c, Some(n));
                            set_text(c.err, "(辞書の定義だけを外します。ファイルは削除されません)");
                        }
                    }
                }
                (S_UP | S_DOWN, _) => {
                    if let Some(r) = c.sel {
                        let to = if id == S_UP { r.checked_sub(1) } else { Some(r + 1).filter(|&t| t < d.cfg.dicts.len()) };
                        if let Some(t) = to {
                            d.cfg.dicts.swap(r, t);
                            reload_list(c, Some(t));
                        }
                    }
                }
                (S_BROWSE, _) => {
                    if let Some(r) = c.sel {
                        if let Some(p) = pick_db_path(hwnd, &d.cfg.dicts[r].path.to_string_lossy()) {
                            d.cfg.dicts[r].path = PathBuf::from(&p);
                            set_text(c.path, &p);
                            lv_set_cell(c.list, r, 1, &p);
                        }
                    }
                }
                (ID_OK, _) => {
                    // 検証
                    if d.cfg.dicts.iter().any(|x| x.name.trim().is_empty()) {
                        set_text(c.err, "辞書の名前を入力してください");
                        return LRESULT(0);
                    }
                    if d.cfg.dicts.iter().any(|x| x.path.as_os_str().is_empty()) {
                        set_text(c.err, "辞書のパスを入力してください");
                        return LRESULT(0);
                    }
                    // ホットキー: 変更されていなければ元の値(Win キー等を含む)を保持
                    let v = send(c.hotkey, HKM_GETHOTKEY, 0, 0).0;
                    let orig_v = c.orig_hotkey.as_ref().map_or(0, to_hk_ctrl);
                    if v != orig_v {
                        if v == 0 {
                            d.cfg.hotkey = None;
                        } else {
                            let (mods, vk) = from_hk_ctrl(v);
                            match config::hotkey_from_parts(mods, vk) {
                                Some(h) => d.cfg.hotkey = Some(h),
                                None => {
                                    set_text(c.err, "このキーはホットキーに使えません(英数字・F1〜F24・Space と修飾キーの組み合わせ)");
                                    return LRESULT(0);
                                }
                            }
                        }
                    }
                    d.cfg.close_to_tray = send(c.tray, BM_GETCHECK_, 0, 0).0 == 1;
                    d.ok = true;
                    let _ = DestroyWindow(hwnd);
                }
                (ID_CANCEL, _) => {
                    let _ = DestroyWindow(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            if !ctl.is_null() {
                let c = Box::from_raw(ctl);
                c.fonts.destroy();
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
