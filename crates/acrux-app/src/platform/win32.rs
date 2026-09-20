//! Implémentation Win32 (user32, gdi32, comdlg32, shell32) sans dépendance :
//! déclarations FFI écrites à la main d'après la documentation Windows.
//!
//! Invariants de sûreté de ce module :
//! - tous les pointeurs passés au système sont valides pendant l'appel ;
//! - l'état de l'application est stocké dans `GWLP_USERDATA` sous forme de
//!   `Box<WindowState>` créé avant `CreateWindowExW` et libéré à `WM_NCDESTROY` ;
//! - la procédure de fenêtre ne panique jamais (une panique traversant une
//!   frontière FFI est un comportement indéfini) : elle est enveloppée dans
//!   `catch_unwind`.
#![allow(unsafe_code)]
#![allow(non_snake_case, non_camel_case_types, clippy::upper_case_acronyms)]
// Conversions entre les types entiers de l'API Win32 (WPARAM, LPARAM, RECT…)
// et ceux de l'application : volontaires et bornées par le protocole des messages.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

use std::ffi::c_void;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::ptr::{null, null_mut};

use crate::ui::cursors::{self, Shape};

use super::{
    App, Cursor, Event, Frame, Key, Modifiers, MouseButton, PrintOutcome, PrintSource, WindowHandle,
};

type HWND = *mut c_void;
type HINSTANCE = *mut c_void;
type HICON = *mut c_void;
type HCURSOR = *mut c_void;
type HBRUSH = *mut c_void;
type HDC = *mut c_void;
type HMENU = *mut c_void;
type HBITMAP = *mut c_void;
type WPARAM = usize;
type LPARAM = isize;
type LRESULT = isize;
type UINT = u32;
type DWORD = u32;
type BOOL = i32;
type ATOM = u16;
type LONG = i32;
type LONG_PTR = isize;

#[repr(C)]
struct WNDCLASSEXW {
    cbSize: UINT,
    style: UINT,
    lpfnWndProc: Option<unsafe extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT>,
    cbClsExtra: i32,
    cbWndExtra: i32,
    hInstance: HINSTANCE,
    hIcon: HICON,
    hCursor: HCURSOR,
    hbrBackground: HBRUSH,
    lpszMenuName: *const u16,
    lpszClassName: *const u16,
    hIconSm: HICON,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct POINT {
    x: LONG,
    y: LONG,
}

#[repr(C)]
struct MSG {
    hwnd: HWND,
    message: UINT,
    wParam: WPARAM,
    lParam: LPARAM,
    time: DWORD,
    pt: POINT,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct RECT {
    left: LONG,
    top: LONG,
    right: LONG,
    bottom: LONG,
}

/// Dialogue d'impression (commdlg.h, `PRINTDLGW`).
#[repr(C)]
#[allow(non_snake_case)]
struct PRINTDLGW {
    lStructSize: DWORD,
    hwndOwner: HWND,
    hDevMode: *mut c_void,
    hDevNames: *mut c_void,
    hDC: HDC,
    Flags: DWORD,
    nFromPage: u16,
    nToPage: u16,
    nMinPage: u16,
    nMaxPage: u16,
    nCopies: u16,
    hInstance: HINSTANCE,
    lCustData: LPARAM,
    lpfnPrintHook: *const c_void,
    lpfnSetupHook: *const c_void,
    lpPrintTemplateName: *const u16,
    lpSetupTemplateName: *const u16,
    hPrintTemplate: *mut c_void,
    hSetupTemplate: *mut c_void,
}

/// Description d'un travail d'impression (`DOCINFOW`).
#[repr(C)]
#[allow(non_snake_case)]
struct DOCINFOW {
    cbSize: i32,
    lpszDocName: *const u16,
    lpszOutput: *const u16,
    lpszDatatype: *const u16,
    fwType: DWORD,
}

const PD_PAGENUMS: DWORD = 0x0000_0002;
const PD_NOSELECTION: DWORD = 0x0000_0004;
const PD_RETURNDC: DWORD = 0x0000_0100;
const PD_USEDEVMODECOPIESANDCOLLATE: DWORD = 0x0004_0000;
const PD_HIDEPRINTTOFILE: DWORD = 0x0010_0000;
const HORZRES: i32 = 8;
const VERTRES: i32 = 10;
const LOGPIXELSX: i32 = 88;
const LOGPIXELSY: i32 = 90;

#[repr(C)]
struct PAINTSTRUCT {
    hdc: HDC,
    fErase: BOOL,
    rcPaint: RECT,
    fRestore: BOOL,
    fIncUpdate: BOOL,
    rgbReserved: [u8; 32],
}

#[repr(C)]
struct BITMAPINFOHEADER {
    biSize: DWORD,
    biWidth: LONG,
    biHeight: LONG,
    biPlanes: u16,
    biBitCount: u16,
    biCompression: DWORD,
    biSizeImage: DWORD,
    biXPelsPerMeter: LONG,
    biYPelsPerMeter: LONG,
    biClrUsed: DWORD,
    biClrImportant: DWORD,
}

#[repr(C)]
struct BITMAPINFO {
    bmiHeader: BITMAPINFOHEADER,
    bmiColors: [u32; 1],
}

#[repr(C)]
struct OPENFILENAMEW {
    lStructSize: DWORD,
    hwndOwner: HWND,
    hInstance: HINSTANCE,
    lpstrFilter: *const u16,
    lpstrCustomFilter: *mut u16,
    nMaxCustFilter: DWORD,
    nFilterIndex: DWORD,
    lpstrFile: *mut u16,
    nMaxFile: DWORD,
    lpstrFileTitle: *mut u16,
    nMaxFileTitle: DWORD,
    lpstrInitialDir: *const u16,
    lpstrTitle: *const u16,
    Flags: DWORD,
    nFileOffset: u16,
    nFileExtension: u16,
    lpstrDefExt: *const u16,
    lCustData: LPARAM,
    lpfnHook: *const c_void,
    lpTemplateName: *const u16,
    pvReserved: *mut c_void,
    dwReserved: DWORD,
    FlagsEx: DWORD,
}

const CS_HREDRAW: UINT = 0x0002;
const CS_VREDRAW: UINT = 0x0001;
const WS_OVERLAPPEDWINDOW: DWORD = 0x00CF_0000;
const WS_POPUP: DWORD = 0x8000_0000;
const GWL_STYLE: i32 = -16;
const SWP_FRAMECHANGED: UINT = 0x0020;
const SWP_SHOWWINDOW: UINT = 0x0040;
const MONITOR_DEFAULTTONEAREST: DWORD = 2;

/// Informations d'un moniteur (`MONITORINFO`).
#[repr(C)]
#[allow(non_snake_case)]
struct MONITORINFO {
    cbSize: DWORD,
    rcMonitor: RECT,
    rcWork: RECT,
    dwFlags: DWORD,
}
const WS_VISIBLE: DWORD = 0x1000_0000;
const CW_USEDEFAULT: i32 = 0x8000_0000_u32 as i32;
const SW_SHOW: i32 = 5;
const SW_MAXIMIZE: i32 = 3;
const SIZE_MAXIMIZED: WPARAM = 2;
const SWP_NOMOVE: UINT = 0x0002;
const SWP_NOZORDER: UINT = 0x0004;
const SWP_NOACTIVATE: UINT = 0x0010;
/// Ressource d'icône du programme, incorporée par `build.rs` (voir `acrux-winres`).
const IDI_ACRUX: usize = 1;
/// Type de ressource demandé à `LoadImageW`.
const IMAGE_ICON: UINT = 1;
/// L'icône appartient au système : ni copie, ni libération à notre charge.
const LR_SHARED: UINT = 0x0000_8000;
/// Indices de `GetSystemMetrics` pour la taille des icônes, grande puis petite.
const SM_CXCURSOR: i32 = 13;
const SM_CXICON: i32 = 11;
const SM_CYICON: i32 = 12;
const SM_CXSMICON: i32 = 49;
const SM_CYSMICON: i32 = 50;

const IDC_ARROW: usize = 32512;
const IDC_IBEAM: usize = 32513;
const IDC_HAND: usize = 32649;
const IDC_SIZENWSE: usize = 32642;
const IDC_SIZENESW: usize = 32643;
const IDC_SIZEWE: usize = 32644;
const IDC_SIZENS: usize = 32645;
const CS_DBLCLKS: UINT = 0x0008;
const WM_SETCURSOR: UINT = 0x0020;
const WM_LBUTTONDBLCLK: UINT = 0x0203;
const HTCLIENT: isize = 1;
const CF_UNICODETEXT: UINT = 13;
const GMEM_MOVEABLE: UINT = 0x0002;
const GWLP_USERDATA: i32 = -21;
const DIB_RGB_COLORS: UINT = 0;
const BI_RGB: DWORD = 0;
const SRCCOPY: DWORD = 0x00CC_0020;
const OFN_FILEMUSTEXIST: DWORD = 0x0000_1000;
const OFN_PATHMUSTEXIST: DWORD = 0x0000_0800;
const OFN_EXPLORER: DWORD = 0x0008_0000;
const OFN_OVERWRITEPROMPT: DWORD = 0x0000_0002;
const MB_YESNO: UINT = 0x4;
const MB_ICONQUESTION: UINT = 0x20;
const IDYES: i32 = 6;
const MB_ICONERROR: UINT = 0x10;

const WM_CREATE: UINT = 0x0001;
const WM_DESTROY: UINT = 0x0002;
const WM_SIZE: UINT = 0x0005;
const WM_PAINT: UINT = 0x000F;
const WM_CLOSE: UINT = 0x0010;
const WM_ERASEBKGND: UINT = 0x0014;
const WM_NCDESTROY: UINT = 0x0082;
const WM_KEYDOWN: UINT = 0x0100;
const WM_CHAR: UINT = 0x0102;
const WM_MOUSEMOVE: UINT = 0x0200;
const WM_LBUTTONDOWN: UINT = 0x0201;
const WM_LBUTTONUP: UINT = 0x0202;
const WM_RBUTTONDOWN: UINT = 0x0204;
const WM_RBUTTONUP: UINT = 0x0205;
const WM_MBUTTONDOWN: UINT = 0x0207;
const WM_MBUTTONUP: UINT = 0x0208;
const WM_MOUSEWHEEL: UINT = 0x020A;
const WM_DROPFILES: UINT = 0x0233;
const WM_DPICHANGED: UINT = 0x02E0;
/// Message privé (`WM_APP + 1`) envoyé par les fils de travail pour réveiller la fenêtre.
const WM_APP_WAKE: UINT = 0x8000 + 1;
const VK_SHIFT: i32 = 0x10;
const VK_CONTROL: i32 = 0x11;
const VK_MENU: i32 = 0x12;
const MK_LBUTTON: WPARAM = 0x0001;

#[repr(C)]
struct ICONINFO {
    f_icon: BOOL,
    x_hotspot: DWORD,
    y_hotspot: DWORD,
    hbm_mask: HBITMAP,
    hbm_color: HBITMAP,
}

#[link(name = "user32")]
extern "system" {
    fn CreateIconIndirect(info: *const ICONINFO) -> HICON;
    fn RegisterClassExW(class: *const WNDCLASSEXW) -> ATOM;
    fn CreateWindowExW(
        ex_style: DWORD,
        class_name: *const u16,
        window_name: *const u16,
        style: DWORD,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        parent: HWND,
        menu: HMENU,
        instance: HINSTANCE,
        param: *mut c_void,
    ) -> HWND;
    fn ShowWindow(hwnd: HWND, cmd: i32) -> BOOL;
    fn UpdateWindow(hwnd: HWND) -> BOOL;
    fn GetMessageW(msg: *mut MSG, hwnd: HWND, min: UINT, max: UINT) -> BOOL;
    fn TranslateMessage(msg: *const MSG) -> BOOL;
    fn DispatchMessageW(msg: *const MSG) -> LRESULT;
    fn DefWindowProcW(hwnd: HWND, msg: UINT, wparam: WPARAM, lparam: LPARAM) -> LRESULT;
    fn PostQuitMessage(code: i32);
    fn DestroyWindow(hwnd: HWND) -> BOOL;
    fn BeginPaint(hwnd: HWND, ps: *mut PAINTSTRUCT) -> HDC;
    fn EndPaint(hwnd: HWND, ps: *const PAINTSTRUCT) -> BOOL;
    fn GetClientRect(hwnd: HWND, rect: *mut RECT) -> BOOL;
    fn InvalidateRect(hwnd: HWND, rect: *const RECT, erase: BOOL) -> BOOL;
    fn SetWindowTextW(hwnd: HWND, text: *const u16) -> BOOL;
    fn LoadCursorW(instance: HINSTANCE, name: *const u16) -> HCURSOR;
    fn LoadImageW(
        instance: HINSTANCE,
        name: *const u16,
        kind: UINT,
        cx: i32,
        cy: i32,
        flags: UINT,
    ) -> *mut c_void;
    fn GetSystemMetrics(index: i32) -> i32;
    fn GetUserDefaultUILanguage() -> u16;
    fn SetWindowLongPtrW(hwnd: HWND, index: i32, value: LONG_PTR) -> LONG_PTR;
    fn GetWindowLongPtrW(hwnd: HWND, index: i32) -> LONG_PTR;
    fn GetWindowRect(hwnd: HWND, rect: *mut RECT) -> BOOL;
    fn MonitorFromWindow(hwnd: HWND, flags: DWORD) -> *mut c_void;
    fn GetMonitorInfoW(monitor: *mut c_void, info: *mut MONITORINFO) -> BOOL;
    fn GetKeyState(key: i32) -> i16;
    fn MessageBoxW(hwnd: HWND, text: *const u16, caption: *const u16, kind: UINT) -> i32;
    fn SetProcessDpiAwarenessContext(context: isize) -> BOOL;
    fn GetDpiForWindow(hwnd: HWND) -> UINT;
    fn SetWindowPos(hwnd: HWND, after: HWND, x: i32, y: i32, cx: i32, cy: i32, flags: UINT)
        -> BOOL;
    fn SetCursor(cursor: HCURSOR) -> HCURSOR;
    fn PostMessageW(hwnd: HWND, msg: UINT, wparam: WPARAM, lparam: LPARAM) -> BOOL;
    fn OpenClipboard(owner: HWND) -> BOOL;
    fn EmptyClipboard() -> BOOL;
    fn SetClipboardData(format: UINT, handle: *mut c_void) -> *mut c_void;
    fn GetClipboardData(format: UINT) -> *mut c_void;
    fn CloseClipboard() -> BOOL;
}

/// Réveil d'une fenêtre depuis un autre fil.
struct Win32Waker(usize);

// SAFETY : un HWND est un identifiant opaque ; PostMessageW est explicitement
// utilisable depuis n'importe quel fil (documentation Windows).
#[allow(unsafe_code)]
unsafe impl Send for Win32Waker {}

impl super::Waker for Win32Waker {
    fn wake(&self) {
        // SAFETY : PostMessageW accepte un HWND détruit (échec silencieux) et
        // ne déréférence aucun pointeur fourni ici.
        unsafe {
            PostMessageW(self.0 as HWND, WM_APP_WAKE, 0, 0);
        }
    }
}

/// Langue de l'interface de l'utilisateur, en deux lettres.
///
/// `GetUserDefaultUILanguage` rend un identifiant de langue dont les dix
/// bits de poids faible donnent la langue principale (§ LANGID) : 0x0C pour
/// le français, 0x09 pour l'anglais.
#[must_use]
pub fn system_language() -> String {
    // SAFETY : sans précondition, sans argument.
    let id = unsafe { GetUserDefaultUILanguage() };
    match id & 0x3FF {
        0x0C => "fr".into(),
        0x07 => "de".into(),
        0x0A => "es".into(),
        0x10 => "it".into(),
        // Anglais, et tout ce qu'Acrux ne sait pas encore parler.
        _ => "en".into(),
    }
}

#[link(name = "dwmapi")]
extern "system" {
    fn DwmSetWindowAttribute(
        hwnd: HWND,
        attribute: DWORD,
        value: *const c_void,
        size: DWORD,
    ) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    fn CreateBitmap(
        width: i32,
        height: i32,
        planes: UINT,
        bits_per_pixel: UINT,
        bits: *const c_void,
    ) -> HBITMAP;
    fn DeleteObject(object: *mut c_void) -> BOOL;
    fn GetPixel(hdc: HDC, x: i32, y: i32) -> DWORD;
    fn GetDeviceCaps(hdc: HDC, index: i32) -> i32;
    fn StartDocW(hdc: HDC, info: *const DOCINFOW) -> i32;
    fn StartPage(hdc: HDC) -> i32;
    fn EndPage(hdc: HDC) -> i32;
    fn EndDoc(hdc: HDC) -> i32;
    fn AbortDoc(hdc: HDC) -> i32;
    fn DeleteDC(hdc: HDC) -> BOOL;
    fn CreateDCW(
        driver: *const u16,
        device: *const u16,
        output: *const u16,
        init: *const c_void,
    ) -> HDC;
    fn StretchDIBits(
        hdc: HDC,
        x_dest: i32,
        y_dest: i32,
        dest_width: i32,
        dest_height: i32,
        x_src: i32,
        y_src: i32,
        src_width: i32,
        src_height: i32,
        bits: *const c_void,
        info: *const BITMAPINFO,
        usage: UINT,
        rop: DWORD,
    ) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> HINSTANCE;
    fn GlobalAlloc(flags: UINT, bytes: usize) -> *mut c_void;
    fn GlobalLock(handle: *mut c_void) -> *mut c_void;
    fn GlobalUnlock(handle: *mut c_void) -> BOOL;
    fn GlobalFree(handle: *mut c_void) -> *mut c_void;
    fn GlobalSize(handle: *mut c_void) -> usize;
}

/// Copie du texte dans le presse-papiers (format Unicode).
fn set_clipboard_text(hwnd: HWND, text: &str) {
    let utf16 = wide(text);
    let bytes = utf16.len() * 2;
    // SAFETY : séquence documentée par Windows. Le bloc global alloué est
    // verrouillé, rempli avec exactement `bytes` octets (taille demandée),
    // déverrouillé puis cédé au système par SetClipboardData ; si cette
    // dernière échoue, le bloc nous appartient encore et est libéré.
    unsafe {
        if OpenClipboard(hwnd) == 0 {
            return;
        }
        EmptyClipboard();
        let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if !handle.is_null() {
            let dst = GlobalLock(handle);
            if dst.is_null() {
                GlobalFree(handle);
            } else {
                std::ptr::copy_nonoverlapping(utf16.as_ptr().cast::<u8>(), dst.cast::<u8>(), bytes);
                GlobalUnlock(handle);
                if SetClipboardData(CF_UNICODETEXT, handle).is_null() {
                    GlobalFree(handle);
                }
            }
        }
        CloseClipboard();
    }
}

/// Lit le texte du presse-papiers (format Unicode).
fn clipboard_text(hwnd: HWND) -> Option<String> {
    // SAFETY : séquence documentée par Windows. Le bloc rendu par
    // GetClipboardData appartient au système : on le verrouille le temps de
    // le copier, sans jamais lire au-delà de la taille que GlobalSize annonce,
    // puis on le rend.
    unsafe {
        if OpenClipboard(hwnd) == 0 {
            return None;
        }
        let handle = GetClipboardData(CF_UNICODETEXT);
        let mut out = None;
        if !handle.is_null() {
            let src = GlobalLock(handle).cast::<u16>();
            if !src.is_null() {
                let units = GlobalSize(handle) / 2;
                let slice = std::slice::from_raw_parts(src, units);
                let end = slice.iter().position(|u| *u == 0).unwrap_or(units);
                out = String::from_utf16(&slice[..end]).ok();
                GlobalUnlock(handle);
            }
        }
        CloseClipboard();
        out
    }
}

#[link(name = "comdlg32")]
extern "system" {
    fn PrintDlgW(pd: *mut PRINTDLGW) -> BOOL;
    fn GetOpenFileNameW(ofn: *mut OPENFILENAMEW) -> BOOL;
    fn GetSaveFileNameW(ofn: *mut OPENFILENAMEW) -> BOOL;
}

#[link(name = "shell32")]
extern "system" {
    fn DragAcceptFiles(hwnd: HWND, accept: BOOL);
    fn DragQueryFileW(drop: *mut c_void, index: UINT, file: *mut u16, len: UINT) -> UINT;
    fn DragFinish(drop: *mut c_void);
    fn ShellExecuteW(
        hwnd: HWND,
        operation: *const u16,
        file: *const u16,
        parameters: *const u16,
        directory: *const u16,
        show: i32,
    ) -> HINSTANCE;
}

/// Ouvre une adresse web dans le navigateur par défaut. Seuls `http`,
/// `https` et `mailto` sont acceptés : jamais de fichier ni de commande.
fn open_url(hwnd: HWND, url: &str) {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:"))
    {
        return;
    }
    let op = wide("open");
    let target = wide(url);
    // SAFETY : chaînes terminées par 0 vivantes pendant l'appel ; SW_SHOWNORMAL = 1.
    unsafe {
        ShellExecuteW(hwnd, op.as_ptr(), target.as_ptr(), null(), null(), 1);
    }
}

/// Journal de débogage partagé avec le visualiseur (`ACRUX_LOG`) : une ligne
/// par WM_PAINT avec la couleur d'un pixel relue dans le DC, pour distinguer
/// un défaut de peinture d'un défaut de présentation (DWM, capture).
fn debug_log(line: &str) {
    use std::io::Write;
    let Ok(path) = std::env::var("ACRUX_LOG") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// État attaché à la fenêtre.
#[allow(clippy::struct_excessive_bools)] // drapeaux d'actions différées, indépendants
struct WindowState {
    app: Box<dyn App>,
    hwnd: HWND,
    width: u32,
    /// La fenêtre occupe tout l'écran de bureau.
    maximised: bool,
    height: u32,
    /// Tampon BGRA (ligne 0 en haut).
    backbuffer: Vec<u8>,
    pending_title: Option<String>,
    want_redraw: bool,
    want_close: bool,
    dragging: bool,
    /// Pointeurs chargés une fois, dans l'ordre de [`Cursor`].
    cursors: [HCURSOR; Cursor::COUNT],
    /// Pointeur demandé par l'application.
    cursor: Cursor,
    /// Journal de débogage actif (`ACRUX_LOG` défini).
    debug: bool,
    /// Rectangle de la fenêtre avant le passage en plein écran.
    windowed_rect: Option<RECT>,
    /// Mode invisible (`ACRUX_HEADLESS`) : la fenêtre existe mais n'est jamais
    /// affichée, et aucun dialogue système ne s'ouvre. C'est ce qui permet de
    /// piloter l'application par messages sans rien faire apparaître sur
    /// l'écran de la personne qui travaille.
    headless: bool,
}

/// Poignée passée à l'application pendant un événement.
struct Handle<'a> {
    state: &'a mut WindowStateActions,
}

/// Thème demandé pour la décoration : sombre, couleur de barre, couleur du
/// texte.
type FrameTheme = (bool, (u8, u8, u8), (u8, u8, u8));

/// Actions différées (appliquées après le retour de l'application, pour ne
/// pas réentrer dans user32 pendant qu'on emprunte l'état).
#[derive(Default)]
struct WindowStateActions {
    title: Option<String>,
    /// La fenêtre occupe tout l'écran de bureau, à l'instant du message.
    maximised: bool,
    redraw: bool,
    close: bool,
    hwnd: HWND,
    cursor: Option<Cursor>,
    clipboard: Option<String>,
    url: Option<String>,
    fullscreen: Option<bool>,
    frame_theme: Option<FrameTheme>,
}

impl WindowHandle for Handle<'_> {
    fn set_title(&mut self, title: &str) {
        self.state.title = Some(title.to_string());
    }

    fn request_redraw(&mut self) {
        self.state.redraw = true;
    }

    fn open_file_dialog(&mut self) -> Option<PathBuf> {
        if let Some(scripted) = scripted_path("ACRUX_OPEN_FILE") {
            return scripted.answer();
        }
        file_dialog(self.state.hwnd, false, "", PDF_TYPES, "")
    }

    fn save_file_dialog(&mut self, suggested: &str) -> Option<PathBuf> {
        if let Some(scripted) = scripted_save(suggested) {
            return scripted.answer();
        }
        file_dialog(
            self.state.hwnd,
            true,
            suggested,
            PDF_TYPES,
            "Enregistrer le PDF",
        )
    }

    fn save_file_dialog_as(&mut self, suggested: &str, types: &[(&str, &str)]) -> Option<PathBuf> {
        if let Some(scripted) = scripted_save(suggested) {
            return scripted.answer();
        }
        file_dialog(
            self.state.hwnd,
            true,
            suggested,
            types,
            "Exporter le document",
        )
    }

    fn confirm(&mut self, title: &str, message: &str) -> bool {
        if let Some(answer) = scripted_confirm() {
            debug_log(&format!("confirmation « {title} » → {answer}"));
            let _ = message;
            return answer;
        }
        let t = wide(title);
        let m = wide(message);
        // SAFETY : chaînes terminées par 0, vivantes pendant l'appel.
        let r = unsafe {
            MessageBoxW(
                self.state.hwnd,
                m.as_ptr(),
                t.as_ptr(),
                MB_YESNO | MB_ICONQUESTION,
            )
        };
        r == IDYES
    }

    fn show_error(&mut self, title: &str, message: &str) {
        if headless() {
            // Sans écran, une boîte d'erreur bloquerait pour toujours : on la
            // journalise, le script de test la lira.
            debug_log(&format!("erreur « {title} » : {message}"));
            return;
        }
        let t = wide(title);
        let m = wide(message);
        // SAFETY : chaînes terminées par 0, vivantes pendant l'appel.
        unsafe {
            MessageBoxW(self.state.hwnd, m.as_ptr(), t.as_ptr(), MB_ICONERROR);
        }
    }

    fn close(&mut self) {
        self.state.close = true;
    }

    fn set_cursor(&mut self, cursor: Cursor) {
        self.state.cursor = Some(cursor);
    }

    fn maximised(&self) -> bool {
        self.state.maximised
    }

    fn set_clipboard_text(&mut self, text: &str) {
        self.state.clipboard = Some(text.to_string());
    }

    fn clipboard_text(&mut self) -> Option<String> {
        // Un texte copié dans ce même tour n'est pas encore passé au
        // système : on le rend tel quel.
        if let Some(t) = &self.state.clipboard {
            return Some(t.clone());
        }
        if headless() {
            return None;
        }
        clipboard_text(self.state.hwnd)
    }

    fn open_url(&mut self, url: &str) {
        self.state.url = Some(url.to_string());
    }

    fn print(&mut self, title: &str, source: &mut dyn PrintSource) -> PrintOutcome {
        print_document(self.state.hwnd, title, source)
    }

    fn set_fullscreen(&mut self, on: bool) {
        self.state.fullscreen = Some(on);
    }

    fn set_frame_theme(&mut self, dark: bool, caption: (u8, u8, u8), text: (u8, u8, u8)) {
        self.state.frame_theme = Some((dark, caption, text));
    }

    fn waker(&self) -> Box<dyn super::Waker> {
        Box::new(Win32Waker(self.state.hwnd as usize))
    }
}

/// Dialogue d'impression puis envoi des pages au périphérique : chaque page
/// est rendue à la résolution de l'imprimante, ajustée à la zone imprimable
/// (proportions conservées) et centrée.
#[allow(clippy::too_many_lines)] // dialogue, calcul de mise en page, boucle d'envoi
fn print_document(hwnd: HWND, title: &str, source: &mut dyn PrintSource) -> PrintOutcome {
    let count = source.page_count();
    if count == 0 {
        return PrintOutcome::Failed("aucune page".into());
    }
    // Sans dialogue (tests, automatisation) : `ACRUX_PRINTER` nomme l'imprimante et
    // `ACRUX_PRINT_OUTPUT` le fichier de sortie du pilote (ex. « Microsoft Print to PDF »).
    let (hdc, first, last, output) = match std::env::var("ACRUX_PRINTER") {
        Ok(name) => {
            let driver = wide("WINSPOOL");
            let device = wide(&name);
            // SAFETY : chaînes terminées par 0 vivantes pendant l'appel.
            let hdc = unsafe { CreateDCW(driver.as_ptr(), device.as_ptr(), null(), null()) };
            if hdc.is_null() {
                return PrintOutcome::Failed(format!("imprimante « {name} » introuvable"));
            }
            (hdc, 0, count - 1, std::env::var("ACRUX_PRINT_OUTPUT").ok())
        }
        Err(_) => match print_dialog(hwnd, count) {
            Ok(chosen) => (chosen.0, chosen.1, chosen.2, None),
            Err(outcome) => return outcome,
        },
    };
    send_pages(hdc, title, first, last, output.as_deref(), source)
}

/// Dialogue d'impression : contexte de l'imprimante choisie et plage de pages.
fn print_dialog(hwnd: HWND, count: usize) -> Result<(HDC, usize, usize), PrintOutcome> {
    let max_page = u16::try_from(count).unwrap_or(u16::MAX);
    let mut pd = PRINTDLGW {
        lStructSize: std::mem::size_of::<PRINTDLGW>() as DWORD,
        hwndOwner: hwnd,
        hDevMode: null_mut(),
        hDevNames: null_mut(),
        hDC: null_mut(),
        Flags: PD_RETURNDC | PD_NOSELECTION | PD_USEDEVMODECOPIESANDCOLLATE | PD_HIDEPRINTTOFILE,
        nFromPage: 1,
        nToPage: max_page,
        nMinPage: 1,
        nMaxPage: max_page,
        nCopies: 1,
        hInstance: null_mut(),
        lCustData: 0,
        lpfnPrintHook: null(),
        lpfnSetupHook: null(),
        lpPrintTemplateName: null(),
        lpSetupTemplateName: null(),
        hPrintTemplate: null_mut(),
        hSetupTemplate: null_mut(),
    };
    // SAFETY : structure complètement initialisée, vivante pendant l'appel.
    let ok = unsafe { PrintDlgW(&raw mut pd) };
    // SAFETY : les blocs globaux rendus par le dialogue nous appartiennent.
    unsafe {
        if !pd.hDevMode.is_null() {
            GlobalFree(pd.hDevMode);
        }
        if !pd.hDevNames.is_null() {
            GlobalFree(pd.hDevNames);
        }
    }
    if ok == 0 {
        return Err(PrintOutcome::Cancelled);
    }
    let hdc = pd.hDC;
    if hdc.is_null() {
        return Err(PrintOutcome::Failed("aucun contexte d'impression".into()));
    }
    let (first, last) = if pd.Flags & PD_PAGENUMS != 0 {
        (
            usize::from(pd.nFromPage.max(1)) - 1,
            usize::from(pd.nToPage.max(1)).min(count) - 1,
        )
    } else {
        (0, count - 1)
    };
    Ok((hdc, first, last))
}

/// Envoie les pages `first..=last` au contexte d'impression, puis le libère.
#[allow(clippy::too_many_lines)] // mise en page, conversion DIB, boucle d'envoi, libération
fn send_pages(
    hdc: HDC,
    title: &str,
    first: usize,
    last: usize,
    output: Option<&str>,
    source: &mut dyn PrintSource,
) -> PrintOutcome {
    // SAFETY : `hdc` est un DC d'imprimante valide jusqu'à DeleteDC.
    let (res_w, res_h, dpi_x, dpi_y) = unsafe {
        (
            GetDeviceCaps(hdc, HORZRES),
            GetDeviceCaps(hdc, VERTRES),
            GetDeviceCaps(hdc, LOGPIXELSX),
            GetDeviceCaps(hdc, LOGPIXELSY),
        )
    };
    if res_w <= 0 || res_h <= 0 || dpi_x <= 0 || dpi_y <= 0 {
        // SAFETY : DC valide, libéré une seule fois.
        unsafe { DeleteDC(hdc) };
        return PrintOutcome::Failed("imprimante sans zone imprimable".into());
    }
    let doc_name = wide(title);
    let out_name = output.map(wide);
    let info = DOCINFOW {
        cbSize: std::mem::size_of::<DOCINFOW>() as i32,
        lpszDocName: doc_name.as_ptr(),
        lpszOutput: out_name.as_ref().map_or(null(), Vec::as_ptr),
        lpszDatatype: null(),
        fwType: 0,
    };
    // SAFETY : `info` et `doc_name` vivent pendant l'appel.
    if unsafe { StartDocW(hdc, &raw const info) } <= 0 {
        // SAFETY : DC valide, libéré une seule fois.
        unsafe { DeleteDC(hdc) };
        return PrintOutcome::Failed("StartDoc a échoué (impression refusée)".into());
    }
    let mut printed = 0;
    let mut failure: Option<String> = None;
    for index in first..=last {
        let (pw, ph) = source.page_size(index);
        if pw <= 0.0 || ph <= 0.0 {
            continue;
        }
        // Ajustement à la zone imprimable, proportions conservées.
        let scale = (f64::from(res_w) / (pw / 72.0 * f64::from(dpi_x)))
            .min(f64::from(res_h) / (ph / 72.0 * f64::from(dpi_y)))
            .min(1.0);
        let dpi = f64::from(dpi_x) * scale;
        let Some((w, h, rgb)) = source.render(index, dpi) else {
            failure = Some(format!("rendu de la page {} impossible", index + 1));
            break;
        };
        if w == 0 || h == 0 {
            continue;
        }
        // DIB 24 bits, lignes alignées sur 4 octets, de haut en bas (hauteur négative).
        let stride = (w as usize * 3).div_ceil(4) * 4;
        let mut dib = vec![0u8; stride * h as usize];
        for y in 0..h as usize {
            let src = &rgb[y * w as usize * 3..(y + 1) * w as usize * 3];
            let dst = &mut dib[y * stride..y * stride + w as usize * 3];
            for (d, s) in dst.chunks_exact_mut(3).zip(src.chunks_exact(3)) {
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
            }
        }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as DWORD,
                biWidth: w as LONG,
                biHeight: -(h as LONG),
                biPlanes: 1,
                biBitCount: 24,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [0],
        };
        // Taille cible en pixels du périphérique (dpi_y peut différer de dpi_x).
        let dest_w = (pw / 72.0 * f64::from(dpi_x) * scale).round() as i32;
        let dest_h = (ph / 72.0 * f64::from(dpi_y) * scale).round() as i32;
        let x = (res_w - dest_w) / 2;
        let y = (res_h - dest_h) / 2;
        // SAFETY : DC valide ; `dib` et `bmi` vivent pendant les appels.
        let ok = unsafe {
            if StartPage(hdc) <= 0 {
                false
            } else {
                StretchDIBits(
                    hdc,
                    x,
                    y,
                    dest_w,
                    dest_h,
                    0,
                    0,
                    w as i32,
                    h as i32,
                    dib.as_ptr().cast(),
                    &raw const bmi,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
                EndPage(hdc) > 0
            }
        };
        if !ok {
            failure = Some(format!("envoi de la page {} refusé", index + 1));
            break;
        }
        printed += 1;
    }
    // SAFETY : DC valide ; fin ou abandon du document puis libération unique.
    unsafe {
        if failure.is_some() {
            AbortDoc(hdc);
        } else {
            EndDoc(hdc);
        }
        DeleteDC(hdc);
    }
    match failure {
        Some(m) => PrintOutcome::Failed(m),
        None => PrintOutcome::Printed(printed),
    }
}

/// Plein écran : fenêtre sans bordure couvrant le moniteur courant ; la
/// position et le style d'origine sont restaurés à la sortie.
fn set_fullscreen(state: &mut WindowState, on: bool) {
    let hwnd = state.hwnd;
    if on {
        if state.windowed_rect.is_some() {
            return;
        }
        let mut rect = RECT::default();
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as DWORD,
            rcMonitor: RECT::default(),
            rcWork: RECT::default(),
            dwFlags: 0,
        };
        // SAFETY : hwnd valide ; structures initialisées et vivantes pendant les appels.
        unsafe {
            GetWindowRect(hwnd, &raw mut rect);
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            if monitor.is_null() || GetMonitorInfoW(monitor, &raw mut info) == 0 {
                return;
            }
            state.windowed_rect = Some(rect);
            SetWindowLongPtrW(hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE) as LONG_PTR);
            let m = info.rcMonitor;
            SetWindowPos(
                hwnd,
                null_mut(),
                m.left,
                m.top,
                m.right - m.left,
                m.bottom - m.top,
                SWP_NOZORDER | SWP_FRAMECHANGED | SWP_SHOWWINDOW,
            );
        }
    } else if let Some(r) = state.windowed_rect.take() {
        // SAFETY : hwnd valide.
        unsafe {
            SetWindowLongPtrW(
                hwnd,
                GWL_STYLE,
                (WS_OVERLAPPEDWINDOW | WS_VISIBLE) as LONG_PTR,
            );
            SetWindowPos(
                hwnd,
                null_mut(),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_FRAMECHANGED | SWP_SHOWWINDOW,
            );
        }
    }
}

/// Dialogue standard « Ouvrir » ou « Enregistrer sous » (filtre PDF).
const PDF_TYPES: &[(&str, &str)] = &[("Documents PDF", "pdf")];

fn file_dialog(
    hwnd: HWND,
    save: bool,
    suggested: &str,
    types: &[(&str, &str)],
    save_title: &str,
) -> Option<PathBuf> {
    let mut buffer = vec![0u16; 32768];
    if save {
        let name: Vec<u16> = suggested.encode_utf16().take(buffer.len() - 1).collect();
        buffer[..name.len()].copy_from_slice(&name);
    }
    // Un filtre Win32 est une suite de paires terminées par un zéro, le tout
    // clos par un zéro supplémentaire : « libellé\0motif\0…\0 ».
    let mut spec = String::new();
    for (label, ext) in types {
        let _ = write!(spec, "{label} (*.{ext})\0*.{ext}\0");
    }
    spec.push_str("Tous les fichiers\0*.*\0");
    let filter = wide(&spec);
    let title = wide(if save { save_title } else { "Ouvrir un PDF" });
    let default_ext = wide(types.first().map_or("pdf", |t| t.1));
    let flags = if save {
        OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_EXPLORER
    } else {
        OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_EXPLORER
    };
    {
        let mut ofn = OPENFILENAMEW {
            lStructSize: std::mem::size_of::<OPENFILENAMEW>() as DWORD,
            hwndOwner: hwnd,
            hInstance: null_mut(),
            lpstrFilter: filter.as_ptr(),
            lpstrCustomFilter: null_mut(),
            nMaxCustFilter: 0,
            nFilterIndex: 1,
            lpstrFile: buffer.as_mut_ptr(),
            nMaxFile: buffer.len() as DWORD,
            lpstrFileTitle: null_mut(),
            nMaxFileTitle: 0,
            lpstrInitialDir: null(),
            lpstrTitle: title.as_ptr(),
            Flags: flags,
            nFileOffset: 0,
            nFileExtension: 0,
            lpstrDefExt: default_ext.as_ptr(),
            lCustData: 0,
            lpfnHook: null(),
            lpTemplateName: null(),
            pvReserved: null_mut(),
            dwReserved: 0,
            FlagsEx: 0,
        };
        // SAFETY : `ofn` et `buffer` vivent jusqu'à la fin de l'appel ; les
        // chaînes passées sont terminées par 0 et restent vivantes.
        let ok = unsafe {
            if save {
                GetSaveFileNameW(&raw mut ofn)
            } else {
                GetOpenFileNameW(&raw mut ofn)
            }
        };
        if ok == 0 {
            return None;
        }
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(0);
    Some(PathBuf::from(String::from_utf16_lossy(&buffer[..len])))
}

fn modifiers() -> Modifiers {
    // SAFETY : GetKeyState n'a pas de précondition.
    unsafe {
        Modifiers {
            ctrl: GetKeyState(VK_CONTROL) < 0,
            shift: GetKeyState(VK_SHIFT) < 0,
            alt: GetKeyState(VK_MENU) < 0,
        }
    }
}

fn key_from_vk(vk: u32) -> Key {
    match vk {
        0x21 => Key::PageUp,
        0x22 => Key::PageDown,
        0x23 => Key::End,
        0x24 => Key::Home,
        0x25 => Key::Left,
        0x26 => Key::Up,
        0x27 => Key::Right,
        0x28 => Key::Down,
        0x1B => Key::Escape,
        0x0D => Key::Enter,
        0x08 => Key::Backspace,
        0x2E => Key::Delete,
        0x09 => Key::Tab,
        0x20 => Key::Space,
        0x70..=0x7B => Key::F((vk - 0x70 + 1) as u8),
        other => Key::Other(other),
    }
}

fn low_i16(l: LPARAM) -> i32 {
    i32::from((l & 0xFFFF) as u16 as i16)
}

fn high_i16(l: LPARAM) -> i32 {
    i32::from(((l >> 16) & 0xFFFF) as u16 as i16)
}

/// Livre un événement à l'application et applique les actions demandées.
fn deliver(state: &mut WindowState, event: Event) {
    let mut actions = WindowStateActions {
        hwnd: state.hwnd,
        maximised: state.maximised,
        ..Default::default()
    };
    {
        let mut handle = Handle {
            state: &mut actions,
        };
        state.app.event(event, &mut handle);
    }
    if let Some(t) = actions.title {
        state.pending_title = Some(t);
    }
    state.want_redraw |= actions.redraw;
    state.want_close |= actions.close;
    if let Some(c) = actions.cursor {
        if c != state.cursor {
            state.cursor = c;
            // SAFETY : pointeur chargé à la création de la fenêtre.
            unsafe {
                SetCursor(state.cursors[c as usize]);
            }
        }
    }
    if let Some(t) = actions.clipboard {
        set_clipboard_text(state.hwnd, &t);
    }
    if let Some(u) = actions.url {
        open_url(state.hwnd, &u);
    }
    if let Some((dark, caption, text)) = actions.frame_theme {
        apply_frame_theme(state.hwnd, dark, caption, text);
    }
    if let Some(on) = actions.fullscreen {
        set_fullscreen(state, on);
    }
    apply_actions(state);
}

fn apply_actions(state: &mut WindowState) {
    if let Some(t) = state.pending_title.take() {
        let w = wide(&t);
        // SAFETY : hwnd valide (créé par nous, pas encore détruit), chaîne terminée par 0.
        unsafe {
            SetWindowTextW(state.hwnd, w.as_ptr());
        }
    }
    if state.want_redraw {
        state.want_redraw = false;
        if state.headless {
            render(state);
        } else {
            // SAFETY : hwnd valide.
            unsafe {
                InvalidateRect(state.hwnd, null(), 0);
            }
        }
    }
    if state.want_close {
        state.want_close = false;
        // SAFETY : hwnd valide.
        unsafe {
            DestroyWindow(state.hwnd);
        }
    }
}

/// Réponse imposée à un dialogue de fichier, pour les tests sans écran.
enum Scripted {
    /// Le dialogue rend ce chemin.
    Path(PathBuf),
    /// Le dialogue est réputé annulé (variable valant « - »).
    Cancelled,
}

impl Scripted {
    /// Ce que le dialogue doit renvoyer.
    fn answer(self) -> Option<PathBuf> {
        match self {
            Scripted::Path(p) => Some(p),
            Scripted::Cancelled => None,
        }
    }
}

/// Chemin imposé par une variable d'environnement. « - » veut dire
/// « l'utilisateur a annulé ».
fn scripted_path(var: &str) -> Option<Scripted> {
    let value = std::env::var_os(var)?;
    if value == "-" {
        return Some(Scripted::Cancelled);
    }
    Some(Scripted::Path(PathBuf::from(value)))
}

/// Destination imposée pour un enregistrement. `ACRUX_SAVE_DIR` donne un dossier
/// et laisse l'application choisir le nom proposé : c'est ce qu'il faut quand
/// un même script enregistre plusieurs fichiers.
fn scripted_save(suggested: &str) -> Option<Scripted> {
    if let Some(answer) = scripted_path("ACRUX_SAVE_FILE") {
        return Some(answer);
    }
    let dir = std::env::var_os("ACRUX_SAVE_DIR")?;
    let name = if suggested.is_empty() {
        "sortie.pdf"
    } else {
        suggested
    };
    Some(Scripted::Path(PathBuf::from(dir).join(name)))
}

/// Réponse imposée aux confirmations (`ACRUX_CONFIRM` : « oui » ou « non »).
fn scripted_confirm() -> Option<bool> {
    let value = std::env::var("ACRUX_CONFIRM").ok()?;
    Some(matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "oui" | "yes" | "1" | "true" | "o" | "y"
    ))
}

/// Vrai si l'application tourne sans se montrer (`ACRUX_HEADLESS`).
fn headless() -> bool {
    std::env::var_os("ACRUX_HEADLESS").is_some()
}

/// Écrit la poignée de fenêtre dans le fichier `ACRUX_HWND`, s'il est demandé :
/// une fenêtre cachée n'apparaît pas dans `MainWindowHandle`, et un script de
/// test a besoin de cette poignée pour lui poster des messages.
fn publish_hwnd(hwnd: HWND) {
    let Some(path) = std::env::var_os("ACRUX_HWND") else {
        return;
    };
    let value = format!("{}", hwnd as usize);
    let _ = std::fs::write(path, value);
}

/// Dessine dans le tampon, sans rien afficher. C'est aussi ce qui écrit la
/// capture `ACRUX_SHOT`, puisque l'application le fait en fin de peinture.
fn render(state: &mut WindowState) {
    if state.width == 0 || state.height == 0 {
        return;
    }
    let needed = state.width as usize * state.height as usize * 4;
    if state.backbuffer.len() != needed {
        state.backbuffer = vec![0; needed];
    }
    let mut frame = Frame::new(state.width, state.height, &mut state.backbuffer);
    state.app.paint(&mut frame);
}

fn paint(state: &mut WindowState) {
    render(state);
    if state.width == 0 || state.height == 0 {
        return;
    }
    let mut ps = PAINTSTRUCT {
        hdc: null_mut(),
        fErase: 0,
        rcPaint: RECT::default(),
        fRestore: 0,
        fIncUpdate: 0,
        rgbReserved: [0; 32],
    };
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as DWORD,
            biWidth: state.width as LONG,
            // Hauteur négative : lignes de haut en bas (top-down DIB).
            biHeight: -(state.height as LONG),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [0],
    };
    // SAFETY : BeginPaint/EndPaint encadrent l'accès au DC ; le tampon a
    // exactement width × height × 4 octets et vit pendant l'appel.
    unsafe {
        let hdc = BeginPaint(state.hwnd, &raw mut ps);
        if !hdc.is_null() {
            let lines = StretchDIBits(
                hdc,
                0,
                0,
                state.width as i32,
                state.height as i32,
                0,
                0,
                state.width as i32,
                state.height as i32,
                state.backbuffer.as_ptr().cast(),
                &raw const info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            if state.debug {
                // Relecture d'un pixel du DC : vérifie que GDI a bien reçu le tampon.
                let px = GetPixel(hdc, 5, 5);
                let src = &state.backbuffer[(5 * state.width as usize + 5) * 4..][..4];
                debug_log(&format!(
                    "WM_PAINT rcPaint=({},{},{},{}) lignes={lines} tampon(5,5)=BGRA{src:?} dc(5,5)=0x{px:08X}",
                    ps.rcPaint.left, ps.rcPaint.top, ps.rcPaint.right, ps.rcPaint.bottom
                ));
            }
        } else if state.debug {
            debug_log("WM_PAINT sans DC");
        }
        EndPaint(state.hwnd, &raw const ps);
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    msg: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Une panique ne doit jamais traverser la frontière FFI.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handle_message(hwnd, msg, wparam, lparam)
    }));
    match result {
        Ok(r) => r,
        Err(_) => {
            // SAFETY : DefWindowProcW accepte n'importe quel message.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

fn state_ptr(hwnd: HWND) -> *mut WindowState {
    // SAFETY : GWLP_USERDATA est soit 0, soit le pointeur que nous y avons mis.
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WindowState }
}

#[allow(clippy::too_many_lines)]
fn handle_message(hwnd: HWND, msg: UINT, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_CREATE {
        // Le pointeur d'état est passé via CREATESTRUCTW.lpCreateParams (premier champ).
        // SAFETY : lparam est un CREATESTRUCTW valide pendant WM_CREATE ; son
        // premier champ est exactement le `param` donné à CreateWindowExW.
        let create = lparam as *const *mut WindowState;
        let ptr = unsafe { *create };
        if ptr.is_null() {
            return -1;
        }
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, ptr as LONG_PTR);
            (*ptr).hwnd = hwnd;
            DragAcceptFiles(hwnd, 1);
        }
        return 0;
    }
    let ptr = state_ptr(hwnd);
    if ptr.is_null() {
        // SAFETY : voir wndproc.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    // SAFETY : le pointeur a été créé par Box::into_raw dans `run` et n'est
    // libéré qu'à WM_NCDESTROY ; user32 sérialise les messages d'une fenêtre
    // sur son thread, donc aucun autre emprunt n'est vivant ici.
    let state = unsafe { &mut *ptr };
    match msg {
        WM_SIZE => {
            state.maximised = wparam == SIZE_MAXIMIZED;
            let mut rect = RECT::default();
            // SAFETY : hwnd valide, rect vivant.
            unsafe {
                GetClientRect(hwnd, &raw mut rect);
            }
            state.width = (rect.right - rect.left).max(0) as u32;
            state.height = (rect.bottom - rect.top).max(0) as u32;
            deliver(
                state,
                Event::Resize {
                    width: state.width,
                    height: state.height,
                },
            );
            0
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            paint(state);
            0
        }
        WM_KEYDOWN => {
            deliver(state, Event::Key(key_from_vk(wparam as u32), modifiers()));
            0
        }
        WM_CHAR => {
            // Ctrl+lettre arrive comme caractère de contrôle 1..=26 : on le
            // normalise en lettre minuscule avec le modificateur Ctrl, pour que
            // l'application raisonne en raccourcis (« Ctrl+F ») et non en
            // codes ASCII historiques.
            let m = modifiers();
            let code = wparam as u32;
            let normalized = if m.ctrl && (1..=26).contains(&code) {
                char::from_u32(code - 1 + u32::from(b'a'))
            } else {
                char::from_u32(code)
            };
            if let Some(c) = normalized {
                if !c.is_control() {
                    deliver(state, Event::Char(c, m));
                }
            }
            0
        }
        WM_MOUSEWHEEL => {
            let delta = f32::from((wparam >> 16) as u16 as i16) / 120.0;
            deliver(
                state,
                Event::Wheel {
                    delta,
                    x: low_i16(lparam),
                    y: high_i16(lparam),
                    modifiers: modifiers(),
                },
            );
            0
        }
        WM_SETCURSOR if (lparam & 0xFFFF) == HTCLIENT => {
            // SAFETY : pointeur chargé à la création de la fenêtre.
            unsafe {
                SetCursor(state.cursors[state.cursor as usize]);
            }
            1
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            let button = match msg {
                WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => MouseButton::Left,
                WM_RBUTTONDOWN => MouseButton::Right,
                _ => MouseButton::Middle,
            };
            if button == MouseButton::Left {
                state.dragging = true;
            }
            deliver(
                state,
                Event::MouseDown {
                    button,
                    x: low_i16(lparam),
                    y: high_i16(lparam),
                    modifiers: modifiers(),
                    clicks: if msg == WM_LBUTTONDBLCLK { 2 } else { 1 },
                },
            );
            0
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            let button = match msg {
                WM_LBUTTONUP => MouseButton::Left,
                WM_RBUTTONUP => MouseButton::Right,
                _ => MouseButton::Middle,
            };
            if button == MouseButton::Left {
                state.dragging = false;
            }
            deliver(
                state,
                Event::MouseUp {
                    button,
                    x: low_i16(lparam),
                    y: high_i16(lparam),
                },
            );
            0
        }
        WM_MOUSEMOVE => {
            let dragging = wparam & MK_LBUTTON != 0 && state.dragging;
            deliver(
                state,
                Event::MouseMove {
                    x: low_i16(lparam),
                    y: high_i16(lparam),
                    dragging,
                },
            );
            0
        }
        WM_DROPFILES => {
            let drop = wparam as *mut c_void;
            let mut buffer = vec![0u16; 32768];
            // SAFETY : `drop` est le HDROP fourni par le système pour ce message ;
            // le tampon a la taille annoncée ; DragFinish libère la ressource.
            let len = unsafe { DragQueryFileW(drop, 0, buffer.as_mut_ptr(), buffer.len() as UINT) };
            unsafe { DragFinish(drop) };
            if len > 0 {
                let path = PathBuf::from(String::from_utf16_lossy(&buffer[..len as usize]));
                deliver(state, Event::FileDropped(path));
            }
            0
        }
        WM_DPICHANGED => {
            let dpi = (wparam & 0xFFFF) as f32;
            deliver(state, Event::DpiChanged(dpi / 96.0));
            // SAFETY : DefWindowProcW redimensionne la fenêtre selon le rectangle suggéré.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_APP_WAKE => {
            deliver(state, Event::Wake);
            0
        }
        WM_CLOSE => {
            // L'application ferme elle-même (`WindowHandle::close`) après avoir
            // éventuellement demandé confirmation.
            deliver(state, Event::Close);
            0
        }
        WM_DESTROY => {
            // SAFETY : sans précondition.
            unsafe { PostQuitMessage(0) };
            0
        }
        WM_NCDESTROY => {
            // SAFETY : dernier message de la fenêtre ; on reprend possession du Box
            // et on efface le pointeur pour que plus rien ne le déréférence.
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(ptr));
            }
            0
        }
        _ => {
            // SAFETY : voir wndproc.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
    }
}

/// Charge l'icône du programme à la taille demandée par le système.
///
/// L'icône est la ressource ordinale 1 de l'exécutable, incorporée à la
/// compilation par `build.rs`. Si elle manque — exécutable construit sans le
/// dossier `branding/` —, `LoadImageW` rend un pointeur nul et Windows retombe
/// sur son icône par défaut : ce n'est pas une erreur.
fn program_icon(instance: HINSTANCE, metric_x: i32, metric_y: i32) -> HICON {
    // SAFETY : GetSystemMetrics et LoadImageW n'ont pas de précondition ; le
    // nom passé est l'ordinal 1, selon la convention MAKEINTRESOURCE.
    unsafe {
        let cx = GetSystemMetrics(metric_x);
        let cy = GetSystemMetrics(metric_y);
        LoadImageW(
            instance,
            IDI_ACRUX as *const u16,
            IMAGE_ICON,
            cx,
            cy,
            LR_SHARED,
        )
        .cast()
    }
}

/// Accorde la décoration de la fenêtre au thème de l'application.
///
/// Trois attributs, du plus ancien au plus récent : le mode sombre (Windows
/// 10 1809), puis la couleur exacte de la barre et du texte (Windows 11).
/// Chacun est refusé sans dommage par les systèmes qui ne le connaissent
/// pas — on ne teste donc pas la version, on demande et on regarde.
fn apply_frame_theme(hwnd: HWND, dark: bool, caption: (u8, u8, u8), text: (u8, u8, u8)) {
    if hwnd.is_null() {
        return;
    }
    let mode: DWORD = DWORD::from(dark);
    // SAFETY : `hwnd` est la fenêtre de l'application ; chaque attribut reçoit
    // un pointeur vers une valeur de la taille annoncée, valide pendant
    // l'appel. Un attribut inconnu du système rend une erreur, sans effet.
    unsafe {
        let value = (&raw const mode).cast::<c_void>();
        // 20 depuis Windows 10 2004 ; 19 sur les versions 1809-1909.
        if DwmSetWindowAttribute(hwnd, 20, value, 4) != 0 {
            DwmSetWindowAttribute(hwnd, 19, value, 4);
        }
        let colorref = |(r, g, b): (u8, u8, u8)| -> DWORD {
            DWORD::from(r) | (DWORD::from(g) << 8) | (DWORD::from(b) << 16)
        };
        let caption = colorref(caption);
        DwmSetWindowAttribute(hwnd, 35, (&raw const caption).cast::<c_void>(), 4);
        let text = colorref(text);
        DwmSetWindowAttribute(hwnd, 36, (&raw const text).cast::<c_void>(), 4);
        let border = caption;
        DwmSetWindowAttribute(hwnd, 34, (&raw const border).cast::<c_void>(), 4);
    }
}

/// Pointeur dessiné par [`crate::ui::cursors`], à la taille des pointeurs
/// du système.
fn drawn_cursor(shape: Shape) -> Option<HCURSOR> {
    // SAFETY : sans précondition.
    let metric = unsafe { GetSystemMetrics(SM_CXCURSOR) };
    let image = cursors::image(shape, u32::try_from(metric).unwrap_or(32).max(32));
    let side = i32::try_from(image.size).ok()?;
    // Masque monochrome tout à zéro : c'est l'alpha de l'image couleur qui
    // découpe la forme. Une ligne de masque est alignée sur 16 bits.
    let mask = vec![0_u8; (image.size as usize).div_ceil(16) * 2 * image.size as usize];
    // SAFETY : les tampons vivent pendant les appels et ont la taille
    // annoncée (côté × côté × 4 octets pour la couleur, lignes de 16 bits
    // pour le masque) ; CreateIconIndirect copie les bitmaps, qu'on libère
    // ensuite.
    unsafe {
        let color = CreateBitmap(side, side, 1, 32, image.pixels.as_ptr().cast());
        let mask = CreateBitmap(side, side, 1, 1, mask.as_ptr().cast());
        if color.is_null() || mask.is_null() {
            if !color.is_null() {
                DeleteObject(color);
            }
            if !mask.is_null() {
                DeleteObject(mask);
            }
            return None;
        }
        let info = ICONINFO {
            f_icon: 0,
            x_hotspot: image.hot.0,
            y_hotspot: image.hot.1,
            hbm_mask: mask,
            hbm_color: color,
        };
        let cursor = CreateIconIndirect(&raw const info);
        DeleteObject(color);
        DeleteObject(mask);
        (!cursor.is_null()).then_some(cursor)
    }
}

/// Crée la fenêtre principale et fait tourner la boucle de messages.
///
/// # Errors
///
/// Retourne un message si l'enregistrement de la classe de fenêtre ou la
/// création de la fenêtre échoue.
#[allow(clippy::too_many_lines)] // création de la fenêtre puis boucle de messages, linéaire
pub fn run(
    title: &str,
    width: u32,
    height: u32,
    maximised: bool,
    app: Box<dyn App>,
) -> Result<(), String> {
    // Résolution par moniteur (Windows 10 1703+) ; ignoré si absent.
    // SAFETY : sans précondition ; -4 = DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2.
    unsafe {
        SetProcessDpiAwarenessContext(-4);
    }
    let class_name = wide("AcruxMainWindow");
    // SAFETY : null = module courant.
    let instance = unsafe { GetModuleHandleW(null()) };
    // SAFETY : IDC_* sont des ressources système prédéfinies.
    let (arrow, beam, pointer, size_we, size_ns, size_nwse, size_nesw) = unsafe {
        (
            LoadCursorW(null_mut(), IDC_ARROW as *const u16),
            LoadCursorW(null_mut(), IDC_IBEAM as *const u16),
            LoadCursorW(null_mut(), IDC_HAND as *const u16),
            LoadCursorW(null_mut(), IDC_SIZEWE as *const u16),
            LoadCursorW(null_mut(), IDC_SIZENS as *const u16),
            LoadCursorW(null_mut(), IDC_SIZENWSE as *const u16),
            LoadCursorW(null_mut(), IDC_SIZENESW as *const u16),
        )
    };
    let drawn = |shape| drawn_cursor(shape).unwrap_or(arrow);
    let cursors = [
        arrow,
        beam,
        pointer,
        drawn(Shape::AddText),
        drawn(Shape::Highlight),
        drawn(Shape::Note),
        drawn(Shape::Redact),
        drawn(Shape::Move),
        drawn(Shape::Pen),
        drawn(Shape::Place),
        size_we,
        size_ns,
        size_nwse,
        size_nesw,
    ];
    let cursor = cursors[0];
    let icon = program_icon(instance, SM_CXICON, SM_CYICON);
    let icon_small = program_icon(instance, SM_CXSMICON, SM_CYSMICON);
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as UINT,
        style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
        lpfnWndProc: Some(wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: icon,
        hCursor: cursor,
        hbrBackground: null_mut(),
        lpszMenuName: null(),
        lpszClassName: class_name.as_ptr(),
        hIconSm: icon_small,
    };
    // SAFETY : la structure est complètement initialisée et vit pendant l'appel.
    let atom = unsafe { RegisterClassExW(&raw const class) };
    if atom == 0 {
        return Err("RegisterClassExW a échoué".into());
    }
    let state = Box::new(WindowState {
        app,
        hwnd: null_mut(),
        width: 0,
        maximised: false,
        height: 0,
        backbuffer: Vec::new(),
        pending_title: None,
        want_redraw: false,
        want_close: false,
        dragging: false,
        cursors,
        cursor: Cursor::Arrow,
        debug: std::env::var_os("ACRUX_LOG").is_some(),
        windowed_rect: None,
        headless: headless(),
    });
    let state_ptr = Box::into_raw(state);
    let title_w = wide(title);
    // SAFETY : chaînes terminées par 0 ; `state_ptr` est transmis tel quel comme
    // lpCreateParams ; WM_CREATE le lit dans CREATESTRUCTW (premier champ) et
    // l'installe dans GWLP_USERDATA ; il sera libéré à WM_NCDESTROY.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            title_w.as_ptr(),
            if headless() {
                WS_OVERLAPPEDWINDOW
            } else {
                WS_OVERLAPPEDWINDOW | WS_VISIBLE
            },
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            width as i32,
            height as i32,
            null_mut(),
            null_mut(),
            instance,
            state_ptr.cast(),
        )
    };
    if hwnd.is_null() {
        // SAFETY : la fenêtre n'existe pas, personne d'autre ne détient le pointeur.
        unsafe { drop(Box::from_raw(state_ptr)) };
        return Err("CreateWindowExW a échoué".into());
    }
    // Échelle DPI du moniteur d'accueil : la taille demandée est en pixels
    // logiques (96 dpi) ; on agrandit la fenêtre et on prévient l'application
    // avant le premier affichage (WM_DPICHANGED ne couvre que les changements
    // ultérieurs).
    // SAFETY : hwnd valide ; `state_ptr` est installé dans la fenêtre et n'est
    // manipulé que sur ce fil, hors de tout message en cours.
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        if dpi > 0 && dpi != 96 {
            let scale = |v: u32| ((u64::from(v) * u64::from(dpi) + 48) / 96) as i32;
            SetWindowPos(
                hwnd,
                null_mut(),
                0,
                0,
                scale(width),
                scale(height),
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            deliver(&mut *state_ptr, Event::DpiChanged(dpi as f32 / 96.0));
        }
        if !headless() {
            // Rouverte comme on l'a laissée : agrandie, elle le reste.
            ShowWindow(hwnd, if maximised { SW_MAXIMIZE } else { SW_SHOW });
            UpdateWindow(hwnd);
            SetCursor(cursor);
        }
    }
    if headless() {
        // Personne ne nous enverra de WM_PAINT : on peint nous-mêmes, une
        // première fois puis à chaque demande de l'application.
        // SAFETY : `state_ptr` est vivant et n'est pas emprunté ailleurs ici.
        unsafe {
            render(&mut *state_ptr);
        }
        publish_hwnd(hwnd);
    }
    let mut msg = MSG {
        hwnd: null_mut(),
        message: 0,
        wParam: 0,
        lParam: 0,
        time: 0,
        pt: POINT { x: 0, y: 0 },
    };
    loop {
        // SAFETY : `msg` vit pendant l'appel ; GetMessageW retourne -1 en cas d'erreur.
        let r = unsafe { GetMessageW(&raw mut msg, null_mut(), 0, 0) };
        if r <= 0 {
            break;
        }
        // SAFETY : `msg` est initialisé par GetMessageW.
        unsafe {
            TranslateMessage(&raw const msg);
            DispatchMessageW(&raw const msg);
        }
    }
    Ok(())
}
