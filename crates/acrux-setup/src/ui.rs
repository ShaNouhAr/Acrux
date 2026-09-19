//! La fenêtre d'installation : contrôles Windows standard, une barre de
//! progression dessinée à la main, et rien d'autre.
//!
//! Les contrôles sont ceux du système (`BUTTON`, `EDIT`, `STATIC`) : ils
//! parlent aux lecteurs d'écran, suivent le thème et la taille de police de
//! l'utilisateur, et n'obligent pas à réinventer la navigation au clavier.
//! `unsafe` est autorisé ici, comme dans `win.rs` : voir CHARTE_PROJET.md §1.2.

#![allow(unsafe_code)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::many_single_char_names
)]

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::PathBuf;

use crate::install::{self, Options, Step};
use crate::payload::Entry;
use crate::win::{self, Handle, Lparam, Lresult, Wparam};

/// Identifiants des contrôles.
const ID_INSTALL: usize = 1;
const ID_CLOSE: usize = 2;
const ID_FOLDER: usize = 3;
const ID_DESKTOP: usize = 4;
const ID_ASSOCIATE: usize = 5;

/// Ce que la fenêtre a besoin de retenir.
struct State {
    entries: Vec<Entry>,
    version: String,
    folder_edit: Handle,
    desktop_box: Handle,
    associate_box: Handle,
    install_button: Handle,
    close_button: Handle,
    status: Handle,
    progress: f32,
    /// Vrai une fois l'installation réussie : la fenêtre change de rôle.
    done: Option<PathBuf>,
    dpi: f32,
}

thread_local! {
    /// La fenêtre est unique et l'installateur n'a qu'un fil : un état local
    /// au fil suffit, et évite une variable globale modifiable.
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Vrai si l'on tourne en mode invisible.
///
/// Comme l'application (`ACRUX_HEADLESS`), l'installateur sait construire sa
/// fenêtre sans jamais la montrer : c'est ce qui permet de vérifier que tous
/// ses contrôles existent sans rien faire apparaître sur l'écran de qui que
/// ce soit.
fn headless() -> bool {
    std::env::var_os("ACRUX_SETUP_HEADLESS").is_some()
}

/// Ouvre la fenêtre et rend la main quand elle se ferme.
pub fn show(entries: Vec<Entry>, version: &str) {
    // SAFETY : sans précondition ; -4 = par moniteur, version 2.
    unsafe {
        win::SetProcessDpiAwarenessContext(-4);
    }
    let class_name = win::wide("AcruxSetupWindow");
    // SAFETY : null = module courant.
    let instance = unsafe { win::GetModuleHandleW(std::ptr::null()) };
    // SAFETY : la structure est complètement initialisée et vit pendant l'appel.
    let atom = unsafe {
        let class = win::WndClassExW {
            size: std::mem::size_of::<win::WndClassExW>() as u32,
            style: 0,
            proc: Some(wndproc),
            cls_extra: 0,
            wnd_extra: 0,
            instance,
            // MAKEINTRESOURCE(1) : un **ordinal**, pas une adresse — c'est
            // la convention de Windows pour désigner une ressource par son
            // numéro.
            icon: win::LoadImageW(
                instance,
                std::ptr::without_provenance(1),
                win::IMAGE_ICON,
                0,
                0,
                win::LR_DEFAULTSIZE,
            ),
            cursor: win::LoadCursorW(
                std::ptr::null_mut(),
                std::ptr::without_provenance(win::IDC_ARROW),
            ),
            background: std::ptr::without_provenance_mut(win::COLOR_WINDOW + 1),
            menu_name: std::ptr::null(),
            class_name: class_name.as_ptr(),
            icon_sm: std::ptr::null_mut(),
        };
        win::RegisterClassExW(&raw const class)
    };
    if atom == 0 {
        win::error_box(std::ptr::null_mut(), "La fenêtre n'a pas pu être créée.");
        return;
    }

    STATE.with(|cell| {
        *cell.borrow_mut() = Some(State {
            entries,
            version: version.to_string(),
            folder_edit: std::ptr::null_mut(),
            desktop_box: std::ptr::null_mut(),
            associate_box: std::ptr::null_mut(),
            install_button: std::ptr::null_mut(),
            close_button: std::ptr::null_mut(),
            status: std::ptr::null_mut(),
            progress: 0.0,
            done: None,
            dpi: 1.0,
        });
    });

    let title = win::wide(&format!("Installer Acrux {version}"));
    // SAFETY : chaînes terminées par zéro, vivantes pendant l'appel.
    let hwnd = unsafe {
        win::CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            win::WS_OVERLAPPED | win::WS_CAPTION | win::WS_SYSMENU | win::WS_MINIMIZEBOX,
            i32::MIN, // CW_USEDEFAULT
            i32::MIN,
            520,
            360,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null_mut(),
        )
    };
    if hwnd.is_null() {
        win::error_box(std::ptr::null_mut(), "La fenêtre n'a pas pu être créée.");
        return;
    }
    if headless() {
        report();
        // SAFETY : `hwnd` vient d'être créée ; on la détruit sans l'avoir
        // montrée, ce qui met fin à la boucle de messages.
        unsafe { win::DestroyWindow(hwnd) };
    } else {
        // SAFETY : `hwnd` vient d'être créée.
        unsafe {
            win::ShowWindow(hwnd, win::SW_SHOW);
            win::UpdateWindow(hwnd);
        }
    }

    let mut message = std::mem::MaybeUninit::<win::Msg>::uninit();
    // SAFETY : boucle de messages classique ; `GetMessageW` initialise la
    // structure avant qu'on la lise.
    unsafe {
        while win::GetMessageW(message.as_mut_ptr(), std::ptr::null_mut(), 0, 0) > 0 {
            win::TranslateMessage(message.as_ptr());
            win::DispatchMessageW(message.as_ptr());
        }
    }
}

/// Dit ce que la fenêtre contient, pour la vérification en mode invisible.
fn report() {
    STATE.with(|cell| {
        let borrow = cell.borrow();
        let Some(state) = borrow.as_ref() else {
            println!("fenêtre : état absent");
            return;
        };
        let present = |nom: &str, h: Handle| {
            println!("{nom} : {}", if h.is_null() { "absent" } else { "présent" });
        };
        println!("dossier proposé : {}", win::control_text(state.folder_edit));
        present("champ dossier", state.folder_edit);
        present("case Bureau", state.desktop_box);
        present("case association", state.associate_box);
        present("bouton installer", state.install_button);
        present("bouton fermer", state.close_button);
        present("texte d'état", state.status);
        println!(
            "cases cochées : Bureau {}, association {}",
            win::is_checked(state.desktop_box),
            win::is_checked(state.associate_box)
        );
        println!("fichiers à poser : {}", state.entries.len());
    });
}

/// Crée les contrôles de la fenêtre.
#[allow(clippy::too_many_lines)] // une ligne par contrôle, à la suite
fn build(hwnd: Handle) {
    // SAFETY : `hwnd` est la fenêtre en cours de création ; toutes les
    // chaînes passées sont terminées par zéro et vivent pendant l'appel.
    let dpi = unsafe { win::GetDpiForWindow(hwnd) };
    let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
    let s = |v: i32| (v as f32 * scale) as i32;
    // SAFETY : redimensionnement de la fenêtre créée.
    unsafe {
        win::SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            s(520),
            s(380),
            0x0002 | 0x0004,
        );
    }

    let instance = unsafe { win::GetModuleHandleW(std::ptr::null()) };
    let font = unsafe {
        let face = win::wide("Segoe UI");
        win::CreateFontW(-s(12), 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr())
    };
    let title_font = unsafe {
        let face = win::wide("Segoe UI");
        win::CreateFontW(-s(20), 0, 0, 0, 600, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr())
    };

    let make = |class: &str, text: &str, style: u32, x: i32, y: i32, w: i32, h: i32, id: usize| {
        let class_w = win::wide(class);
        let text_w = win::wide(text);
        // SAFETY : chaînes terminées par zéro ; `hwnd` est le parent valide.
        let control = unsafe {
            win::CreateWindowExW(
                0,
                class_w.as_ptr(),
                text_w.as_ptr(),
                win::WS_CHILD | win::WS_VISIBLE | style,
                s(x),
                s(y),
                s(w),
                s(h),
                hwnd,
                std::ptr::without_provenance_mut(id),
                instance,
                std::ptr::null_mut(),
            )
        };
        // SAFETY : `control` vient d'être créé ; `font` est une police valide.
        unsafe {
            win::SendMessageW(control, win::WM_SETFONT, font as Wparam, 1);
        }
        control
    };

    let version = STATE.with(|c| {
        c.borrow()
            .as_ref()
            .map_or_else(String::new, |s| s.version.clone())
    });
    let heading = make("STATIC", "Acrux", win::SS_LEFT, 24, 20, 300, 34, 0);
    // SAFETY : contrôle valide, police valide.
    unsafe {
        win::SendMessageW(heading, win::WM_SETFONT, title_font as Wparam, 1);
    }
    make(
        "STATIC",
        &format!("Lecteur et éditeur PDF — version {version}"),
        win::SS_LEFT,
        24,
        56,
        440,
        20,
        0,
    );
    make(
        "STATIC",
        "Dossier d'installation",
        win::SS_LEFT,
        24,
        98,
        440,
        18,
        0,
    );
    let folder = install::default_folder().display().to_string();
    let folder_edit = make(
        "EDIT",
        &folder,
        win::WS_BORDER | win::WS_TABSTOP | win::ES_AUTOHSCROLL,
        24,
        118,
        464,
        26,
        ID_FOLDER,
    );
    let desktop_box = make(
        "BUTTON",
        "Créer un raccourci sur le Bureau",
        win::BS_AUTOCHECKBOX | win::WS_TABSTOP,
        24,
        158,
        440,
        22,
        ID_DESKTOP,
    );
    let associate_box = make(
        "BUTTON",
        "Proposer Acrux pour ouvrir les fichiers PDF",
        win::BS_AUTOCHECKBOX | win::WS_TABSTOP,
        24,
        184,
        440,
        22,
        ID_ASSOCIATE,
    );
    let status = make(
        "STATIC",
        "Tout se fera dans votre compte : aucun droit d'administrateur n'est demandé.",
        win::SS_LEFT,
        24,
        222,
        464,
        36,
        0,
    );
    let install_button = make(
        "BUTTON",
        "Installer",
        win::BS_DEFPUSHBUTTON | win::WS_TABSTOP,
        286,
        306,
        96,
        32,
        ID_INSTALL,
    );
    let close_button = make(
        "BUTTON",
        "Annuler",
        win::WS_TABSTOP,
        392,
        306,
        96,
        32,
        ID_CLOSE,
    );
    // SAFETY : cases à cocher valides ; 1 = cochée.
    unsafe {
        win::SendMessageW(desktop_box, win::BM_SETCHECK, 1, 0);
        win::SendMessageW(associate_box, win::BM_SETCHECK, 1, 0);
    }

    STATE.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut() {
            state.folder_edit = folder_edit;
            state.desktop_box = desktop_box;
            state.associate_box = associate_box;
            state.install_button = install_button;
            state.close_button = close_button;
            state.status = status;
            state.dpi = scale;
        }
    });
}

/// Lance l'installation et met la fenêtre à jour au fur et à mesure.
fn start(hwnd: Handle) {
    let (options, entries_len) = STATE.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let Some(state) = borrow.as_mut() else {
            return (None, 0);
        };
        let options = Options {
            folder: PathBuf::from(win::control_text(state.folder_edit)),
            desktop: win::is_checked(state.desktop_box),
            associate: win::is_checked(state.associate_box),
        };
        // Pendant l'installation, plus rien ne se clique.
        // SAFETY : contrôles valides.
        unsafe {
            win::EnableWindow(state.install_button, 0);
            win::EnableWindow(state.close_button, 0);
            win::EnableWindow(state.folder_edit, 0);
            win::EnableWindow(state.desktop_box, 0);
            win::EnableWindow(state.associate_box, 0);
        }
        (Some(options), state.entries.len())
    });
    let Some(options) = options else { return };
    if entries_len == 0 {
        return;
    }

    let version = STATE.with(|c| {
        c.borrow()
            .as_ref()
            .map_or_else(String::new, |s| s.version.clone())
    });
    // L'archive est sortie de l'état le temps de l'installation : le rapport
    // d'avancement y accède aussi, et deux emprunts simultanés seraient
    // refusés.
    let entries = STATE.with(|c| {
        c.borrow_mut()
            .as_mut()
            .map_or_else(Vec::new, |s| std::mem::take(&mut s.entries))
    });

    let outcome = install::install(&entries, &options, &version, &mut |step| {
        let Step::Progress(message, done) = step;
        set_status(hwnd, &message, done);
    });

    STATE.with(|c| {
        if let Some(state) = c.borrow_mut().as_mut() {
            state.entries = entries;
        }
    });

    match outcome {
        Ok(()) => finish(hwnd, &options.folder),
        Err(message) => {
            win::error_box(hwnd, &message);
            STATE.with(|cell| {
                if let Some(state) = cell.borrow_mut().as_mut() {
                    // SAFETY : contrôles valides.
                    unsafe {
                        win::EnableWindow(state.install_button, 1);
                        win::EnableWindow(state.close_button, 1);
                        win::EnableWindow(state.folder_edit, 1);
                        win::EnableWindow(state.desktop_box, 1);
                        win::EnableWindow(state.associate_box, 1);
                    }
                    state.progress = 0.0;
                }
            });
            set_status(hwnd, "L'installation n'a pas abouti.", 0.0);
        }
    }
}

/// Met à jour le texte d'avancement et repeint tout de suite.
fn set_status(hwnd: Handle, message: &str, progress: f32) {
    STATE.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut() {
            state.progress = progress;
            let text = win::wide(message);
            // SAFETY : contrôle valide, chaîne terminée par zéro.
            unsafe {
                win::SendMessageW(state.status, win::WM_SETTEXT, 0, text.as_ptr() as Lparam);
            }
        }
    });
    // SAFETY : `hwnd` est la fenêtre ; on force un repaint immédiat pour que
    // la barre avance vraiment sous les yeux de l'utilisateur.
    unsafe {
        win::InvalidateRect(hwnd, std::ptr::null(), 1);
        win::UpdateWindow(hwnd);
    }
}

/// Bascule la fenêtre en page de fin.
fn finish(hwnd: Handle, folder: &std::path::Path) {
    STATE.with(|cell| {
        if let Some(state) = cell.borrow_mut().as_mut() {
            state.done = Some(folder.to_path_buf());
            state.progress = 1.0;
            let launch = win::wide("Lancer Acrux");
            let close = win::wide("Fermer");
            // SAFETY : contrôles valides, chaînes terminées par zéro.
            unsafe {
                win::SendMessageW(
                    state.install_button,
                    win::WM_SETTEXT,
                    0,
                    launch.as_ptr() as Lparam,
                );
                win::SendMessageW(
                    state.close_button,
                    win::WM_SETTEXT,
                    0,
                    close.as_ptr() as Lparam,
                );
                win::EnableWindow(state.install_button, 1);
                win::EnableWindow(state.close_button, 1);
            }
        }
    });
    set_status(
        hwnd,
        "Acrux est installé. Il apparaît dans le menu Démarrer.",
        1.0,
    );
}

/// Dessine la barre de progression.
fn paint(hwnd: Handle) {
    let progress = STATE.with(|c| c.borrow().as_ref().map_or(0.0, |s| s.progress));
    let scale = STATE.with(|c| c.borrow().as_ref().map_or(1.0, |s| s.dpi));
    let s = |v: i32| (v as f32 * scale) as i32;
    let mut ps = std::mem::MaybeUninit::<win::PaintStruct>::uninit();
    // SAFETY : `BeginPaint` initialise la structure ; chaque pinceau créé est
    // détruit avant de rendre la main.
    unsafe {
        let hdc = win::BeginPaint(hwnd, ps.as_mut_ptr());
        let (x, y, w, h) = (s(24), s(272), s(464), s(10));
        let track = win::Rect {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        };
        let grey = win::CreateSolidBrush(0x00E4_E4E4);
        win::FillRect(hdc, &raw const track, grey);
        win::DeleteObject(grey);
        let filled = (w as f32 * progress.clamp(0.0, 1.0)) as i32;
        if filled > 0 {
            let bar = win::Rect {
                left: x,
                top: y,
                right: x + filled,
                bottom: y + h,
            };
            // Le rouge de la marque, en 0x00BBGGRR.
            let accent = win::CreateSolidBrush(0x0028_2BCC);
            win::FillRect(hdc, &raw const bar, accent);
            win::DeleteObject(accent);
        }
        win::EndPaint(hwnd, ps.as_ptr());
    }
}

/// Procédure de fenêtre.
///
/// # Safety
/// Appelée par Windows avec une fenêtre et des paramètres valides.
unsafe extern "system" fn wndproc(
    hwnd: Handle,
    msg: u32,
    wparam: Wparam,
    lparam: Lparam,
) -> Lresult {
    match msg {
        1 => {
            // WM_CREATE
            build(hwnd);
            0
        }
        win::WM_PAINT => {
            paint(hwnd);
            0
        }
        win::WM_COMMAND => {
            let id = wparam & 0xFFFF;
            match id {
                ID_INSTALL => {
                    let done = STATE.with(|c| c.borrow().as_ref().and_then(|s| s.done.clone()));
                    match done {
                        Some(folder) => {
                            win::open(&folder.join("acrux.exe"));
                            // SAFETY : fenêtre valide.
                            unsafe { win::DestroyWindow(hwnd) };
                        }
                        None => start(hwnd),
                    }
                    0
                }
                ID_CLOSE => {
                    // SAFETY : fenêtre valide.
                    unsafe { win::DestroyWindow(hwnd) };
                    0
                }
                _ => 0,
            }
        }
        win::WM_CLOSE => {
            // SAFETY : fenêtre valide.
            unsafe { win::DestroyWindow(hwnd) };
            0
        }
        win::WM_DESTROY => {
            // SAFETY : sans précondition.
            unsafe { win::PostQuitMessage(0) };
            0
        }
        // SAFETY : traitement par défaut, paramètres transmis tels quels.
        _ => unsafe { win::DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Le type de pointeur brut n'a pas besoin d'être exporté.
const _: Option<*mut c_void> = None;
