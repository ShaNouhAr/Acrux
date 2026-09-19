//! Le strict nécessaire de Media Foundation, déclaré à la main.
//!
//! Décoder du H.264 et de l'AAC à la main n'aurait aucun sens : ce sont des
//! normes immenses, brevetées, et le système en a déjà des décodeurs — souvent
//! accélérés par la carte graphique. Acrux fait donc ici ce qu'il fait pour
//! HTTPS : il demande au système. Ce qui reste à notre charge, et qui nous
//! appartient vraiment, c'est ce qu'on fait des images une fois décodées.
//!
//! Tout passe par `IMFSourceReader`, qui est le mode « je tire les images
//! quand j'en veux » de Media Foundation : exactement ce qu'il faut quand
//! c'est notre boucle de dessin qui mène la danse.

use std::ffi::c_void;

/// Identifiant d'interface ou d'attribut.
///
/// Déclarés en `static` et non en `const` : l'API les prend par adresse, et
/// une constante n'en a pas — elle serait recopiée dans un temporaire à
/// chaque usage.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid {
    pub d1: u32,
    pub d2: u16,
    pub d3: u16,
    pub d4: [u8; 8],
}

/// Écrit un GUID sous la forme habituelle `{d1-d2-d3-d4...}`.
const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Guid {
    Guid { d1, d2, d3, d4 }
}

/// Les types de média de Media Foundation dérivent tous du même modèle
/// `{xxxxxxxx-0000-0010-8000-00AA00389B71}`, hérité de DirectShow.
const fn media_subtype(fourcc: u32) -> Guid {
    guid(
        fourcc,
        0x0000,
        0x0010,
        [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
    )
}

pub static MF_MEDIATYPE_VIDEO: Guid = media_subtype(0x7364_6976); // « vids »
pub static MF_MEDIATYPE_AUDIO: Guid = media_subtype(0x7364_7561); // « auds »
/// `D3DFMT_X8R8G8B8` : quatre octets par pixel, bleu-vert-rouge-inutilisé.
pub static MF_VIDEOFORMAT_RGB32: Guid = media_subtype(22);
pub static MF_AUDIOFORMAT_PCM: Guid = media_subtype(1);

pub static MF_MT_MAJOR_TYPE: Guid = guid(
    0x48EB_A18E,
    0xF8C9,
    0x4687,
    [0xBF, 0x11, 0x0A, 0x74, 0xC9, 0xF9, 0x6A, 0x8F],
);
pub static MF_MT_SUBTYPE: Guid = guid(
    0xF7E3_4C9A,
    0x42E8,
    0x4714,
    [0xB7, 0x4B, 0xCB, 0x29, 0xD7, 0x2C, 0x35, 0xE5],
);
pub static MF_MT_FRAME_SIZE: Guid = guid(
    0x1652_C33D,
    0xD6B2,
    0x4012,
    [0xB8, 0x34, 0x72, 0x03, 0x08, 0x49, 0xA3, 0x7D],
);
pub static MF_MT_DEFAULT_STRIDE: Guid = guid(
    0x644B_4E48,
    0x1E02,
    0x4516,
    [0xB0, 0xEB, 0xC0, 0x1C, 0xA9, 0xD4, 0x9A, 0xC6],
);
pub static MF_MT_AUDIO_NUM_CHANNELS: Guid = guid(
    0x37E4_8BF5,
    0x645E,
    0x4C5B,
    [0x89, 0xDE, 0xAD, 0xA9, 0xE2, 0x9B, 0x69, 0x6A],
);
pub static MF_MT_AUDIO_SAMPLES_PER_SECOND: Guid = guid(
    0x5FAE_EAE7,
    0x0290,
    0x4C31,
    [0x9E, 0x8A, 0xC5, 0x34, 0xF6, 0x8D, 0x9D, 0xBA],
);
pub static MF_MT_AUDIO_BITS_PER_SAMPLE: Guid = guid(
    0xF2DE_B57F,
    0x40FA,
    0x4764,
    [0xAA, 0x33, 0xED, 0x4F, 0x2D, 0x1F, 0xF6, 0x69],
);
pub static MF_PD_DURATION: Guid = guid(
    0x6C99_0D33,
    0xBB8E,
    0x477A,
    [0x85, 0x98, 0x0D, 0x5D, 0x96, 0xFC, 0xD8, 0x8A],
);
pub static MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING: Guid = guid(
    0xFB39_4F3D,
    0xCCF1,
    0x42EE,
    [0xBB, 0xB3, 0xF9, 0xB8, 0x45, 0xD5, 0x68, 0x1D],
);
pub static GUID_NULL: Guid = guid(0, 0, 0, [0; 8]);

/// Flux vidéo, flux audio, et la source elle-même.
pub const FIRST_VIDEO_STREAM: u32 = 0xFFFF_FFFC;
pub const FIRST_AUDIO_STREAM: u32 = 0xFFFF_FFFD;
pub const ALL_STREAMS: u32 = 0xFFFF_FFFE;
pub const MEDIASOURCE: u32 = 0xFFFF_FFFF;

/// Le flux est arrivé à sa fin.
pub const END_OF_STREAM: u32 = 0x0000_0002;
/// Démarrage allégé : pas de sockets ni de support réseau.
const MFSTARTUP_LITE: u32 = 1;
/// Version de l'API attendue.
const MF_VERSION: u32 = 0x0002_0070;

#[link(name = "mfplat")]
extern "system" {
    fn MFStartup(version: u32, flags: u32) -> i32;
    fn MFCreateAttributes(out: *mut *mut c_void, initial: u32) -> i32;
    fn MFCreateMediaType(out: *mut *mut c_void) -> i32;
}

#[link(name = "mfreadwrite")]
extern "system" {
    fn MFCreateSourceReaderFromURL(
        url: *const u16,
        attributes: *mut c_void,
        out: *mut *mut c_void,
    ) -> i32;
}

/// Démarre Media Foundation une fois pour toute la durée du programme.
///
/// `MFShutdown` n'est jamais appelé : le système le fait à la fin du
/// processus, et un arrêt anticipé pendant qu'un lecteur vit encore ferait
/// planter les objets qui en dépendent.
pub fn startup() -> Result<(), String> {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    static mut OUTCOME: i32 = 0;
    // SAFETY : `Once` garantit un seul passage, et la lecture qui suit a lieu
    // après lui ; personne d'autre n'écrit `OUTCOME`.
    unsafe {
        ONCE.call_once(|| {
            OUTCOME = MFStartup(MF_VERSION, MFSTARTUP_LITE);
        });
        if OUTCOME < 0 {
            return Err(format!(
                "Media Foundation indisponible ({:#010x})",
                OUTCOME as u32
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Les quelques interfaces COM dont nous avons besoin.
//
// Une interface COM est un pointeur vers un pointeur de table de fonctions.
// Ces tables sont recopiées ici **dans l'ordre exact** de leur déclaration
// d'origine : une entrée oubliée ne se voit pas à la compilation et se paie
// par un appel dans le vide.
// ---------------------------------------------------------------------------

/// Les trois entrées de `IUnknown`, en tête de toutes les autres.
#[repr(C)]
pub struct UnknownVtbl {
    pub query_interface:
        unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
    pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    pub release: unsafe extern "system" fn(*mut c_void) -> u32,
}

/// `IMFAttributes` : trente entrées, dont quatre nous servent.
#[repr(C)]
pub struct AttributesVtbl {
    pub base: UnknownVtbl,
    pub get_item: usize,
    pub get_item_type: usize,
    pub compare_item: usize,
    pub compare: usize,
    pub get_uint32: unsafe extern "system" fn(*mut c_void, *const Guid, *mut u32) -> i32,
    pub get_uint64: unsafe extern "system" fn(*mut c_void, *const Guid, *mut u64) -> i32,
    pub get_double: usize,
    pub get_guid: unsafe extern "system" fn(*mut c_void, *const Guid, *mut Guid) -> i32,
    pub get_string_length: usize,
    pub get_string: usize,
    pub get_allocated_string: usize,
    pub get_blob_size: usize,
    pub get_blob: usize,
    pub get_allocated_blob: usize,
    pub get_unknown: usize,
    pub set_item: usize,
    pub delete_item: usize,
    pub delete_all_items: usize,
    pub set_uint32: unsafe extern "system" fn(*mut c_void, *const Guid, u32) -> i32,
    pub set_uint64: unsafe extern "system" fn(*mut c_void, *const Guid, u64) -> i32,
    pub set_double: usize,
    pub set_guid: unsafe extern "system" fn(*mut c_void, *const Guid, *const Guid) -> i32,
    pub set_string: usize,
    pub set_blob: usize,
    pub set_unknown: usize,
    pub lock_store: usize,
    pub unlock_store: usize,
    pub get_count: usize,
    pub get_item_by_index: usize,
    pub copy_all_items: usize,
}

/// `IMFSourceReader`. N'hérite **pas** de `IMFAttributes`.
#[repr(C)]
pub struct SourceReaderVtbl {
    pub base: UnknownVtbl,
    pub get_stream_selection: usize,
    pub set_stream_selection: unsafe extern "system" fn(*mut c_void, u32, i32) -> i32,
    pub get_native_media_type: usize,
    pub get_current_media_type:
        unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void) -> i32,
    pub set_current_media_type:
        unsafe extern "system" fn(*mut c_void, u32, *mut u32, *mut c_void) -> i32,
    pub set_current_position:
        unsafe extern "system" fn(*mut c_void, *const Guid, *const PropVariant) -> i32,
    pub read_sample: unsafe extern "system" fn(
        *mut c_void,
        u32,
        u32,
        *mut u32,
        *mut u32,
        *mut i64,
        *mut *mut c_void,
    ) -> i32,
    pub flush: unsafe extern "system" fn(*mut c_void, u32) -> i32,
    pub get_service_for_stream: usize,
    pub get_presentation_attribute:
        unsafe extern "system" fn(*mut c_void, u32, *const Guid, *mut PropVariant) -> i32,
}

/// `IMFSample`, qui prolonge `IMFAttributes`.
#[repr(C)]
pub struct SampleVtbl {
    pub attributes: AttributesVtbl,
    pub get_sample_flags: usize,
    pub set_sample_flags: usize,
    pub get_sample_time: unsafe extern "system" fn(*mut c_void, *mut i64) -> i32,
    pub set_sample_time: usize,
    pub get_sample_duration: unsafe extern "system" fn(*mut c_void, *mut i64) -> i32,
    pub set_sample_duration: usize,
    pub get_buffer_count: usize,
    pub get_buffer_by_index: usize,
    pub convert_to_contiguous_buffer:
        unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
    pub add_buffer: usize,
    pub remove_buffer_by_index: usize,
    pub remove_all_buffers: usize,
    pub get_total_length: usize,
    pub copy_to_buffer: usize,
}

/// `IMFMediaBuffer`.
#[repr(C)]
pub struct MediaBufferVtbl {
    pub base: UnknownVtbl,
    pub lock: unsafe extern "system" fn(*mut c_void, *mut *mut u8, *mut u32, *mut u32) -> i32,
    pub unlock: unsafe extern "system" fn(*mut c_void) -> i32,
    pub get_current_length: unsafe extern "system" fn(*mut c_void, *mut u32) -> i32,
    pub set_current_length: usize,
    pub get_max_length: usize,
}

/// `PROPVARIANT`, réduit à ce qu'on en lit : un type et huit octets.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PropVariant {
    pub vt: u16,
    reserved: [u16; 3],
    pub value: u64,
}

impl PropVariant {
    /// Un entier 64 bits signé, tel que `SetCurrentPosition` l'attend.
    #[must_use]
    pub fn from_i64(value: i64) -> Self {
        PropVariant {
            // VT_I8.
            vt: 20,
            reserved: [0; 3],
            #[allow(clippy::cast_sign_loss)] // réinterprétation, pas conversion
            value: value as u64,
        }
    }

    /// Un entier vide.
    #[must_use]
    pub fn empty() -> Self {
        PropVariant {
            vt: 0,
            reserved: [0; 3],
            value: 0,
        }
    }
}

/// Pointeur COM qui relâche sa référence en sortant de portée.
pub struct Com(pub *mut c_void);

impl Com {
    /// Vrai si le pointeur est nul.
    #[must_use]
    pub fn is_null(&self) -> bool {
        self.0.is_null()
    }

    /// La table de fonctions, vue comme celle de `T`.
    ///
    /// # Safety
    /// `T` doit être la table de l'interface réellement portée par ce
    /// pointeur, avec ses entrées dans le bon ordre.
    pub unsafe fn vtbl<T>(&self) -> *const T {
        // SAFETY : un pointeur d'interface COM pointe sur sa table.
        unsafe { *self.0.cast::<*const T>() }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0.is_null() {
            return;
        }
        // SAFETY : `IUnknown::Release` est la troisième entrée de toute
        // table COM ; la référence n'est relâchée qu'ici.
        unsafe {
            let vtbl = *self.0.cast::<*const UnknownVtbl>();
            ((*vtbl).release)(self.0);
        }
        self.0 = std::ptr::null_mut();
    }
}

/// Crée un jeu d'attributs vide.
pub fn create_attributes(initial: u32) -> Result<Com, String> {
    let mut out: *mut c_void = std::ptr::null_mut();
    // SAFETY : `out` reçoit l'objet créé ; il est confié à `Com`.
    let hr = unsafe { MFCreateAttributes(&raw mut out, initial) };
    check(hr, "création des attributs")?;
    Ok(Com(out))
}

/// Crée un type de média vide.
pub fn create_media_type() -> Result<Com, String> {
    let mut out: *mut c_void = std::ptr::null_mut();
    // SAFETY : idem.
    let hr = unsafe { MFCreateMediaType(&raw mut out) };
    check(hr, "création du type de média")?;
    Ok(Com(out))
}

/// Ouvre un fichier et rend son lecteur.
pub fn create_source_reader(path: &std::path::Path, attributes: &Com) -> Result<Com, String> {
    let url: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut out: *mut c_void = std::ptr::null_mut();
    // SAFETY : `url` est terminée par zéro et vit pendant l'appel ; `out`
    // reçoit l'objet, confié à `Com`.
    let hr = unsafe { MFCreateSourceReaderFromURL(url.as_ptr(), attributes.0, &raw mut out) };
    check(hr, "ouverture du média")?;
    Ok(Com(out))
}

use std::os::windows::ffi::OsStrExt as _;

/// Transforme un `HRESULT` en message lisible.
pub fn check(hr: i32, quoi: &str) -> Result<(), String> {
    if hr >= 0 {
        return Ok(());
    }
    #[allow(clippy::cast_sign_loss)] // un HRESULT se lit en hexadécimal
    let code = hr as u32;
    let explication = match code {
        0xC00D_36C4 => " (format non reconnu)",
        0xC00D_36B4 => " (le flux ne peut pas être converti dans ce format)",
        0x8007_0002 => " (fichier introuvable)",
        0xC00D_36E6 => " (aucun décodeur pour ce format)",
        _ => "",
    };
    Err(format!("{quoi} : échec {code:#010x}{explication}"))
}
