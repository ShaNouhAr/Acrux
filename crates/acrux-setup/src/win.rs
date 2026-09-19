//! Le strict nécessaire de l'API Windows pour un installateur.
//!
//! Déclaré à la main, comme partout ailleurs dans le projet : fenêtre et
//! contrôles standard, registre, raccourcis (COM `IShellLink`), dossiers
//! connus. Rien d'autre que ce que le système fournit.
//! `unsafe` est autorisé ici : un installateur est du code système par
//! nature (fenêtre, registre, raccourcis COM). Voir CHARTE_PROJET.md §1.2 —
//! c'est, avec `acrux-app/src/platform/`, la seule exception du dépôt, et le
//! moteur PDF n'en contient pas une ligne. Chaque bloc porte l'invariant
//! qu'il garantit.

#![allow(unsafe_code)]
#![allow(non_snake_case, non_camel_case_types)]

use std::ffi::c_void;
use std::path::{Path, PathBuf};

pub type Handle = *mut c_void;
pub type Word = u16;
pub type Dword = u32;
pub type Wparam = usize;
pub type Lparam = isize;
pub type Lresult = isize;

/// Chaîne terminée par zéro, prête pour les API « W ».
#[must_use]
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Chaîne Rust depuis un tampon UTF-16 terminé par zéro.
#[must_use]
pub fn from_wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

// ---------------------------------------------------------------------------
// Fenêtres et contrôles.
// ---------------------------------------------------------------------------

pub const WS_OVERLAPPED: Dword = 0x0000_0000;
pub const WS_CAPTION: Dword = 0x00C0_0000;
pub const WS_SYSMENU: Dword = 0x0008_0000;
pub const WS_MINIMIZEBOX: Dword = 0x0002_0000;
pub const WS_VISIBLE: Dword = 0x1000_0000;
pub const WS_CHILD: Dword = 0x4000_0000;
pub const WS_TABSTOP: Dword = 0x0001_0000;
pub const WS_BORDER: Dword = 0x0080_0000;
pub const BS_AUTOCHECKBOX: Dword = 0x0000_0003;
pub const BS_DEFPUSHBUTTON: Dword = 0x0000_0001;
pub const ES_AUTOHSCROLL: Dword = 0x0000_0080;
pub const SS_LEFT: Dword = 0x0000_0000;

pub const WM_DESTROY: u32 = 0x0002;
pub const WM_PAINT: u32 = 0x000F;
pub const WM_CLOSE: u32 = 0x0010;
pub const WM_COMMAND: u32 = 0x0111;
pub const WM_SETFONT: u32 = 0x0030;

pub const BM_GETCHECK: u32 = 0x00F0;
pub const BM_SETCHECK: u32 = 0x00F1;
pub const WM_GETTEXT: u32 = 0x000D;
pub const WM_GETTEXTLENGTH: u32 = 0x000E;
pub const WM_SETTEXT: u32 = 0x000C;

pub const SW_SHOW: i32 = 5;
pub const MB_ICONERROR: u32 = 0x0000_0010;
pub const MB_YESNO: u32 = 0x0000_0004;
pub const MB_ICONQUESTION: u32 = 0x0000_0020;
pub const IDYES: i32 = 6;
pub const IDC_ARROW: usize = 32512;
pub const IMAGE_ICON: u32 = 1;
pub const LR_DEFAULTSIZE: u32 = 0x0000_0040;
pub const COLOR_WINDOW: usize = 5;

#[repr(C)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
pub struct Msg {
    pub hwnd: Handle,
    pub message: u32,
    pub wparam: Wparam,
    pub lparam: Lparam,
    pub time: Dword,
    pub pt: Point,
}

#[repr(C)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
pub struct PaintStruct {
    pub hdc: Handle,
    pub erase: i32,
    pub paint: Rect,
    pub restore: i32,
    pub inc_update: i32,
    pub reserved: [u8; 32],
}

#[repr(C)]
pub struct WndClassExW {
    pub size: u32,
    pub style: u32,
    pub proc: Option<unsafe extern "system" fn(Handle, u32, Wparam, Lparam) -> Lresult>,
    pub cls_extra: i32,
    pub wnd_extra: i32,
    pub instance: Handle,
    pub icon: Handle,
    pub cursor: Handle,
    pub background: Handle,
    pub menu_name: *const u16,
    pub class_name: *const u16,
    pub icon_sm: Handle,
}

#[link(name = "user32")]
extern "system" {
    pub fn RegisterClassExW(class: *const WndClassExW) -> Word;
    pub fn CreateWindowExW(
        ex_style: Dword,
        class_name: *const u16,
        window_name: *const u16,
        style: Dword,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: Handle,
        menu: Handle,
        instance: Handle,
        param: *mut c_void,
    ) -> Handle;
    pub fn DefWindowProcW(hwnd: Handle, msg: u32, w: Wparam, l: Lparam) -> Lresult;
    pub fn ShowWindow(hwnd: Handle, cmd: i32) -> i32;
    pub fn UpdateWindow(hwnd: Handle) -> i32;
    pub fn GetMessageW(msg: *mut Msg, hwnd: Handle, min: u32, max: u32) -> i32;
    pub fn TranslateMessage(msg: *const Msg) -> i32;
    pub fn DispatchMessageW(msg: *const Msg) -> Lresult;
    pub fn PostQuitMessage(code: i32);
    pub fn DestroyWindow(hwnd: Handle) -> i32;
    pub fn SendMessageW(hwnd: Handle, msg: u32, w: Wparam, l: Lparam) -> Lresult;
    pub fn MessageBoxW(hwnd: Handle, text: *const u16, caption: *const u16, kind: u32) -> i32;
    pub fn LoadCursorW(instance: Handle, name: *const u16) -> Handle;
    pub fn LoadImageW(
        instance: Handle,
        name: *const u16,
        kind: u32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> Handle;
    pub fn SetWindowPos(
        hwnd: Handle,
        after: Handle,
        x: i32,
        y: i32,
        cx: i32,
        cy: i32,
        flags: u32,
    ) -> i32;
    pub fn InvalidateRect(hwnd: Handle, rect: *const Rect, erase: i32) -> i32;
    pub fn BeginPaint(hwnd: Handle, ps: *mut PaintStruct) -> Handle;
    pub fn EndPaint(hwnd: Handle, ps: *const PaintStruct) -> i32;
    pub fn FillRect(hdc: Handle, rect: *const Rect, brush: Handle) -> i32;
    pub fn GetDpiForWindow(hwnd: Handle) -> u32;
    pub fn SetProcessDpiAwarenessContext(context: isize) -> i32;
    pub fn EnableWindow(hwnd: Handle, enable: i32) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    pub fn CreateSolidBrush(color: Dword) -> Handle;
    pub fn DeleteObject(object: Handle) -> i32;
    pub fn CreateFontW(
        height: i32,
        width: i32,
        escapement: i32,
        orientation: i32,
        weight: i32,
        italic: Dword,
        underline: Dword,
        strike: Dword,
        charset: Dword,
        out_precision: Dword,
        clip_precision: Dword,
        quality: Dword,
        pitch: Dword,
        face: *const u16,
    ) -> Handle;
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GetModuleHandleW(name: *const u16) -> Handle;
    pub fn MoveFileExW(from: *const u16, to: *const u16, flags: Dword) -> i32;
}

/// `MOVEFILE_DELAY_UNTIL_REBOOT` : la suppression est notée et exécutée au
/// prochain démarrage.
const MOVEFILE_DELAY_UNTIL_REBOOT: Dword = 4;

/// Demande au système de supprimer un chemin au prochain démarrage.
///
/// C'est la seule façon propre de se débarrasser du programme de
/// désinstallation lui-même : Windows refuse de supprimer un exécutable en
/// cours d'exécution, mais accepte de le noter pour plus tard.
pub fn delete_on_reboot(path: &Path) {
    let from = wide(&path.display().to_string());
    // SAFETY : chemin terminé par zéro ; `to` nul signifie « supprimer ».
    unsafe {
        MoveFileExW(from.as_ptr(), std::ptr::null(), MOVEFILE_DELAY_UNTIL_REBOOT);
    }
}

#[link(name = "shell32")]
extern "system" {
    pub fn ShellExecuteW(
        hwnd: Handle,
        verb: *const u16,
        file: *const u16,
        params: *const u16,
        dir: *const u16,
        show: i32,
    ) -> Handle;
    pub fn SHGetFolderPathW(
        hwnd: Handle,
        folder: i32,
        token: Handle,
        flags: Dword,
        path: *mut u16,
    ) -> i32;
}

/// Identifiants de dossiers connus utilisés par l'installateur.
pub const CSIDL_LOCAL_APPDATA: i32 = 0x001C;
pub const CSIDL_DESKTOPDIRECTORY: i32 = 0x0010;
pub const CSIDL_PROGRAMS: i32 = 0x0002;

/// Chemin d'un dossier connu de l'utilisateur.
#[must_use]
pub fn known_folder(id: i32) -> Option<PathBuf> {
    let mut buffer = [0u16; 320];
    // SAFETY : le tampon a la taille attendue (MAX_PATH) ; l'appel n'écrit
    // pas au-delà.
    let ok = unsafe {
        SHGetFolderPathW(
            std::ptr::null_mut(),
            id,
            std::ptr::null_mut(),
            0,
            buffer.as_mut_ptr(),
        )
    };
    (ok == 0).then(|| PathBuf::from(from_wide(&buffer)))
}

/// Texte d'un contrôle.
#[must_use]
pub fn control_text(hwnd: Handle) -> String {
    // SAFETY : `hwnd` est un contrôle valide ; le tampon est dimensionné sur
    // la longueur que le contrôle annonce.
    unsafe {
        let len = usize::try_from(SendMessageW(hwnd, WM_GETTEXTLENGTH, 0, 0)).unwrap_or(0);
        let mut buffer = vec![0u16; len + 1];
        SendMessageW(
            hwnd,
            WM_GETTEXT,
            buffer.len(),
            buffer.as_mut_ptr() as Lparam,
        );
        from_wide(&buffer)
    }
}

/// Vrai si une case est cochée.
#[must_use]
pub fn is_checked(hwnd: Handle) -> bool {
    // SAFETY : `hwnd` est une case à cocher valide.
    unsafe { SendMessageW(hwnd, BM_GETCHECK, 0, 0) == 1 }
}

/// Affiche une boîte de message d'erreur.
pub fn error_box(hwnd: Handle, message: &str) {
    let text = wide(message);
    let caption = wide("Acrux — installation");
    // SAFETY : les deux chaînes sont terminées par zéro et vivent pendant l'appel.
    unsafe {
        MessageBoxW(hwnd, text.as_ptr(), caption.as_ptr(), MB_ICONERROR);
    }
}

/// Pose une question oui / non.
#[must_use]
pub fn confirm(hwnd: Handle, message: &str) -> bool {
    let text = wide(message);
    let caption = wide("Acrux");
    // SAFETY : idem.
    unsafe {
        MessageBoxW(
            hwnd,
            text.as_ptr(),
            caption.as_ptr(),
            MB_YESNO | MB_ICONQUESTION,
        ) == IDYES
    }
}

/// Ouvre un fichier ou un dossier avec le programme par défaut.
pub fn open(path: &Path) {
    let file = wide(&path.display().to_string());
    let verb = wide("open");
    // SAFETY : chaînes terminées par zéro, vivantes pendant l'appel.
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOW,
        );
    }
}

// ---------------------------------------------------------------------------
// Registre.
// ---------------------------------------------------------------------------

pub const HKEY_CURRENT_USER: Handle = 0x8000_0001_usize as Handle;
pub const KEY_ALL_ACCESS: Dword = 0x000F_003F;
pub const REG_SZ: Dword = 1;
pub const REG_DWORD: Dword = 4;

#[link(name = "advapi32")]
extern "system" {
    pub fn RegCreateKeyExW(
        key: Handle,
        sub: *const u16,
        reserved: Dword,
        class: *const u16,
        options: Dword,
        access: Dword,
        security: *const c_void,
        result: *mut Handle,
        disposition: *mut Dword,
    ) -> i32;
    pub fn RegSetValueExW(
        key: Handle,
        name: *const u16,
        reserved: Dword,
        kind: Dword,
        data: *const u8,
        len: Dword,
    ) -> i32;
    pub fn RegCloseKey(key: Handle) -> i32;
    pub fn RegDeleteTreeW(key: Handle, sub: *const u16) -> i32;
}

/// Écrit des valeurs texte et entières sous une clé de `HKEY_CURRENT_USER`.
///
/// Tout est fait dans la ruche de l'utilisateur : l'installation ne demande
/// jamais les droits d'administrateur, et ne touche rien qui appartienne aux
/// autres comptes de la machine.
pub fn write_key(path: &str, values: &[(&str, Value<'_>)]) -> Result<(), String> {
    let sub = wide(path);
    let mut key: Handle = std::ptr::null_mut();
    // SAFETY : chemin terminé par zéro ; `key` reçoit la poignée ouverte.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_ALL_ACCESS,
            std::ptr::null(),
            &raw mut key,
            std::ptr::null_mut(),
        )
    };
    if status != 0 {
        return Err(format!("registre : création de {path} refusée ({status})"));
    }
    for (name, value) in values {
        let name_w = wide(name);
        let status = match value {
            Value::Text(text) => {
                let data = wide(text);
                let bytes = std::mem::size_of_val(&data[..]);
                // SAFETY : `data` vit pendant l'appel ; `bytes` est sa taille exacte.
                unsafe {
                    RegSetValueExW(
                        key,
                        name_w.as_ptr(),
                        0,
                        REG_SZ,
                        data.as_ptr().cast::<u8>(),
                        u32::try_from(bytes).unwrap_or(0),
                    )
                }
            }
            Value::Number(number) => {
                let data = number.to_ne_bytes();
                // SAFETY : quatre octets, taille annoncée exacte.
                unsafe { RegSetValueExW(key, name_w.as_ptr(), 0, REG_DWORD, data.as_ptr(), 4) }
            }
        };
        if status != 0 {
            // SAFETY : `key` est ouverte.
            unsafe { RegCloseKey(key) };
            return Err(format!("registre : écriture de {name} refusée ({status})"));
        }
    }
    // SAFETY : `key` est ouverte.
    unsafe { RegCloseKey(key) };
    Ok(())
}

/// Valeur de registre.
pub enum Value<'a> {
    /// Chaîne.
    Text(&'a str),
    /// Entier 32 bits.
    Number(u32),
}

/// Supprime une clé et toute sa descendance, sans se plaindre si elle est
/// déjà absente.
pub fn delete_key(path: &str) {
    let sub = wide(path);
    // SAFETY : chemin terminé par zéro.
    unsafe {
        RegDeleteTreeW(HKEY_CURRENT_USER, sub.as_ptr());
    }
}

// ---------------------------------------------------------------------------
// Raccourcis (.lnk) par COM.
// ---------------------------------------------------------------------------

#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

const CLSID_SHELL_LINK: Guid = Guid {
    data1: 0x0002_1401,
    data2: 0,
    data3: 0,
    data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
};
const IID_ISHELL_LINK_W: Guid = Guid {
    data1: 0x0002_14F9,
    data2: 0,
    data3: 0,
    data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
};
const IID_IPERSIST_FILE: Guid = Guid {
    data1: 0x0000_010B,
    data2: 0,
    data3: 0,
    data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46],
};

const CLSCTX_INPROC_SERVER: Dword = 1;
const COINIT_APARTMENTTHREADED: Dword = 2;

#[link(name = "ole32")]
extern "system" {
    fn CoInitializeEx(reserved: *mut c_void, flags: Dword) -> i32;
    fn CoUninitialize();
    fn CoCreateInstance(
        clsid: *const Guid,
        outer: *mut c_void,
        context: Dword,
        iid: *const Guid,
        out: *mut *mut c_void,
    ) -> i32;
}

/// Les entrées de `IShellLinkW` dont l'installateur a besoin.
#[repr(C)]
struct ShellLinkVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_path: unsafe extern "system" fn(*mut c_void, *mut u16, i32, *mut c_void, u32) -> i32,
    get_id_list: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
    set_id_list: unsafe extern "system" fn(*mut c_void, *mut c_void) -> i32,
    get_description: unsafe extern "system" fn(*mut c_void, *mut u16, i32) -> i32,
    set_description: unsafe extern "system" fn(*mut c_void, *const u16) -> i32,
    get_working_directory: unsafe extern "system" fn(*mut c_void, *mut u16, i32) -> i32,
    set_working_directory: unsafe extern "system" fn(*mut c_void, *const u16) -> i32,
    get_arguments: unsafe extern "system" fn(*mut c_void, *mut u16, i32) -> i32,
    set_arguments: unsafe extern "system" fn(*mut c_void, *const u16) -> i32,
    get_hotkey: unsafe extern "system" fn(*mut c_void, *mut u16) -> i32,
    set_hotkey: unsafe extern "system" fn(*mut c_void, u16) -> i32,
    get_show_cmd: unsafe extern "system" fn(*mut c_void, *mut i32) -> i32,
    set_show_cmd: unsafe extern "system" fn(*mut c_void, i32) -> i32,
    get_icon_location: unsafe extern "system" fn(*mut c_void, *mut u16, i32, *mut i32) -> i32,
    set_icon_location: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i32,
    set_relative_path: unsafe extern "system" fn(*mut c_void, *const u16, Dword) -> i32,
    resolve: unsafe extern "system" fn(*mut c_void, Handle, Dword) -> i32,
    set_path: unsafe extern "system" fn(*mut c_void, *const u16) -> i32,
}

/// `IPersistFile`, dont seule `Save` nous intéresse.
#[repr(C)]
struct PersistFileVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_class_id: unsafe extern "system" fn(*mut c_void, *mut Guid) -> i32,
    is_dirty: unsafe extern "system" fn(*mut c_void) -> i32,
    load: unsafe extern "system" fn(*mut c_void, *const u16, Dword) -> i32,
    save: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i32,
}

/// Crée un raccourci `.lnk`.
///
/// # Errors
/// COM indisponible, ou écriture refusée.
pub fn create_shortcut(
    link: &Path,
    target: &Path,
    description: &str,
    icon: Option<&Path>,
) -> Result<(), String> {
    // SAFETY : suite d'appels COM classique. Chaque pointeur est vérifié
    // avant usage, chaque interface obtenue est relâchée, et les chaînes
    // passées vivent jusqu'à la fin de l'appel.
    unsafe {
        let status = CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED);
        // S_OK ou S_FALSE (déjà initialisé) conviennent ; le reste est fatal.
        if status < 0 {
            return Err(format!("COM indisponible ({status:#x})"));
        }
        let mut shell_link: *mut c_void = std::ptr::null_mut();
        let clsid = CLSID_SHELL_LINK;
        let iid = IID_ISHELL_LINK_W;
        let created = CoCreateInstance(
            &raw const clsid,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &raw const iid,
            &raw mut shell_link,
        );
        if created < 0 || shell_link.is_null() {
            CoUninitialize();
            return Err(format!("raccourci : création refusée ({created:#x})"));
        }
        let vtbl = *shell_link.cast::<*const ShellLinkVtbl>();
        let path = wide(&target.display().to_string());
        ((*vtbl).set_path)(shell_link, path.as_ptr());
        let text = wide(description);
        ((*vtbl).set_description)(shell_link, text.as_ptr());
        if let Some(folder) = target.parent() {
            let dir = wide(&folder.display().to_string());
            ((*vtbl).set_working_directory)(shell_link, dir.as_ptr());
        }
        if let Some(icon) = icon {
            let source = wide(&icon.display().to_string());
            ((*vtbl).set_icon_location)(shell_link, source.as_ptr(), 0);
        }
        let mut persist: *mut c_void = std::ptr::null_mut();
        let persist_iid = IID_IPERSIST_FILE;
        let ok = ((*vtbl).query_interface)(shell_link, &raw const persist_iid, &raw mut persist);
        let mut result = Ok(());
        if ok >= 0 && !persist.is_null() {
            let pvtbl = *persist.cast::<*const PersistFileVtbl>();
            let destination = wide(&link.display().to_string());
            let saved = ((*pvtbl).save)(persist, destination.as_ptr(), 1);
            if saved < 0 {
                result = Err(format!("raccourci : enregistrement refusé ({saved:#x})"));
            }
            ((*pvtbl).release)(persist);
        } else {
            result = Err("raccourci : interface d'enregistrement absente".into());
        }
        ((*vtbl).release)(shell_link);
        CoUninitialize();
        result
    }
}
