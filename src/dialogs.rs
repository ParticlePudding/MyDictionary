//! モーダルダイアログ: 登録/編集、設定、ライセンス情報、タグの一覧。

use crate::config::{self, Config, DictConf, Hotkey};
use crate::undo::Undo;
use crate::{core, import, json_io, license, pics, safe};
use crate::ui::{self, *};
use std::collections::HashMap;
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
/// 既定のボタンをたずねるメッセージ(DM_GETDEFID)と、その答えの印(DC_HASDEFID)。
/// 答えておくと、1行の入力欄で Enter を押したときに、そのボタンが押されたことになる
const DM_GETDEFID_: u32 = 0x0400;
const DC_HASDEFID_: isize = 0x534B;

static REGISTER: Once = Once::new();

unsafe fn register_classes() {
    REGISTER.call_once(|| {
        let hinst = GetModuleHandleW(None).unwrap();
        for (name, proc) in [
            (w!("MyDictionaryEntry"), entry_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT),
            (w!("MyDictionarySettings"), settings_proc),
            (w!("MyDictionarySelect"), select_proc),
            (w!("MyDictionaryLicense"), license_proc),
            (w!("MyDictionaryTags"), tags_proc),
            (w!("MyDictionaryPrompt"), prompt_proc),
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
        // Esc = 閉じる。複数行の欄は Esc を自分で受け取ってしまうため、読み取り専用の欄(ライセンス情報の本文)では補う
        if msg.message == WM_KEYDOWN && msg.wParam.0 == VK_ESCAPE.0 as usize && is_readonly_multiline(msg.hwnd) {
            let _ = PostMessageW(hwnd, WM_COMMAND, WPARAM(ID_CANCEL), LPARAM(0));
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

/// 読み取り専用の複数行 EDIT か
unsafe fn is_readonly_multiline(h: HWND) -> bool {
    let mut cls = [0u16; 16];
    let n = GetClassNameW(h, &mut cls) as usize;
    if !String::from_utf16_lossy(&cls[..n]).eq_ignore_ascii_case("edit") {
        return false;
    }
    let style = GetWindowLongW(h, GWL_STYLE) as u32;
    let need = ES_MULTILINE as u32 | ES_READONLY as u32;
    style & need == need
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
    /// 同じ見出し語の語を調べるための索引: そろえた見出し語(`core::fold`)→ (辞書の添字, id)。編集中の語自身は含めない
    pub same_words: HashMap<String, Vec<(usize, i64)>>,
    /// 辞書の名前(`same_words` の辞書の添字で引く)
    pub dict_names: Vec<String>,
    /// 登録先の候補(`choices` と同じ並び)の、辞書の添字。編集のときは、その語の辞書1つ
    pub targets: Vec<usize>,
    /// 編集前の見出し語(新規なら空)。見出し語を変えていないときは、保存時の確認を出さない
    pub orig_word: String,
    /// 「その語を表示」が押された: 表示する語 (辞書の添字, id)。このとき `ok` は false
    pub open_existing: Option<(usize, i64)>,
    /// すでに使われているタグと語数(「選ぶ…」で一覧から選べるようにする)
    pub all_tags: Vec<(String, usize)>,
    /// 辞書ファイルのパス(辞書の添字で引く)。「画像を追加…」で、画像をコピーする先を決める
    pub dict_paths: Vec<PathBuf>,
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
const E_DUP: usize = 309; // 同じ見出し語があるときの知らせ
const E_DUPLINK: usize = 310; // 「その語を表示」
const E_TAGPICK: usize = 311; // タグを一覧から選ぶ
const E_ADDPIC: usize = 312; // 「画像を追加…」

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
    dup: HWND,
    dup_link: HWND,
    fields: Vec<(RECT, HWND)>,
    /// 「画像を追加…」で選んだ画像: (元のファイル, 意味の欄に書いたパス)。コピーは、保存を押したときに行う
    pending_pics: Vec<(PathBuf, String)>,
}

/// 見出し語の欄の下に出す、重複の知らせの行の高さ
const DUP_H: i32 = 18;

fn entry_height(show_choice: bool) -> i32 {
    // 20 + [見出し語 + 重複の知らせ] + [読み] + [タグ] + [意味] + [参照先] + [辞書] + エラー + ボタン
    20 + 66 + DUP_H + 66 + 66 + (22 + 150 + 12) + (22 + 76 + 12) + if show_choice { 62 } else { 0 } + 24 + 36 + 20
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
            // 同じ見出し語の語があるときの知らせと、その語へ移るリンク(欄のすぐ下。無いときは空)
            let hinst = GetModuleHandleW(None).unwrap();
            let dup = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                PCWSTR::null(),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | 0x4000 /* SS_ENDELLIPSIS */ | 0x80 /* SS_NOPREFIX */),
                s(m),
                s(y + 22 + CTL_H + 4),
                s(fw - 124),
                s(DUP_H),
                hwnd,
                HMENU(E_DUP as *mut c_void),
                hinst,
                None,
            )
            .unwrap();
            send(dup, WM_SETFONT, fonts.small.0 as usize, 1);
            let link_t = wide("<a>その語を表示</a>");
            let dup_link = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("SysLink"),
                PCWSTR(link_t.as_ptr()),
                WINDOW_STYLE(WS_CHILD.0 | WS_TABSTOP.0),
                s(m + fw - 120),
                s(y + 22 + CTL_H + 4),
                s(120),
                s(DUP_H),
                hwnd,
                HMENU(E_DUPLINK as *mut c_void),
                hinst,
                None,
            )
            .unwrap_or_default();
            send(dup_link, WM_SETFONT, fonts.small.0 as usize, 1);
            // 文字の幅に合わせて、欄の右端にそろえる
            let mut ideal = SIZE::default();
            send(dup_link, LM_GETIDEALSIZE, s(120) as usize, &mut ideal as *mut _ as isize);
            if ideal.cx > 0 && ideal.cx <= s(120) {
                let _ = MoveWindow(dup_link, s(m + fw) - ideal.cx, s(y + 22 + CTL_H + 4), ideal.cx, s(DUP_H), false);
            }
            y += 66 + DUP_H;
            label(hwnd, "読み(任意。ひらがな・カタカナなど。読みでも検索でき、見出し語順の並びにも使われます)", (s(m), s(y), s(fw), s(20)), fonts.small, 0);
            let reading = ui::field(hwnd, E_READING, (s(m), s(y + 22), s(fw), s(CTL_H)), dpi, false, &mut fields, fonts.base);
            y += 66;
            label(hwnd, "タグ(スペースまたはカンマ区切り・任意)", (s(m), s(y), s(fw), s(20)), fonts.small, 0);
            // タグの欄の右に、すでにあるタグから選ぶボタン
            let pick_w = 84;
            let tags = ui::field(hwnd, E_TAGS, (s(m), s(y + 22), s(fw - pick_w - 8), s(CTL_H)), dpi, false, &mut fields, fonts.base);
            button(hwnd, "選ぶ…", E_TAGPICK, (s(m + fw - pick_w), s(y + 22), s(pick_w), s(CTL_H)), true);
            y += 66;

            label(hwnd, "意味(リンクは [表示名](開く先)、画像は ![説明](パス) と書けます)", (s(m), s(y), s(fw - 130), s(20)), fonts.small, 0);
            // 画像を選んで、意味の欄に書き方を入れる(画像のコピーは、保存のとき)
            let pic_t = wide("<a>画像を追加…</a>");
            let add_pic = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("SysLink"),
                PCWSTR(pic_t.as_ptr()),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                s(m + fw - 124),
                s(y),
                s(124),
                s(20),
                hwnd,
                HMENU(E_ADDPIC as *mut c_void),
                hinst,
                None,
            )
            .unwrap_or_default();
            send(add_pic, WM_SETFONT, fonts.small.0 as usize, 1);
            let mut ideal = SIZE::default();
            send(add_pic, LM_GETIDEALSIZE, s(124) as usize, &mut ideal as *mut _ as isize);
            if ideal.cx > 0 && ideal.cx <= s(124) {
                let _ = MoveWindow(add_pic, s(m + fw) - ideal.cx, s(y), ideal.cx, s(20), false);
            }
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

            let c = Box::new(EntryCtl { data, fonts, dpi, word, reading, tags, meaning, link, combo, err, dup, dup_link, fields, pending_pics: Vec::new() });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(c) as isize);
            update_dup(&*(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const EntryCtl));
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
            let id = GetDlgCtrlID(HWND(lparam.0 as *mut c_void)) as usize;
            let is_hint = id == E_HINT;
            let color = if is_err {
                DANGER
            } else if id == E_DUP {
                WARN
            } else if is_hint || msg == WM_CTLCOLORSTATIC {
                MUTED
            } else {
                TEXT
            };
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
                        if id == E_WORD {
                            update_dup(c);
                        }
                    }
                }
                (E_TAGPICK, _) => {
                    // すでにあるタグの一覧から選ぶ。欄に書いてある、一覧に無いタグ(新しいタグ)は残す
                    let d = &*c.data;
                    let current: Vec<String> = core::normalize_tags(&get_text(c.tags)).split(' ').filter(|t| !t.is_empty()).map(str::to_owned).collect();
                    let mut dlg = TagsDlg {
                        title: "タグを選ぶ".into(),
                        note: "この語に付けるタグに、チェックを入れてください。\n新しいタグは、タグの欄に直接入力します。".into(),
                        tags: d.all_tags.clone(),
                        checked: current.clone(),
                        manage: false,
                        action: None,
                        ok: false,
                    };
                    if tags_dialog(hwnd, &mut dlg) {
                        let known = |t: &String| d.all_tags.iter().any(|(k, _)| k == t);
                        let mut out: Vec<String> = current.iter().filter(|t| !known(t) || dlg.checked.contains(t)).cloned().collect();
                        out.extend(dlg.checked.iter().filter(|t| !current.contains(t)).cloned());
                        set_text(c.tags, &out.join(" "));
                    }
                    let _ = SetFocus(c.tags);
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
                    } else if !confirm_same_word(hwnd, c, &word) {
                        let _ = SetFocus(c.word);
                    } else {
                        // 「画像を追加…」で選んだ画像を、辞書の画像のフォルダへコピーする。
                        // 同じ名前の別の画像があったときは番号が付くので、意味の欄のパスもそれに合わせる
                        let mut meaning = meaning;
                        if let Some(dict) = current_target(c).and_then(|t| d.dict_paths.get(t)) {
                            for (src, token) in &c.pending_pics {
                                if !meaning.contains(&format!("({token})")) {
                                    continue; // 書いた行を消していたら、コピーしない
                                }
                                match pics::place(src, dict) {
                                    Ok(rel) => {
                                        if rel != *token {
                                            meaning = meaning.replace(&format!("({token})"), &format!("({rel})"));
                                        }
                                    }
                                    Err(e) => {
                                        set_text(c.err, &format!("画像をコピーできませんでした({}): {e}", src.display()));
                                        return LRESULT(0);
                                    }
                                }
                            }
                        }
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
        WM_NOTIFY => {
            let hdr = &*(lparam.0 as *const NMHDR);
            if !ctl.is_null() && hdr.idFrom == E_ADDPIC && (hdr.code == NM_CLICK || hdr.code == NM_RETURN) {
                add_picture(hwnd, &mut *ctl);
                return LRESULT(0);
            }
            // 「その語を表示」: 登録をやめて、すでにある語を表示する
            if !ctl.is_null() && hdr.idFrom == E_DUPLINK && (hdr.code == NM_CLICK || hdr.code == NM_RETURN) {
                let c = &mut *ctl;
                let key = core::fold(get_text(c.word).trim());
                let target = current_target(c);
                let d = &mut *c.data;
                // 登録先の辞書にあればその語を、無ければ最初に見つかった語を表示する
                let pick = d.same_words.get(&key).and_then(|h| h.iter().find(|x| Some(x.0) == target).or(h.first())).copied();
                if pick.is_some() {
                    let discard = !entry_modified(c)
                        || msg_box(hwnd, "入力した内容は保存されません。\nすでにある語を表示しますか?", "確認", MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) == IDYES;
                    if discard {
                        d.open_existing = pick;
                        d.ok = false;
                        let _ = DestroyWindow(hwnd);
                    }
                }
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

/// 「画像を追加…」: 画像を選んでもらい、意味の欄のカーソルの位置に `![](パス)` を入れる。
/// パスは、辞書と同じフォルダの `<辞書のファイル名>_pic` の中の、予定の名前。実際のコピーは、保存を押したときに行う
/// (キャンセルしたときに、ファイルを残さないため)
unsafe fn add_picture(hwnd: HWND, c: &mut EntryCtl) {
    let d = &*c.data;
    let Some(dict) = current_target(c).and_then(|t| d.dict_paths.get(t)) else { return };
    let mut buf = [0u16; 1024];
    let filter: Vec<u16> = "画像 (*.png;*.jpg;*.jpeg;*.gif;*.bmp)\0*.png;*.jpg;*.jpeg;*.gif;*.bmp\0すべてのファイル\0*.*\0\0".encode_utf16().collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrTitle: w!("追加する画像を選択"),
        Flags: OFN_PATHMUSTEXIST | OFN_FILEMUSTEXIST | OFN_HIDEREADONLY,
        ..Default::default()
    };
    if !GetOpenFileNameW(&mut ofn).as_bool() {
        return;
    }
    let len = buf.iter().position(|&ch| ch == 0).unwrap_or(0);
    let src = PathBuf::from(String::from_utf16_lossy(&buf[..len]));
    // 表示できる画像かを、先に確かめる
    let size = std::fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
    if size > pics::MAX_BYTES {
        set_text(c.err, &format!("画像が大きすぎます({} MB まで)", pics::MAX_BYTES / 1024 / 1024));
        return;
    }
    if std::fs::read(&src).ok().and_then(|b| pics::probe(&b)).is_none() {
        set_text(c.err, "この形式の画像は表示できません(PNG・JPEG・GIF・BMP に対応しています)");
        return;
    }
    set_text(c.err, "");
    let token = pics::planned_path(&src, dict);
    // カーソルの位置に入れ、説明を書けるように、カーソルを [ ] の間に置く
    let (mut a, mut b) = (0u32, 0u32);
    send(c.meaning, EM_GETSEL, &mut a as *mut _ as usize, &mut b as *mut _ as isize);
    let text = wide(&format!("![]({token})"));
    send(c.meaning, EM_REPLACESEL, 1, text.as_ptr() as isize);
    send(c.meaning, EM_SETSEL, a as usize + 2, a as isize + 2);
    let _ = SetFocus(c.meaning);
    c.pending_pics.push((src, token));
}

/// 同じ見出し語の語がある辞書と件数を、知らせの文にする
fn dup_text(hits: &[(usize, i64)], names: &[String]) -> String {
    let mut per: Vec<(usize, usize)> = Vec::new(); // (辞書の添字, 件数)。見つかった順
    for (d, _) in hits {
        match per.iter_mut().find(|p| p.0 == *d) {
            Some(p) => p.1 += 1,
            None => per.push((*d, 1)),
        }
    }
    let name = |d: usize| names.get(d).map(String::as_str).unwrap_or("?");
    if names.len() <= 1 {
        format!("同じ見出し語の語が、すでに {} 件あります。", hits.len())
    } else if per.len() == 1 {
        format!("同じ見出し語の語が、辞書「{}」に {} 件あります。", name(per[0].0), per[0].1)
    } else {
        let parts: Vec<String> = per.iter().map(|(d, n)| format!("「{}」に {} 件", name(*d), n)).collect();
        format!("同じ見出し語の語が、辞書{}あります。", parts.join("、"))
    }
}

/// 見出し語の欄の内容に合わせて、重複の知らせを出す / 消す
unsafe fn update_dup(c: &EntryCtl) {
    let d = &*c.data;
    let key = core::fold(get_text(c.word).trim());
    match d.same_words.get(&key).filter(|h| !key.is_empty() && !h.is_empty()) {
        Some(hits) => {
            set_text(c.dup, &dup_text(hits, &d.dict_names));
            let _ = ShowWindow(c.dup_link, SW_SHOW);
        }
        None => {
            set_text(c.dup, "");
            let _ = ShowWindow(c.dup_link, SW_HIDE);
        }
    }
}

/// いま選ばれている登録先の辞書(辞書の添字)
unsafe fn current_target(c: &EntryCtl) -> Option<usize> {
    let d = &*c.data;
    let i = if d.show_choice {
        let sel = send(c.combo, CB_GETCURSEL, 0, 0).0;
        if sel >= 0 {
            sel as usize
        } else {
            d.choice
        }
    } else {
        d.choice
    };
    d.targets.get(i).copied()
}

/// 見出し語以外の欄が、開いたときから変わっているか
unsafe fn entry_modified(c: &EntryCtl) -> bool {
    let d = &*c.data;
    let norm = |s: &str| s.replace("\r\n", "\n").trim().to_string();
    norm(&get_text(c.reading)) != norm(&d.reading)
        || core::normalize_tags(&get_text(c.tags)) != core::normalize_tags(&d.tags)
        || norm(&get_text(c.meaning)) != norm(&d.meaning)
        || norm(&get_text(c.link)) != norm(&d.link)
}

/// 保存の前に、登録先の辞書に同じ見出し語の語があれば確認する。保存してよければ true。
/// 編集で見出し語を変えていないときは、たずねない(同音異義語を編集するたびに出ないように)
unsafe fn confirm_same_word(hwnd: HWND, c: &EntryCtl, word: &str) -> bool {
    let d = &*c.data;
    let key = core::fold(word);
    let target = current_target(c);
    let n = d.same_words.get(&key).map_or(0, |h| h.iter().filter(|x| Some(x.0) == target).count());
    let unchanged = !d.orig_word.is_empty() && core::fold(d.orig_word.trim()) == key;
    if n == 0 || unchanged {
        return true;
    }
    let place = match target.and_then(|t| d.dict_names.get(t)) {
        Some(name) if d.dict_names.len() > 1 => format!("辞書「{name}」に"),
        _ => String::new(),
    };
    let text = format!("同じ見出し語「{word}」の語が、{place}すでに {n} 件あります。\n\n別の語として保存しますか?");
    msg_box(hwnd, &text, "同じ見出し語があります", MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) == IDYES
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
    /// 最後に実行した取り込みを、元に戻すための記録: (辞書のパス, 記録, 何を元に戻すのか)
    pub undo: Option<(PathBuf, Undo, String)>,
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
const S_LICENSE: usize = 432;

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
            // 左下に、アプリの版と、いま使っている設定ファイルの場所(個人の設定か、共用の設定かが分かるように)。
            // その下に、ライセンス情報を開くリンク
            let info = format!("{} {}  設定: {}", ui::APP_NAME, ui::APP_VERSION, d.cfg_path.display());
            let hinst2 = GetModuleHandleW(None).unwrap();
            let t = wide(&info);
            let info_h = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                PCWSTR(t.as_ptr()),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | 0x8000 /* SS_PATHELLIPSIS */ | 0x80 /* SS_NOPREFIX */),
                s(m),
                s(y - 1),
                s(fw - 2 * BTN_W - 30),
                s(18),
                hwnd,
                HMENU(S_MUTED as *mut c_void),
                hinst2,
                None,
            )
            .unwrap();
            send(info_h, WM_SETFONT, fonts.small.0 as usize, 1);
            let link_t = wide("<a>ライセンス情報</a>");
            if let Ok(link) = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("SysLink"),
                PCWSTR(link_t.as_ptr()),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                s(m),
                s(y + 18),
                s(160),
                s(20),
                hwnd,
                HMENU(S_LICENSE as *mut c_void),
                hinst2,
                None,
            ) {
                send(link, WM_SETFONT, fonts.small.0 as usize, 1);
            }
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
            if hdr.idFrom == S_LICENSE && (hdr.code == NM_CLICK || hdr.code == NM_RETURN) {
                license_dialog(hwnd);
                return LRESULT(0);
            }
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
                        } else if let Some((out, undo)) = import_into(hwnd, &dict, &d.backup_root) {
                            d.imported = true;
                            // 元に戻せるのは、直前の1回だけ
                            d.undo = (!undo.is_empty()).then(|| (dict.path.clone(), undo, format!("{} 件の取り込み", out.writes())));
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
// ライセンス情報
// ============================================================================

const L_TEXT: usize = 601;
const L_COPY: usize = 602;
const LICENSE_W: i32 = 680;
const LICENSE_H: i32 = 600;

struct LicenseCtl {
    fonts: Fonts,
    mono: HFONT,
    dpi: i32,
    text: HWND,
    note: HWND,
    fields: Vec<(RECT, HWND)>,
    footer_line: i32,
}

/// ライセンス情報の画面(このアプリのライセンスと、使用しているオープンソースソフトウェアのライセンス)
pub unsafe fn license_dialog(owner: HWND) {
    let title = format!("ライセンス情報 - {}", ui::APP_NAME);
    run_modal(owner, w!("MyDictionaryLicense"), &title, LICENSE_W, LICENSE_H, std::ptr::null());
}

unsafe extern "system" fn license_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ctl = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut LicenseCtl;
    match msg {
        WM_CREATE => {
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            let s = |v: i32| scale(v, dpi);
            let fonts = Fonts::new(dpi);
            // 区切り線(=====)や字下げをそろえるため、本文は等幅フォント
            let mono = ui::font("Consolas", 9, dpi, false);
            let (m, fw) = (24, LICENSE_W - 48);
            let mut fields = Vec::new();
            let mut y = 20;
            label(hwnd, "ライセンス情報", (s(m), s(y), s(fw), s(24)), fonts.heading, 0);
            y += 30;
            label(
                hwnd,
                &format!("{} は MIT License で提供しています。また、以下のオープンソースソフトウェアを使用しています。", ui::APP_NAME),
                (s(m), s(y), s(fw), s(40)),
                fonts.small,
                S_MUTED,
            );
            y += 46;
            let footer_y = LICENSE_H - CTL_H - 16;
            let text_h = footer_y - 16 - y;
            let text = ui::field(hwnd, L_TEXT, (s(m), s(y), s(fw), s(text_h)), dpi, true, &mut fields, mono);
            send(text, EM_SETREADONLY, 1, 0);
            send(text, EM_SETLIMITTEXT, 0, 0);
            let body = license::to_crlf(&license::full_text(ui::APP_NAME, ui::APP_VERSION));
            set_text(text, &body);
            send(text, EM_SETSEL, 0, 0);
            let footer_line = s(footer_y - 16);
            let note = label(hwnd, "", (s(m), s(footer_y + 9), s(fw - 2 * BTN_W - 30), s(20)), fonts.small, S_MUTED);
            button(hwnd, "すべてコピー", L_COPY, (s(m + fw - 2 * BTN_W - 10), s(footer_y), s(BTN_W), s(CTL_H)), true);
            button(hwnd, "閉じる", ID_CANCEL, (s(m + fw - BTN_W), s(footer_y), s(BTN_W), s(CTL_H)), true);
            let c = Box::new(LicenseCtl { fonts, mono, dpi, text, note, fields, footer_line });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(c) as isize);
            let _ = SetFocus(text);
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            if !ctl.is_null() {
                let c = &*ctl;
                ui::paint_fields(hdc, &c.fields, c.dpi);
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                ui::fill(hdc, &RECT { left: 0, top: c.footer_line, right: rc.right, bottom: c.footer_line + 1 }, BORDER);
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT => {
            // 読み取り専用の EDIT も WM_CTLCOLORSTATIC で来る。背景は白のまま
            let hdc = HDC(wparam.0 as *mut c_void);
            let id = GetDlgCtrlID(HWND(lparam.0 as *mut c_void));
            let color = if id as usize == S_MUTED { MUTED } else { TEXT };
            ui::ctl_color(hdc, color, WHITE, white_brush())
        }
        WM_DRAWITEM => {
            let dis = &*(lparam.0 as *const DRAWITEMSTRUCT);
            if !ctl.is_null() {
                let kind = if dis.CtlID as usize == ID_CANCEL { Btn::Primary } else { Btn::Secondary };
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
            let c = &*ctl;
            match (id, code) {
                (L_TEXT, EN_SETFOCUS_ | EN_KILLFOCUS_) => {
                    ui::invalidate_field(hwnd, &c.fields, c.text);
                }
                (L_COPY, _) => {
                    let ok = ui::set_clipboard(hwnd, &get_text(c.text));
                    set_text(c.note, if ok { "クリップボードにコピーしました" } else { "コピーできませんでした" });
                }
                // Esc・閉じる・Ctrl+Enter(run_modal が ID_OK を送る)
                (ID_CANCEL | ID_OK, _) => {
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
                let _ = DeleteObject(c.mono);
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

/// 取り込みの確認画面。3つの扱い(両方残す・上書き・飛ばす)から選んでもらう。
/// `plans` は、それぞれを選んだときの見込み。キャンセルなら None。
/// 選択肢つきの標準の確認画面(TaskDialog)を使う。使えない環境では、これまでと同じ「両方残す」だけの確認にする
unsafe fn ask_import_mode(owner: HWND, dict_name: &str, total: usize, preview: &str, plans: &[import::Outcome; 3]) -> Option<import::Mode> {
    const ID_GO: i32 = 1; // IDOK
    const R_KEEP: i32 = 101;
    const R_OVER: i32 = 102;
    const R_SKIP: i32 = 103;
    let title = wide("取り込み");
    let main = wide(&format!("{total} 件を、辞書「{dict_name}」に取り込みます"));
    let content = wide(&format!("{preview}\n同じ見出し語の語がすでにあるときの扱いを選んでください。"));
    let radios_text = [
        wide(&format!("両方残す(これまでの動作): {}", plans[0].summary())),
        wide(&format!("ファイルの内容で上書きする: {}", plans[1].summary())),
        wide(&format!("見出し語がすでにある語は取り込まない: {}", plans[2].summary())),
    ];
    let radios = [
        TASKDIALOG_BUTTON { nButtonID: R_KEEP, pszButtonText: PCWSTR(radios_text[0].as_ptr()) },
        TASKDIALOG_BUTTON { nButtonID: R_OVER, pszButtonText: PCWSTR(radios_text[1].as_ptr()) },
        TASKDIALOG_BUTTON { nButtonID: R_SKIP, pszButtonText: PCWSTR(radios_text[2].as_ptr()) },
    ];
    let (go, cancel) = (wide("取り込む"), wide("キャンセル"));
    let buttons = [
        TASKDIALOG_BUTTON { nButtonID: ID_GO, pszButtonText: PCWSTR(go.as_ptr()) },
        TASKDIALOG_BUTTON { nButtonID: 2 /* IDCANCEL */, pszButtonText: PCWSTR(cancel.as_ptr()) },
    ];
    // 上書きを選んだときに何が起きるかを、「詳細」に出す
    let details = import::overwrite_details(&plans[1]);
    let details_w = wide(&details);
    let (show, hide) = (wide("上書きの詳細を表示"), wide("詳細を隠す"));
    let mut cfg = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: owner,
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION | TDF_POSITION_RELATIVE_TO_WINDOW,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        pszMainInstruction: PCWSTR(main.as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        cButtons: buttons.len() as u32,
        pButtons: buttons.as_ptr(),
        nDefaultButton: ID_GO,
        cRadioButtons: radios.len() as u32,
        pRadioButtons: radios.as_ptr(),
        nDefaultRadioButton: R_KEEP,
        cxWidth: 300,
        ..Default::default()
    };
    if !details.is_empty() {
        cfg.pszExpandedInformation = PCWSTR(details_w.as_ptr());
        cfg.pszCollapsedControlText = PCWSTR(show.as_ptr());
        cfg.pszExpandedControlText = PCWSTR(hide.as_ptr());
    }
    let (mut button, mut radio) = (0i32, 0i32);
    if TaskDialogIndirect(&cfg, Some(&mut button), Some(&mut radio), None).is_ok() {
        return match (button, radio) {
            (ID_GO, R_OVER) => Some(import::Mode::Overwrite),
            (ID_GO, R_SKIP) => Some(import::Mode::Skip),
            (ID_GO, _) => Some(import::Mode::KeepBoth),
            _ => None,
        };
    }
    // TaskDialog が使えないとき: これまでと同じ確認
    let ask = format!(
        "{total} 件を、辞書「{dict_name}」に取り込みます。\n\n{preview}\nすでに同じ見出し語と意味の語がある場合は、重複として飛ばします。\n({})\n取り込みますか?",
        plans[0].summary()
    );
    (msg_box(owner, &ask, "インポート", MB_YESNO | MB_ICONQUESTION) == IDYES).then_some(import::Mode::KeepBoth)
}

/// ファイルを選び、確認のうえ辞書へ取り込む。取り込んだら、結果と、元に戻すための記録を返す
unsafe fn import_into(owner: HWND, dict: &DictConf, backup_root: &std::path::Path) -> Option<(import::Outcome, Undo)> {
    let file = pick_import_file(owner)?;
    let bytes = match std::fs::read(&file) {
        Ok(b) => b,
        Err(e) => {
            msg_box(owner, &format!("ファイルを読み込めません: {e}"), "インポート", MB_OK | MB_ICONERROR);
            return None;
        }
    };
    let Some(text) = decode_text(&bytes) else {
        msg_box(owner, "文字コードを判別できません。UTF-8 または Shift_JIS で保存したファイルを指定してください。", "インポート", MB_OK | MB_ICONERROR);
        return None;
    };
    // 拡張子が .json、または中身が [ か { で始まっていれば JSON として読む
    let head = text.trim_start_matches('\u{feff}').trim_start();
    let is_json = file.to_lowercase().ends_with(".json") || head.starts_with('[') || head.starts_with('{');
    let (rows, skipped) = if is_json {
        match json_io::parse(&text) {
            Ok(r) => r,
            Err(e) => {
                msg_box(owner, &format!("取り込めません。\n\n{e}"), "インポート", MB_OK | MB_ICONERROR);
                return None;
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
        return None;
    }
    let mut preview = String::new();
    for r in rows.iter().take(3) {
        let m: String = r.meaning.chars().take(30).map(|c| if c == '\n' { ' ' } else { c }).collect();
        preview.push_str(&format!("  ・{} — {}\n", r.word, m));
    }
    if rows.len() > 3 {
        preview.push_str("  …\n");
    }
    if skipped > 0 {
        preview.push_str(&format!("(見出し語か意味が空の {skipped} 行は取り込みません)\n"));
    }

    // 3つの扱いそれぞれの見込みを出す(辞書は変更しない)。まだ無い辞書は、ここで作られる
    let old = SetCursor(LoadCursorW(None, IDC_WAIT).unwrap());
    let plans = core::open_ro(&dict.path).or_else(|_| core::open(&dict.path)).and_then(|c| {
        let p = |mode| import::import_rows(&c, &rows, mode, true).map(|r| r.0);
        Ok([p(import::Mode::KeepBoth)?, p(import::Mode::Overwrite)?, p(import::Mode::Skip)?])
    });
    SetCursor(old);
    let plans = match plans {
        Ok(p) => p,
        Err(e) => {
            msg_box(owner, &format!("辞書を読み込めません。\n\n{e}"), "インポート", MB_OK | MB_ICONERROR);
            return None;
        }
    };
    let mode = ask_import_mode(owner, &dict.name, rows.len(), &preview, &plans)?;

    // 書き込みの中で、もう一度照合し直す(確認している間に、ほかの人が変更しているかもしれないため)
    let old = SetCursor(LoadCursorW(None, IDC_WAIT).unwrap());
    let r = safe::write(&dict.path, backup_root, |c| import::import_rows(c, &rows, mode, false));
    SetCursor(old);
    match r {
        Ok((out, undo)) => {
            let mut text = format!("{}。\n", out.summary());
            let details = import::overwrite_details(&import::Outcome { changes: Vec::new(), ..out.clone() });
            if !details.is_empty() {
                text.push_str(&format!("\n{details}"));
            }
            if !undo.is_empty() {
                text.push_str("\nこの取り込みは、設定の画面を閉じたあと、メイン画面の下段の「元に戻す」で取り消せます。");
            }
            msg_box(owner, &text, "インポート", MB_OK | MB_ICONINFORMATION);
            Some((out, undo))
        }
        Err(e) => {
            msg_box(owner, &format!("取り込めませんでした。辞書は変更されていません。\n\n{e}"), "インポート", MB_OK | MB_ICONERROR);
            None
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
// 書き出し(JSON / CSV / TSV)
// ============================================================================

/// 書き出しの形式
#[derive(Clone, Copy, PartialEq)]
enum ExportKind {
    Json,
    Csv,
    Tsv,
}

impl ExportKind {
    fn ext(self) -> &'static str {
        match self {
            ExportKind::Json => "json",
            ExportKind::Csv => "csv",
            ExportKind::Tsv => "tsv",
        }
    }

    fn of_ext(path: &str) -> Option<ExportKind> {
        let l = path.to_lowercase();
        [ExportKind::Json, ExportKind::Csv, ExportKind::Tsv].into_iter().find(|k| l.ends_with(&format!(".{}", k.ext())))
    }
}

/// 書き出し先と形式を選んでもらう。
/// ファイル名に .json / .csv / .tsv を書いていれば、その形式。書いていなければ「ファイルの種類」で決めて、拡張子を足す
/// (保存の画面には拡張子を自動で足させない。足されたものか、書かれたものかを見分けられないため)
unsafe fn pick_export_file(owner: HWND, default_name: &str) -> Option<(String, ExportKind)> {
    let mut buf = [0u16; 1024];
    for (i, ch) in default_name.encode_utf16().take(200).enumerate() {
        buf[i] = ch;
    }
    let filter: Vec<u16> =
        "JSON — そのまま取り込める控え (*.json)\0*.json\0CSV — Excel で開く (*.csv)\0*.csv\0TSV — タブ区切り (*.tsv)\0*.tsv\0すべてのファイル\0*.*\0\0".encode_utf16().collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        lpstrTitle: w!("書き出し先と形式を指定"),
        Flags: OFN_PATHMUSTEXIST | OFN_HIDEREADONLY | OFN_OVERWRITEPROMPT,
        ..Default::default()
    };
    if !GetSaveFileNameW(&mut ofn).as_bool() {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
    let chosen = String::from_utf16_lossy(&buf[..len]);
    let (path, kind) = match ExportKind::of_ext(&chosen) {
        Some(k) => (chosen.clone(), k),
        None => {
            let k = match ofn.nFilterIndex {
                2 => ExportKind::Csv,
                3 => ExportKind::Tsv,
                _ => ExportKind::Json,
            };
            (format!("{chosen}.{}", k.ext()), k)
        }
    };
    // 拡張子を足した場合は、上書きの確認をここで行う(保存の画面は、足す前の名前で確認している)
    if path != chosen && std::path::Path::new(&path).exists() {
        let text = format!("{path}\n\nこのファイルは、すでにあります。上書きしますか?");
        if msg_box(owner, &text, "書き出し", MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) != IDYES {
            return None;
        }
    }
    Some((path, kind))
}

/// 辞書の全語をファイルに書き出す(辞書ファイルの現在の内容を読む)
unsafe fn export_dict(owner: HWND, dict: &DictConf) {
    // ファイル名に使えない文字は置き換える
    let safe_name: String = dict.name.chars().map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c }).collect();
    let Some((file, kind)) = pick_export_file(owner, &safe_name) else { return };
    let old = SetCursor(LoadCursorW(None, IDC_WAIT).unwrap());
    let loaded = core::open_ro(&dict.path).and_then(|c| core::load_all(&c));
    let result = match loaded {
        Ok(mut entries) => {
            core::sort_entries(&mut entries);
            let text = match kind {
                ExportKind::Json => json_io::export(&entries),
                ExportKind::Csv => import::export_table(&entries, ','),
                ExportKind::Tsv => import::export_table(&entries, '\t'),
            };
            std::fs::write(&file, text).map(|_| entries.len()).map_err(|e| e.to_string())
        }
        Err(e) => Err(format!("辞書を読み込めません: {e}")),
    };
    SetCursor(old);
    match result {
        Ok(n) => {
            let note = match kind {
                ExportKind::Json => "このファイルは「取り込む…」でそのまま取り込めます。",
                _ => "表を直してから「取り込む…」で「上書きする」を選ぶと、直した内容を辞書に反映できます。\n「管理用」の列は、どの語かを見分けるためのものです。変えないでください。",
            };
            msg_box(owner, &format!("辞書「{}」の {n} 語を書き出しました。\n\n{file}\n\n{note}", dict.name), "エクスポート", MB_OK | MB_ICONINFORMATION);
        }
        Err(e) => {
            msg_box(owner, &format!("書き出せませんでした。\n\n{e}"), "エクスポート", MB_OK | MB_ICONERROR);
        }
    }
}

// ============================================================================
// タグの一覧
// ============================================================================

/// タグの一覧の画面で選ばれた、タグの一括変更
pub enum TagAction {
    /// 名前の変更(`to` がすでにあるタグなら、そのタグにまとめる)
    Rename { from: String, to: String },
    /// 削除(語からタグを外す。語そのものは消さない)
    Delete { tag: String },
}

pub struct TagsDlg {
    pub title: String,
    /// 一覧の上に出す説明
    pub note: String,
    /// (タグ, 語数)。表示する順
    pub tags: Vec<(String, usize)>,
    /// 入: はじめにチェックを入れるタグ / 出: OK のときにチェックされていたタグ(`tags` の順)
    pub checked: Vec<String>,
    /// 名前の変更・削除のボタンを出す
    pub manage: bool,
    /// 出: 名前の変更・削除が選ばれた(このとき `ok` は false)
    pub action: Option<TagAction>,
    pub ok: bool,
}

const T_FILTER: usize = 701;
const T_LIST: usize = 702;
const T_RENAME: usize = 703;
const T_DELETE: usize = 704;
const T_ERR: usize = 705;
const T_NOTE: usize = 706;
const TAGS_W: i32 = 460;
const TAGS_H: i32 = 540;

struct TagsCtl {
    data: *mut TagsDlg,
    fonts: Fonts,
    dpi: i32,
    filter: HWND,
    list: HWND,
    err: HWND,
    fields: Vec<(RECT, HWND)>,
    /// `data.tags` と同じ並びの、チェックの状態
    checks: Vec<bool>,
    /// 一覧の行 → `data.tags` の添字(絞り込みで、一部だけを表示しているため)
    rows: Vec<usize>,
}

/// タグの一覧を表示する。OK なら true(結果は `data.checked`)。
/// 名前の変更・削除が選ばれたときは false を返し、`data.action` に内容が入る
pub unsafe fn tags_dialog(owner: HWND, data: &mut TagsDlg) -> bool {
    data.ok = false;
    data.action = None;
    let title = data.title.clone();
    run_modal(owner, w!("MyDictionaryTags"), &title, TAGS_W, TAGS_H, data as *mut TagsDlg as *const c_void);
    data.ok
}

/// 絞り込みの欄に合うタグだけを、一覧に並べ直す
unsafe fn tags_fill(c: &mut TagsCtl) {
    let d = &*c.data;
    let key = core::fold(get_text(c.filter).trim());
    c.rows = (0..d.tags.len()).filter(|&i| key.is_empty() || core::fold(&d.tags[i].0).contains(&key)).collect();
    let (list, rows, checks) = (c.list, &c.rows, &c.checks);
    ui::quiet(|| {
        send(list, LVM_DELETEALLITEMS_, 0, 0);
        for (row, &i) in rows.iter().enumerate() {
            let mut t = wide(&d.tags[i].0);
            let item = LVITEMW { mask: LVIF_TEXT, iItem: row as i32, pszText: PWSTR(t.as_mut_ptr()), ..Default::default() };
            send(list, LVM_INSERTITEMW, 0, &item as *const _ as isize);
            lv_set_cell(list, row, 1, &format!("{} 語", d.tags[i].1));
            set_check(list, row, checks[i]);
        }
    });
}

/// 一覧で選んでいる(青く表示されている)タグの、`data.tags` の添字
unsafe fn tags_selected(c: &TagsCtl) -> Option<usize> {
    let row = send(c.list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize).0;
    if row < 0 {
        None
    } else {
        c.rows.get(row as usize).copied()
    }
}

unsafe extern "system" fn tags_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ctl = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TagsCtl;
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let data = cs.lpCreateParams as *mut TagsDlg;
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            let s = |v: i32| scale(v, dpi);
            let fonts = Fonts::new(dpi);
            let d = &*data;
            let (m, fw) = (20, TAGS_W - 40);
            let hinst = GetModuleHandleW(None).unwrap();
            let mut fields = Vec::new();
            let mut y = 16;
            label(hwnd, &d.note, (s(m), s(y), s(fw), s(38)), fonts.small, T_NOTE);
            y += 44;
            let filter = ui::field(hwnd, T_FILTER, (s(m), s(y), s(fw), s(CTL_H)), dpi, false, &mut fields, fonts.base);
            let cue = wide("タグの名前で絞り込む");
            send(filter, EM_SETCUEBANNER, 1, cue.as_ptr() as isize);
            y += CTL_H + 10;
            let list_h = TAGS_H - y - (if d.manage { CTL_H + 10 } else { 0 }) - 24 - CTL_H - 20 - 10;
            let list = CreateWindowExW(
                WS_EX_CLIENTEDGE,
                w!("SysListView32"),
                PCWSTR::null(),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | LVS_REPORT as u32 | LVS_SINGLESEL as u32 | LVS_SHOWSELALWAYS as u32),
                s(m),
                s(y),
                s(fw),
                s(list_h),
                hwnd,
                HMENU(T_LIST as *mut c_void),
                hinst,
                None,
            )
            .unwrap();
            send(list, WM_SETFONT, fonts.base.0 as usize, 1);
            send(list, LVM_SETEXTENDEDLISTVIEWSTYLE, 0, (LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize);
            let _ = SetWindowTheme(list, w!("Explorer"), PCWSTR::null());
            for (i, (t, wd)) in [("タグ", fw - 130), ("語数", 96)].iter().enumerate() {
                let mut tt = wide(t);
                let col = LVCOLUMNW { mask: LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM, cx: s(*wd), pszText: PWSTR(tt.as_mut_ptr()), iSubItem: i as i32, ..Default::default() };
                send(list, LVM_INSERTCOLUMNW, i, &col as *const _ as isize);
            }
            y += list_h + 10;
            if d.manage {
                button(hwnd, "名前の変更…", T_RENAME, (s(m), s(y), s(BTN_W + 16), s(CTL_H)), true);
                button(hwnd, "削除", T_DELETE, (s(m + BTN_W + 26), s(y), s(BTN_W), s(CTL_H)), true);
                y += CTL_H + 10;
            }
            let err = label(hwnd, "", (s(m), s(y), s(fw), s(20)), fonts.small, T_ERR);
            y += 24;
            button(hwnd, "OK", ID_OK, (s(m + fw - 2 * BTN_W - 10), s(y), s(BTN_W), s(CTL_H)), true);
            button(hwnd, "キャンセル", ID_CANCEL, (s(m + fw - BTN_W), s(y), s(BTN_W), s(CTL_H)), true);

            let checks: Vec<bool> = d.tags.iter().map(|(t, _)| d.checked.contains(t)).collect();
            let c = Box::new(TagsCtl { data, fonts, dpi, filter, list, err, fields, checks, rows: Vec::new() });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(c) as isize);
            let c = &mut *(GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut TagsCtl);
            tags_fill(c);
            if d.tags.is_empty() {
                set_text(c.err, "タグは、まだ1つもありません");
            }
            let _ = SetFocus(filter);
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
            let color = match GetDlgCtrlID(HWND(lparam.0 as *mut c_void)) as usize {
                T_ERR => DANGER,
                T_NOTE => MUTED,
                _ => TEXT,
            };
            ui::ctl_color(hdc, color, WHITE, white_brush())
        }
        WM_DRAWITEM => {
            let dis = &*(lparam.0 as *const DRAWITEMSTRUCT);
            if !ctl.is_null() {
                let kind = match dis.CtlID as usize {
                    ID_OK => Btn::Primary,
                    T_DELETE => Btn::Danger,
                    _ => Btn::Secondary,
                };
                ui::draw_button(dis, kind, (*ctl).fonts.base, WHITE, (*ctl).dpi);
            }
            LRESULT(1)
        }
        WM_NOTIFY => {
            // チェックの付け外しを控える(絞り込みで行を並べ直しても、状態を保てるように)
            let hdr = &*(lparam.0 as *const NMHDR);
            if !ctl.is_null() && hdr.idFrom == T_LIST && hdr.code == LVN_ITEMCHANGED && !ui::is_busy() {
                let nm = &*(lparam.0 as *const NMLISTVIEW);
                let c = &mut *ctl;
                if (nm.uOldState ^ nm.uNewState) & 0xF000 != 0 && nm.iItem >= 0 {
                    if let Some(&i) = c.rows.get(nm.iItem as usize) {
                        c.checks[i] = get_check(c.list, nm.iItem as usize);
                    }
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
                (T_FILTER, EN_SETFOCUS_ | EN_KILLFOCUS_) => {
                    ui::invalidate_field(hwnd, &c.fields, c.filter);
                }
                (T_FILTER, EN_CHANGE_) if !ui::is_busy() => {
                    tags_fill(c);
                    set_text(c.err, "");
                }
                (T_RENAME, _) => match tags_selected(c) {
                    None => set_text(c.err, "名前を変えるタグを、一覧で選んでください"),
                    Some(i) => {
                        let from = d.tags[i].0.clone();
                        let label = format!("タグ「{from}」の新しい名前を入力してください。\nすでにあるタグの名前にすると、そのタグにまとめられます。");
                        if let Some(input) = prompt_dialog(hwnd, "タグの名前の変更", &label, &from) {
                            let to = core::normalize_tags(&input);
                            if to.is_empty() || to.contains(' ') {
                                set_text(c.err, "タグの名前は、空白や区切りを含まない1語で入力してください");
                            } else if to == from {
                                set_text(c.err, "名前が変わっていません");
                            } else {
                                d.action = Some(TagAction::Rename { from, to });
                                let _ = DestroyWindow(hwnd);
                            }
                        }
                    }
                },
                (T_DELETE, _) => match tags_selected(c) {
                    None => set_text(c.err, "削除するタグを、一覧で選んでください"),
                    Some(i) => {
                        let (tag, n) = d.tags[i].clone();
                        let text = format!("タグ「{tag}」を、{n} 語から外しますか?\n\n語そのものは消えません。直後なら「元に戻す」で取り消せます。");
                        if msg_box(hwnd, &text, "タグの削除", MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) == IDYES {
                            d.action = Some(TagAction::Delete { tag });
                            let _ = DestroyWindow(hwnd);
                        }
                    }
                },
                (ID_OK, _) => {
                    d.checked = d.tags.iter().zip(&c.checks).filter(|(_, &on)| on).map(|(t, _)| t.0.clone()).collect();
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
        // Enter = OK(絞り込みの欄で Enter を押すと、そのまま確定する)
        DM_GETDEFID_ => LRESULT((DC_HASDEFID_ << 16) | ID_OK as isize),
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
// 1行の入力(タグの名前の変更など)
// ============================================================================

struct PromptDlg {
    label: String,
    text: String,
    ok: bool,
}

const P_TEXT: usize = 801;
const P_LABEL: usize = 802;

struct PromptCtl {
    data: *mut PromptDlg,
    fonts: Fonts,
    dpi: i32,
    text: HWND,
    fields: Vec<(RECT, HWND)>,
}

/// 1行の文字を入力してもらう。OK なら、入力された文字(前後の空白は除く)
unsafe fn prompt_dialog(owner: HWND, title: &str, label: &str, initial: &str) -> Option<String> {
    let mut data = PromptDlg { label: label.to_string(), text: initial.to_string(), ok: false };
    run_modal(owner, w!("MyDictionaryPrompt"), title, 460, 196, &mut data as *mut PromptDlg as *const c_void);
    data.ok.then(|| data.text.trim().to_string())
}

unsafe extern "system" fn prompt_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ctl = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PromptCtl;
    match msg {
        WM_CREATE => {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            let data = cs.lpCreateParams as *mut PromptDlg;
            let dpi = GetDpiForWindow(hwnd).max(96) as i32;
            let s = |v: i32| scale(v, dpi);
            let fonts = Fonts::new(dpi);
            let d = &*data;
            let (m, fw) = (20, 460 - 40);
            let mut fields = Vec::new();
            label(hwnd, &d.label, (s(m), s(18), s(fw), s(44)), fonts.small, P_LABEL);
            let text = ui::field(hwnd, P_TEXT, (s(m), s(70), s(fw), s(CTL_H)), dpi, false, &mut fields, fonts.base);
            set_text(text, &d.text);
            button(hwnd, "OK", ID_OK, (s(m + fw - 2 * BTN_W - 10), s(140), s(BTN_W), s(CTL_H)), true);
            button(hwnd, "キャンセル", ID_CANCEL, (s(m + fw - BTN_W), s(140), s(BTN_W), s(CTL_H)), true);
            let c = Box::new(PromptCtl { data, fonts, dpi, text, fields });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(c) as isize);
            let _ = SetFocus(text);
            send(text, EM_SETSEL, 0, -1);
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
            let color = if GetDlgCtrlID(HWND(lparam.0 as *mut c_void)) as usize == P_LABEL { MUTED } else { TEXT };
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
                (P_TEXT, EN_SETFOCUS_ | EN_KILLFOCUS_) => {
                    ui::invalidate_field(hwnd, &c.fields, c.text);
                }
                (ID_OK, _) => {
                    let d = &mut *c.data;
                    d.text = get_text(c.text);
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
        // Enter = OK
        DM_GETDEFID_ => LRESULT((DC_HASDEFID_ << 16) | ID_OK as isize),
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
