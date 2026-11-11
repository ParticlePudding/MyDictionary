//! モーダルダイアログ: 登録/編集、設定。

use crate::config::{self, Config, DictConf, Hotkey};
use crate::{core, import, json_io, safe};
use crate::ui::{self, *};
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Once;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Globalization::{MultiByteToWideChar, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};
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
            (w!("MyDictionaryEntry"), entry_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT),
            (w!("MyDictionarySettings"), settings_proc),
            (w!("MyDictionarySelect"), select_proc),
        ] {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(proc),
                hInstance: hinst.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap(),
                hbrBackground: CreateSolidBrush(WHITE),
                lpszClassName: name,
                hIcon: ui::app_icon(),
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
        if ui::select_all_if_edit(&msg) {
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
    /// 読み(任意)
    pub reading: String,
    pub tags: String,
    pub meaning: String,
    /// 参照先(URL またはファイル/フォルダのパス)
    pub link: String,
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
const E_LINK: usize = 307;
const E_READING: usize = 308;

struct EntryCtl {
    data: *mut EntryDlg,
    fonts: Fonts,
    dpi: i32,
    word: HWND,
    reading: HWND,
    tags: HWND,
    meaning: HWND,
    link: HWND,
    combo: HWND,
    err: HWND,
    fields: Vec<(RECT, HWND)>,
}

fn entry_height(show_choice: bool) -> i32 {
    // 20 + [見出し語] + [タグ] + [辞書] + [意味] + エラー + ボタン
    20 + 66 + 66 + 66 + (22 + 150 + 12) + (22 + 76 + 12) + if show_choice { 62 } else { 0 } + 24 + 36 + 20
}

/// 登録・編集ダイアログを表示する。保存されたら true(結果は `data` に入る)
pub unsafe fn entry_dialog(owner: HWND, data: &mut EntryDlg) -> bool {
    data.ok = false;
    let h = entry_height(data.show_choice);
    let title = data.title.clone();
    run_modal(owner, w!("MyDictionaryEntry"), &title, 540, h, data as *mut EntryDlg as *const c_void);
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
            let word = ui::field(hwnd, E_WORD, (s(m), s(y + 22), s(fw), s(CTL_H)), dpi, false, &mut fields, fonts.base);
            y += 66;
            label(hwnd, "読み(任意。ひらがな・カタカナなど。読みでも検索でき、見出し語順の並びにも使われます)", (s(m), s(y), s(fw), s(20)), fonts.small, 0);
            let reading = ui::field(hwnd, E_READING, (s(m), s(y + 22), s(fw), s(CTL_H)), dpi, false, &mut fields, fonts.base);
            y += 66;
            label(hwnd, "タグ(スペースまたはカンマ区切り・任意)", (s(m), s(y), s(fw), s(20)), fonts.small, 0);
            let tags = ui::field(hwnd, E_TAGS, (s(m), s(y + 22), s(fw), s(CTL_H)), dpi, false, &mut fields, fonts.base);
            y += 66;

            label(hwnd, "意味", (s(m), s(y), s(fw), s(20)), fonts.small, 0);
            let meaning = ui::field(hwnd, E_MEANING, (s(m), s(y + 22), s(fw), s(150)), dpi, true, &mut fields, fonts.base);
            y += 22 + 150 + 12;
            label(hwnd, "参照先(任意。1行に1つ。「表示名 | URL」と書くと表示名を付けられます)", (s(m), s(y), s(fw), s(20)), fonts.small, 0);
            let link = ui::field(hwnd, E_LINK, (s(m), s(y + 22), s(fw), s(76)), dpi, true, &mut fields, fonts.base);
            y += 22 + 76 + 12;

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
            button(hwnd, "保存", ID_OK, (s(m + fw - 2 * BTN_W - 10), s(y), s(BTN_W), s(CTL_H)), true);
            button(hwnd, "キャンセル", ID_CANCEL, (s(m + fw - BTN_W), s(y), s(BTN_W), s(CTL_H)), true);

            set_text(word, &d.word);
            set_text(reading, &d.reading);
            set_text(link, &d.link.replace("\r\n", "\n").replace('\n', "\r\n"));
            set_text(tags, &d.tags);
            set_text(meaning, &d.meaning.replace("\r\n", "\n").replace('\n', "\r\n"));
            let _ = SetWindowTextW(hwnd, PCWSTR(wide(&d.title).as_ptr()));

            let c = Box::new(EntryCtl { data, fonts, dpi, word, reading, tags, meaning, link, combo, err, fields });
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
                (E_WORD | E_READING | E_TAGS | E_MEANING | E_LINK, EN_SETFOCUS_ | EN_KILLFOCUS_) => {
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
                        d.reading = get_text(c.reading).trim().to_string();
                        d.meaning = meaning;
                        d.tags = core::normalize_tags(&get_text(c.tags));
                        d.link = get_text(c.link).replace("\r\n", "\n").lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
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
        WM_NEXTDLGCTL => {
            ui::next_dlg_ctl(hwnd, wparam, lparam);
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
    /// 書き込み前バックアップの置き場(インポートで使う)
    pub backup_root: PathBuf,
    /// 設定ファイルの場所(画面に表示する)
    pub cfg_path: PathBuf,
    /// インポートを実行した(キャンセルで閉じても、辞書の読み直しが必要)
    pub imported: bool,
    pub ok: bool,
}

const S_HOTKEY: usize = 401;
const S_TRAY: usize = 403;
const S_TITLE: usize = 405;
const S_SHARED: usize = 406;
const S_LIST: usize = 410;
const S_ADD: usize = 411;
const S_DEL: usize = 412;
const S_UP: usize = 413;
const S_DOWN: usize = 414;
const S_NAME: usize = 420;
const S_PATH: usize = 421;
const S_BROWSE: usize = 422;
const S_RO: usize = 423;
const S_IMPORT: usize = 424;
const S_EXPORT: usize = 425;
const S_ERR: usize = 430;
const S_MUTED: usize = 431;

struct SettingsCtl {
    data: *mut SettingsDlg,
    fonts: Fonts,
    dpi: i32,
    hotkey: HWND,
    tray: HWND,
    title: HWND,
    shared: HWND,
    section_line: i32,
    list: HWND,
    name: HWND,
    path: HWND,
    ro: HWND,
    err: HWND,
    fields: Vec<(RECT, HWND)>,
    sel: Option<usize>,
    orig_hotkey: Option<Hotkey>,
    /// 保存/キャンセルの上に引く区切り線の位置
    footer_line: i32,
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
    run_modal(owner, w!("MyDictionarySettings"), "設定", 640, 680, data as *mut SettingsDlg as *const c_void);
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
            label(hwnd, "呼び出しホットキー", (s(m), s(y + 8), s(180), s(24)), fonts.base, 0);
            let hotkey = CreateWindowExW(
                WS_EX_CLIENTEDGE,
                w!("msctls_hotkey32"),
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                s(m + 190),
                s(y),
                s(240),
                s(CTL_H),
                hwnd,
                HMENU(S_HOTKEY as *mut c_void),
                hinst,
                None,
            )
            .unwrap();
            send(hotkey, WM_SETFONT, fonts.base.0 as usize, 1);
            send(hotkey, HKM_SETRULES, 0x3, 0x6); // 修飾キー無し/Shiftのみは不可(代替: Ctrl+Alt)
            send(hotkey, HKM_SETHOTKEY, d.cfg.hotkey.as_ref().map_or(0, to_hk_ctrl) as usize, 0);
            y += 40;
            label(
                hwnd,
                "枠を選んでキーを押します。BackSpace で無効。Win キーを使う場合は設定ファイルで指定します。",
                (s(m + 190), s(y), s(fw - 190), s(36)),
                fonts.small,
                S_MUTED,
            );
            y += 46;
label(hwnd, "ウィンドウに表示する名前", (s(m), s(y + 8), s(180), s(24)), fonts.base, 0);
            let title = ui::field(hwnd, S_TITLE, (s(m + 190), s(y), s(fw - 190), s(CTL_H)), dpi, false, &mut fields, fonts.base);
            let cue = wide("例: 社内用語集(空なら MyDictionary と表示します)");
            send(title, EM_SETCUEBANNER, 1, cue.as_ptr() as isize);
            set_text(title, &d.cfg.title);
            y += 46;
            let tray = checkbox(hwnd, "閉じるボタンや Esc で終了せず、トレイに隠して常駐する", S_TRAY, (s(m), s(y), s(fw), s(26)), fonts.base, d.cfg.close_to_tray);
            y += 30;
            let shared = checkbox(hwnd, "共用フォルダで運用する(この設定を複数人で使う。しおりは無効、並び順は保存しない)", S_SHARED, (s(m), s(y), s(fw), s(26)), fonts.base, d.cfg.shared);
            y += 34;
            let section_line = s(y);
            y += 10;

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
                s(fw - BTN_W - 12),
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
            for (i, (t, wd)) in [("名前", 120), ("パス", 250), ("参照のみ", 76)].iter().enumerate() {
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
            let bx = m + fw - BTN_W;
            button(hwnd, "追加…", S_ADD, (s(bx), s(y), s(BTN_W), s(CTL_H)), true);
            button(hwnd, "削除", S_DEL, (s(bx), s(y + 38), s(BTN_W), s(CTL_H)), true);
            button(hwnd, "↑ 上へ", S_UP, (s(bx), s(y + 76), s(BTN_W), s(CTL_H)), true);
            button(hwnd, "↓ 下へ", S_DOWN, (s(bx), s(y + 114), s(BTN_W), s(CTL_H)), true);
            y += 162;

            label(hwnd, "名前", (s(m), s(y + 8), s(60), s(24)), fonts.base, 0);
            let name = ui::field(hwnd, S_NAME, (s(m + 70), s(y), s(fw - 70), s(CTL_H)), dpi, false, &mut fields, fonts.base);
            y += 46;
            label(hwnd, "パス", (s(m), s(y + 8), s(60), s(24)), fonts.base, 0);
            let path = ui::field(hwnd, S_PATH, (s(m + 70), s(y), s(fw - 70 - BTN_W - 12), s(CTL_H)), dpi, false, &mut fields, fonts.base);
            button(hwnd, "参照…", S_BROWSE, (s(m + fw - BTN_W), s(y), s(BTN_W), s(CTL_H)), true);
            y += 46;
            let ro = checkbox(hwnd, "参照のみ(登録・編集・削除をしない)", S_RO, (s(m + 70), s(y + 5), s(fw - 70 - 2 * BTN_W - 24), s(26)), fonts.base, false);
            button(hwnd, "取り込む…", S_IMPORT, (s(m + fw - 2 * BTN_W - 12), s(y), s(BTN_W), s(CTL_H)), true);
            button(hwnd, "書き出す…", S_EXPORT, (s(m + fw - BTN_W), s(y), s(BTN_W), s(CTL_H)), true);
            y += 46;

            let err = label(hwnd, "", (s(m), s(y), s(fw), s(22)), fonts.small, S_ERR);
            y += 28;
            // ここから下は、画面全体(全般・辞書の両方)の保存/キャンセル。区切り線で分ける
            let footer_line = s(y);
            y += 16;
            // 左下に、アプリの名前と版
            // 左下に、アプリの版と、いま使っている設定ファイルの場所(個人の設定か、共用の設定かが分かるように)
            let info = format!("{} {}  設定: {}", ui::APP_NAME, ui::APP_VERSION, d.cfg_path.display());
            let hinst2 = GetModuleHandleW(None).unwrap();
            let t = wide(&info);
            let info_h = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                PCWSTR(t.as_ptr()),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | 0x8000 /* SS_PATHELLIPSIS */ | 0x80 /* SS_NOPREFIX */),
                s(m),
                s(y + 9),
                s(fw - 2 * BTN_W - 30),
                s(20),
                hwnd,
                HMENU(S_MUTED as *mut c_void),
                hinst2,
                None,
            )
            .unwrap();
            send(info_h, WM_SETFONT, fonts.small.0 as usize, 1);
            button(hwnd, "保存", ID_OK, (s(m + fw - 2 * BTN_W - 10), s(y), s(BTN_W), s(CTL_H)), true);
            button(hwnd, "キャンセル", ID_CANCEL, (s(m + fw - BTN_W), s(y), s(BTN_W), s(CTL_H)), true);

            let c = Box::new(SettingsCtl {
                data,
                fonts,
                dpi,
                hotkey,
                tray,
                title,
                shared,
                section_line,
                list,
                name,
                path,
                ro,
                err,
                fields,
                sel: None,
                orig_hotkey: d.cfg.hotkey.clone(),
                footer_line,
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
                ui::fill(hdc, &RECT { left: s(24), top: c.section_line, right: rc.right - s(24), bottom: c.section_line + 1 }, BORDER);
                ui::fill(hdc, &RECT { left: 0, top: c.footer_line, right: rc.right, bottom: c.footer_line + 1 }, BORDER);
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
                (S_NAME | S_PATH | S_TITLE, EN_SETFOCUS_ | EN_KILLFOCUS_) => {
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
                (S_IMPORT, _) => {
                    if let Some(r) = c.sel {
                        let dict = d.cfg.dicts[r].clone();
                        if dict.readonly {
                            set_text(c.err, "参照のみの辞書には取り込めません");
                        } else if dict.path.as_os_str().is_empty() {
                            set_text(c.err, "先に辞書のパスを指定してください");
                        } else if import_into(hwnd, &dict, &d.backup_root) {
                            d.imported = true;
                        }
                    }
                }
                (S_EXPORT, _) => {
                    if let Some(r) = c.sel {
                        let dict = d.cfg.dicts[r].clone();
                        if dict.path.as_os_str().is_empty() {
                            set_text(c.err, "先に辞書のパスを指定してください");
                        } else {
                            export_dict(hwnd, &dict);
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
                    d.cfg.shared = send(c.shared, BM_GETCHECK_, 0, 0).0 == 1;
                    d.cfg.title = get_text(c.title).trim().to_string();
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
        WM_NEXTDLGCTL => {
            ui::next_dlg_ctl(hwnd, wparam, lparam);
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
// CSV / TSV のインポート
// ============================================================================

unsafe fn msg_box(owner: HWND, text: &str, caption: &str, style: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    let (t, c) = (wide(text), wide(caption));
    MessageBoxW(owner, PCWSTR(t.as_ptr()), PCWSTR(c.as_ptr()), style)
}

unsafe fn pick_import_file(owner: HWND) -> Option<String> {
    let mut buf = [0u16; 1024];
    let filter: Vec<u16> = "JSON / CSV / TSV (*.json;*.csv;*.tsv;*.txt)\0*.json;*.csv;*.tsv;*.tab;*.txt\0すべてのファイル\0*.*\0\0".encode_utf16().collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrTitle: w!("取り込むファイルを選択(JSON / CSV / TSV)"),
        Flags: OFN_PATHMUSTEXIST | OFN_FILEMUSTEXIST | OFN_HIDEREADONLY,
        ..Default::default()
    };
    if GetOpenFileNameW(&mut ofn).as_bool() {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
        Some(String::from_utf16_lossy(&buf[..len]))
    } else {
        None
    }
}

/// UTF-8 / UTF-16 でなければ、日本語版 Excel の既定である Shift_JIS(CP932)として読む
unsafe fn decode_text(bytes: &[u8]) -> Option<String> {
    if let Some(s) = import::decode_unicode(bytes) {
        return Some(s);
    }
    let flags = MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0x8); // MB_ERR_INVALID_CHARS
    let n = MultiByteToWideChar(932, flags, bytes, None);
    if n <= 0 {
        return None;
    }
    let mut buf = vec![0u16; n as usize];
    let n = MultiByteToWideChar(932, flags, bytes, Some(&mut buf));
    (n > 0).then(|| String::from_utf16_lossy(&buf[..n as usize]))
}

/// ファイルを選び、確認のうえ辞書へ取り込む。取り込んだら true
unsafe fn import_into(owner: HWND, dict: &DictConf, backup_root: &std::path::Path) -> bool {
    let Some(file) = pick_import_file(owner) else { return false };
    let bytes = match std::fs::read(&file) {
        Ok(b) => b,
        Err(e) => {
            msg_box(owner, &format!("ファイルを読み込めません: {e}"), "インポート", MB_OK | MB_ICONERROR);
            return false;
        }
    };
    let Some(text) = decode_text(&bytes) else {
        msg_box(owner, "文字コードを判別できません。UTF-8 または Shift_JIS で保存したファイルを指定してください。", "インポート", MB_OK | MB_ICONERROR);
        return false;
    };
    // 拡張子が .json、または中身が [ か { で始まっていれば JSON として読む
    let head = text.trim_start_matches('\u{feff}').trim_start();
    let is_json = file.to_lowercase().ends_with(".json") || head.starts_with('[') || head.starts_with('{');
    let (rows, skipped) = if is_json {
        match json_io::parse(&text) {
            Ok(r) => r,
            Err(e) => {
                msg_box(owner, &format!("取り込めません。\n\n{e}"), "インポート", MB_OK | MB_ICONERROR);
                return false;
            }
        }
    } else {
        import::to_rows(import::parse_table(&text, import::sniff_delim(&text, &file)))
    };
    if rows.is_empty() {
        msg_box(
            owner,
            "取り込める語がありません。\n\nCSV / TSV: 1行目に「見出し語, 意味, タグ, 参照先, 読み」の列名を書くか、列名なしでこの順に並べてください(見出し語と意味以外は省略できます)。\nJSON: 「書き出す…」で出力される形式、または word と meaning を持つオブジェクトの配列。",
            "インポート",
            MB_OK | MB_ICONWARNING,
        );
        return false;
    }
    let mut preview = String::new();
    for r in rows.iter().take(3) {
        let m: String = r.meaning.chars().take(30).map(|c| if c == '\n' { ' ' } else { c }).collect();
        preview.push_str(&format!("  ・{} — {}\n", r.word, m));
    }
    let skip_note = if skipped > 0 { format!("\n(見出し語か意味が空の {skipped} 行は取り込みません)") } else { String::new() };
    let ask = format!(
        "{} 件を、辞書「{}」に取り込みます。{}\n\n{}{}\nすでに同じ見出し語と意味の語がある場合は、重複として飛ばします。\n取り込みますか?",
        rows.len(),
        dict.name,
        skip_note,
        preview,
        if rows.len() > 3 { "  …\n" } else { "" }
    );
    if msg_box(owner, &ask, "インポート", MB_YESNO | MB_ICONQUESTION) != IDYES {
        return false;
    }
    let old = SetCursor(LoadCursorW(None, IDC_WAIT).unwrap());
    let r = safe::write(&dict.path, backup_root, |c| import::insert_rows(c, &rows));
    SetCursor(old);
    match r {
        Ok((added, dup)) => {
            let dup_note = if dup > 0 { format!("\n重複のため飛ばした語: {dup} 件") } else { String::new() };
            msg_box(owner, &format!("{added} 件を取り込みました。{dup_note}"), "インポート", MB_OK | MB_ICONINFORMATION);
            true
        }
        Err(e) => {
            msg_box(owner, &format!("取り込めませんでした。辞書は変更されていません。\n\n{e}"), "インポート", MB_OK | MB_ICONERROR);
            false
        }
    }
}

// ============================================================================
// 検索に含める辞書の選択
// ============================================================================

pub struct SelectDlg {
    pub names: Vec<String>,
    pub checked: Vec<bool>,
    pub ok: bool,
}

const D_LIST: usize = 501;
const D_ALL: usize = 502;
const D_NONE: usize = 503;
const D_ERR: usize = 504;
const LVM_GETITEMSTATE_: u32 = 0x102C;

struct SelectCtl {
    data: *mut SelectDlg,
    fonts: Fonts,
    dpi: i32,
    list: HWND,
    err: HWND,
}

/// 検索に含める辞書を選ぶ。OK なら true(結果は `data.checked`)
pub unsafe fn select_dialog(owner: HWND, data: &mut SelectDlg) -> bool {
    data.ok = false;
    run_modal(owner, w!("MyDictionarySelect"), "検索する辞書", 420, 400, data as *mut SelectDlg as *const c_void);
    data.ok
}

unsafe fn set_check(list: HWND, i: usize, on: bool) {
    // 状態イメージ 1 = 未チェック、2 = チェック済み
    let it = LVITEMW {
        stateMask: LIST_VIEW_ITEM_STATE_FLAGS(0xF000),
        state: LIST_VIEW_ITEM_STATE_FLAGS(if on { 0x2000 } else { 0x1000 }),
        ..Default::default()
    };
    send(list, LVM_SETITEMSTATE, i, &it as *const _ as isize);
}

unsafe fn get_check(list: HWND, i: usize) -> bool {
    (send(list, LVM_GETITEMSTATE_, i, 0xF000).0 as u32 >> 12) == 2
}

unsafe extern "system" fn select_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ctl = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut SelectCtl;
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let data = cs.lpCreateParams as *mut SelectDlg;
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            let s = |v: i32| scale(v, dpi);
            let fonts = Fonts::new(dpi);
            let d = &*data;
            let (m, fw) = (20, 420 - 40);
            let hinst = GetModuleHandleW(None).unwrap();
            let mut y = 18;
            label(hwnd, "検索に含める辞書にチェックを入れてください", (s(m), s(y), s(fw), s(22)), fonts.base, 0);
            y += 30;
            let list = CreateWindowExW(
                WS_EX_CLIENTEDGE,
                w!("SysListView32"),
                PCWSTR::null(),
                WINDOW_STYLE(
                    WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | LVS_REPORT as u32 | LVS_SINGLESEL as u32 | LVS_SHOWSELALWAYS as u32 | LVS_NOCOLUMNHEADER as u32,
                ),
                s(m),
                s(y),
                s(fw),
                s(220),
                hwnd,
                HMENU(D_LIST as *mut c_void),
                hinst,
                None,
            )
            .unwrap();
            send(list, WM_SETFONT, fonts.base.0 as usize, 1);
            send(list, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, (LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize);
            let _ = SetWindowTheme(list, w!("Explorer"), PCWSTR::null());
            let col = LVCOLUMNW { mask: LVCF_WIDTH, cx: s(fw - 30), ..Default::default() };
            send(list, LVM_INSERTCOLUMNW, 0, &col as *const _ as isize);
            for (i, name) in d.names.iter().enumerate() {
                let mut t = wide(name);
                let item = LVITEMW { mask: LVIF_TEXT, iItem: i as i32, pszText: PWSTR(t.as_mut_ptr()), ..Default::default() };
                send(list, LVM_INSERTITEMW, 0, &item as *const _ as isize);
                set_check(list, i, d.checked.get(i).copied().unwrap_or(true));
            }
            y += 230;
            button(hwnd, "すべて選択", D_ALL, (s(m), s(y), s(BTN_W), s(CTL_H)), true);
            button(hwnd, "すべて解除", D_NONE, (s(m + BTN_W + 10), s(y), s(BTN_W), s(CTL_H)), true);
            y += 44;
            let err = label(hwnd, "", (s(m), s(y), s(fw), s(20)), fonts.small, D_ERR);
            y += 24;
            button(hwnd, "OK", ID_OK, (s(m + fw - 2 * BTN_W - 10), s(y), s(BTN_W), s(CTL_H)), true);
            button(hwnd, "キャンセル", ID_CANCEL, (s(m + fw - BTN_W), s(y), s(BTN_W), s(CTL_H)), true);

            let c = Box::new(SelectCtl { data, fonts, dpi, list, err });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(c) as isize);
            let _ = SetFocus(list);
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let hdc = HDC(wparam.0 as *mut c_void);
            let color = if GetDlgCtrlID(HWND(lparam.0 as *mut c_void)) as usize == D_ERR { DANGER } else { TEXT };
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
            if ctl.is_null() {
                return LRESULT(0);
            }
            let c = &mut *ctl;
            let d = &mut *c.data;
            match wparam.0 & 0xFFFF {
                D_ALL | D_NONE => {
                    for i in 0..d.names.len() {
                        set_check(c.list, i, wparam.0 & 0xFFFF == D_ALL);
                    }
                    set_text(c.err, "");
                }
                ID_OK => {
                    let checked: Vec<bool> = (0..d.names.len()).map(|i| get_check(c.list, i)).collect();
                    if checked.iter().any(|&b| b) {
                        d.checked = checked;
                        d.ok = true;
                        let _ = DestroyWindow(hwnd);
                    } else {
                        set_text(c.err, "辞書を1つ以上選んでください");
                    }
                }
                ID_CANCEL => {
                    let _ = DestroyWindow(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_NEXTDLGCTL => {
            ui::next_dlg_ctl(hwnd, wparam, lparam);
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
// JSON へのエクスポート
// ============================================================================

unsafe fn pick_export_file(owner: HWND, default_name: &str) -> Option<String> {
    let mut buf = [0u16; 1024];
    for (i, ch) in default_name.encode_utf16().take(200).enumerate() {
        buf[i] = ch;
    }
    let filter: Vec<u16> = "JSON (*.json)\0*.json\0すべてのファイル\0*.*\0\0".encode_utf16().collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrDefExt: w!("json"),
        lpstrTitle: w!("書き出し先を指定"),
        Flags: OFN_PATHMUSTEXIST | OFN_HIDEREADONLY | OFN_OVERWRITEPROMPT,
        ..Default::default()
    };
    if GetSaveFileNameW(&mut ofn).as_bool() {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
        Some(String::from_utf16_lossy(&buf[..len]))
    } else {
        None
    }
}

/// 辞書の全語を JSON ファイルに書き出す(辞書ファイルの現在の内容を読む)
unsafe fn export_dict(owner: HWND, dict: &DictConf) {
    // ファイル名に使えない文字は置き換える
    let safe_name: String = dict.name.chars().map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c }).collect();
    let Some(file) = pick_export_file(owner, &format!("{safe_name}.json")) else { return };
    let old = SetCursor(LoadCursorW(None, IDC_WAIT).unwrap());
    let loaded = core::open_ro(&dict.path).and_then(|c| core::load_all(&c));
    let result = match loaded {
        Ok(mut entries) => {
            core::sort_entries(&mut entries);
            std::fs::write(&file, json_io::export(&entries)).map(|_| entries.len()).map_err(|e| e.to_string())
        }
        Err(e) => Err(format!("辞書を読み込めません: {e}")),
    };
    SetCursor(old);
    match result {
        Ok(n) => {
            msg_box(owner, &format!("辞書「{}」の {n} 語を書き出しました。\n\n{file}\n\nこのファイルは「取り込む…」でそのまま取り込めます。", dict.name), "エクスポート", MB_OK | MB_ICONINFORMATION);
        }
        Err(e) => {
            msg_box(owner, &format!("書き出せませんでした。\n\n{e}"), "エクスポート", MB_OK | MB_ICONERROR);
        }
    }
}
