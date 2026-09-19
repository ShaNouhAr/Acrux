//! Lecture des vidéos et des sons d'un PDF.
//!
//! Le décodage est confié au système (voir [`mf`]) ; ce qui nous appartient,
//! c'est **ce qu'on fait des images**. Elles arrivent ici en BGRA dans un
//! tampon ordinaire, que le visualiseur compose dans sa page comme n'importe
//! quel dessin : la vidéo suit le défilement, le zoom et la découpe, sans
//! fenêtre flottante par-dessus. C'est ce qui manque à beaucoup de lecteurs,
//! et qui se voit tout de suite quand on fait défiler la page.
//!
//! # Qui donne l'heure
//!
//! Le son est le maître du temps. C'est contre-intuitif mais nécessaire :
//! l'oreille entend un hoquet de vingt millisecondes, l'œil ne voit pas une
//! image de retard. La position de lecture est donc celle que la carte son
//! déclare avoir jouée, et les images sont choisies pour y correspondre.
//! Sans piste audio, l'horloge de la machine prend le relais.

#![allow(unsafe_code)]

mod mf;

use std::ffi::c_void;
use std::path::Path;
use std::time::Instant;

use mf::{Com, PropVariant};

mod audio;

/// Une image décodée, prête à composer.
pub struct Frame {
    /// Largeur en pixels.
    pub width: u32,
    /// Hauteur en pixels.
    pub height: u32,
    /// Pixels, quatre octets par pixel dans l'ordre bleu, vert, rouge, inutilisé.
    pub bgra: Vec<u8>,
    /// Instant du média auquel cette image appartient, en secondes.
    pub time: f64,
}

/// Où en est la lecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// À l'arrêt, au début ou après une pause.
    Paused,
    /// En cours.
    Playing,
    /// Arrivé au bout.
    Ended,
}

/// Un média ouvert.
pub struct Player {
    reader: Com,
    /// Dimensions de la vidéo, ou `None` pour un média sans image.
    size: Option<(u32, u32)>,
    /// Nombre d'octets par ligne dans les images décodées.
    stride: usize,
    /// Durée totale en secondes, `0` si le média ne la déclare pas.
    duration: f64,
    state: State,
    /// Image affichée, et celle décodée d'avance qui attend son tour.
    current: Option<Frame>,
    pending: Option<Frame>,
    /// Position du média au dernier démarrage, et l'instant correspondant.
    anchor_time: f64,
    anchor_instant: Instant,
    /// Sortie audio, quand le média a du son.
    audio: Option<audio::Output>,
    /// Le flux vidéo est épuisé.
    video_done: bool,
    /// Le flux audio est épuisé.
    audio_done: bool,
    /// Les images arrivent du bas vers le haut (pas de ligne négatif).
    flipped: bool,
}

/// Cent nanosecondes : l'unité de temps de Media Foundation.
const TICKS: f64 = 10_000_000.0;

impl Player {
    /// Ouvre un fichier média.
    ///
    /// # Errors
    /// Media Foundation indisponible, format inconnu, fichier illisible, ou
    /// média sans piste utilisable.
    pub fn open(path: &Path) -> Result<Player, String> {
        mf::startup()?;
        let attributes = mf::create_attributes(1)?;
        // Laisse Media Foundation insérer le convertisseur qu'il faut pour
        // nous rendre du BGRA, quel que soit le format d'origine.
        // SAFETY : `attributes` est un `IMFAttributes` valide.
        unsafe {
            let vtbl = attributes.vtbl::<mf::AttributesVtbl>();
            ((*vtbl).set_uint32)(
                attributes.0,
                &raw const mf::MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING,
                1,
            );
        }
        let reader = mf::create_source_reader(path, &attributes)?;

        let mut player = Player {
            reader,
            size: None,
            stride: 0,
            duration: 0.0,
            state: State::Paused,
            current: None,
            pending: None,
            anchor_time: 0.0,
            anchor_instant: Instant::now(),
            audio: None,
            video_done: false,
            audio_done: false,
            flipped: false,
        };
        player.duration = player.read_duration();
        let video = player.setup_video();
        let audio = player.setup_audio();
        if video.is_err() && audio.is_err() {
            return Err(format!(
                "aucune piste lisible : {}",
                video.err().unwrap_or_default()
            ));
        }
        Ok(player)
    }

    /// Prépare la sortie vidéo en BGRA et retient ses dimensions.
    fn setup_video(&mut self) -> Result<(), String> {
        let wanted = mf::create_media_type()?;
        // SAFETY : `wanted` est un `IMFMediaType` neuf ; les GUID pointés
        // vivent pour toute la durée du programme.
        unsafe {
            let vtbl = wanted.vtbl::<mf::AttributesVtbl>();
            ((*vtbl).set_guid)(
                wanted.0,
                &raw const mf::MF_MT_MAJOR_TYPE,
                &raw const mf::MF_MEDIATYPE_VIDEO,
            );
            ((*vtbl).set_guid)(
                wanted.0,
                &raw const mf::MF_MT_SUBTYPE,
                &raw const mf::MF_VIDEOFORMAT_RGB32,
            );
        }
        // SAFETY : `reader` est un `IMFSourceReader` valide.
        let hr = unsafe {
            let vtbl = self.reader.vtbl::<mf::SourceReaderVtbl>();
            ((*vtbl).set_current_media_type)(
                self.reader.0,
                mf::FIRST_VIDEO_STREAM,
                std::ptr::null_mut(),
                wanted.0,
            )
        };
        mf::check(hr, "sortie vidéo en BGRA")?;

        // Le type réellement obtenu donne les dimensions et le pas de ligne.
        let mut actual: *mut c_void = std::ptr::null_mut();
        // SAFETY : `actual` reçoit le type courant, confié à `Com`.
        let hr = unsafe {
            let vtbl = self.reader.vtbl::<mf::SourceReaderVtbl>();
            ((*vtbl).get_current_media_type)(self.reader.0, mf::FIRST_VIDEO_STREAM, &raw mut actual)
        };
        mf::check(hr, "lecture du type vidéo")?;
        let actual = Com(actual);
        // SAFETY : `actual` est un `IMFMediaType` valide.
        let (packed, stride) = unsafe {
            let vtbl = actual.vtbl::<mf::AttributesVtbl>();
            let mut size = 0u64;
            ((*vtbl).get_uint64)(actual.0, &raw const mf::MF_MT_FRAME_SIZE, &raw mut size);
            let mut stride = 0u32;
            ((*vtbl).get_uint32)(
                actual.0,
                &raw const mf::MF_MT_DEFAULT_STRIDE,
                &raw mut stride,
            );
            (size, stride)
        };
        // `MF_MT_FRAME_SIZE` empaquette la largeur en haut et la hauteur en bas.
        let width = u32::try_from(packed >> 32).unwrap_or(0);
        let height = u32::try_from(packed & 0xFFFF_FFFF).unwrap_or(0);
        if width == 0 || height == 0 {
            return Err(String::from("vidéo sans dimensions"));
        }
        // Un pas négatif annonce une image à l'envers : c'est l'héritage des
        // bitmaps Windows, qui se lisent du bas vers le haut.
        #[allow(clippy::cast_possible_wrap)] // relecture d'un i32 rangé en u32
        let signed = stride as i32;
        self.stride = if signed == 0 {
            width as usize * 4
        } else {
            signed.unsigned_abs() as usize
        };
        self.flipped = signed < 0;
        self.size = Some((width, height));
        Ok(())
    }

    /// Prépare la sortie audio, si le média a une piste sonore.
    fn setup_audio(&mut self) -> Result<(), String> {
        let wanted = mf::create_media_type()?;
        // SAFETY : type neuf, GUID statiques.
        unsafe {
            let vtbl = wanted.vtbl::<mf::AttributesVtbl>();
            ((*vtbl).set_guid)(
                wanted.0,
                &raw const mf::MF_MT_MAJOR_TYPE,
                &raw const mf::MF_MEDIATYPE_AUDIO,
            );
            ((*vtbl).set_guid)(
                wanted.0,
                &raw const mf::MF_MT_SUBTYPE,
                &raw const mf::MF_AUDIOFORMAT_PCM,
            );
        }
        // SAFETY : lecteur valide.
        let hr = unsafe {
            let vtbl = self.reader.vtbl::<mf::SourceReaderVtbl>();
            ((*vtbl).set_current_media_type)(
                self.reader.0,
                mf::FIRST_AUDIO_STREAM,
                std::ptr::null_mut(),
                wanted.0,
            )
        };
        mf::check(hr, "sortie audio en PCM")?;

        let mut actual: *mut c_void = std::ptr::null_mut();
        // SAFETY : `actual` reçoit le type courant, confié à `Com`.
        let hr = unsafe {
            let vtbl = self.reader.vtbl::<mf::SourceReaderVtbl>();
            ((*vtbl).get_current_media_type)(self.reader.0, mf::FIRST_AUDIO_STREAM, &raw mut actual)
        };
        mf::check(hr, "lecture du type audio")?;
        let actual = Com(actual);
        // SAFETY : `actual` est un `IMFMediaType` valide.
        let (channels, rate, bits) = unsafe {
            let vtbl = actual.vtbl::<mf::AttributesVtbl>();
            let read = |key: &mf::Guid| {
                let mut v = 0u32;
                ((*vtbl).get_uint32)(actual.0, std::ptr::from_ref(key), &raw mut v);
                v
            };
            (
                read(&mf::MF_MT_AUDIO_NUM_CHANNELS),
                read(&mf::MF_MT_AUDIO_SAMPLES_PER_SECOND),
                read(&mf::MF_MT_AUDIO_BITS_PER_SAMPLE),
            )
        };
        if channels == 0 || rate == 0 {
            return Err(String::from("audio sans format"));
        }
        self.audio = Some(audio::Output::open(channels, rate, bits.max(16))?);
        Ok(())
    }

    /// Durée déclarée par le média, en secondes.
    fn read_duration(&self) -> f64 {
        let mut value = PropVariant::empty();
        // SAFETY : `value` est une `PROPVARIANT` initialisée ; l'appel la
        // remplit ou la laisse vide.
        let hr = unsafe {
            let vtbl = self.reader.vtbl::<mf::SourceReaderVtbl>();
            ((*vtbl).get_presentation_attribute)(
                self.reader.0,
                mf::MEDIASOURCE,
                &raw const mf::MF_PD_DURATION,
                &raw mut value,
            )
        };
        if hr < 0 {
            return 0.0;
        }
        #[allow(clippy::cast_precision_loss)] // durée en centaines de nanosecondes
        let seconds = value.value as f64 / TICKS;
        seconds.max(0.0)
    }

    /// Dimensions de l'image, ou `None` pour un média sans vidéo.
    #[must_use]
    pub fn size(&self) -> Option<(u32, u32)> {
        self.size
    }

    /// Durée totale en secondes.
    #[must_use]
    pub fn duration(&self) -> f64 {
        self.duration
    }

    /// État de lecture.
    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    /// Position de lecture en secondes.
    #[must_use]
    pub fn position(&self) -> f64 {
        let position = match (&self.audio, self.state) {
            // Le son donne l'heure quand il joue.
            (Some(out), State::Playing) => self.anchor_time + out.played_seconds(),
            (_, State::Playing) => self.anchor_time + self.anchor_instant.elapsed().as_secs_f64(),
            _ => self.anchor_time,
        };
        if self.duration > 0.0 {
            position.clamp(0.0, self.duration)
        } else {
            position.max(0.0)
        }
    }

    /// Démarre ou reprend la lecture.
    pub fn play(&mut self) {
        if self.state == State::Playing {
            return;
        }
        if self.state == State::Ended {
            self.seek(0.0);
        }
        self.anchor_time = self.position();
        self.anchor_instant = Instant::now();
        if let Some(out) = &mut self.audio {
            out.start();
        }
        self.state = State::Playing;
    }

    /// Met en pause.
    pub fn pause(&mut self) {
        if self.state != State::Playing {
            return;
        }
        self.anchor_time = self.position();
        if let Some(out) = &mut self.audio {
            out.stop();
        }
        self.state = State::Paused;
    }

    /// Bascule entre lecture et pause.
    pub fn toggle(&mut self) {
        if self.state == State::Playing {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Se place à un instant donné, en secondes.
    pub fn seek(&mut self, seconds: f64) {
        let target = if self.duration > 0.0 {
            seconds.clamp(0.0, self.duration)
        } else {
            seconds.max(0.0)
        };
        #[allow(clippy::cast_possible_truncation)] // borné par la durée du média
        let ticks = (target * TICKS) as i64;
        let position = PropVariant::from_i64(ticks);
        // SAFETY : lecteur valide ; `GUID_NULL` demande l'unité par défaut,
        // les centaines de nanosecondes.
        unsafe {
            let vtbl = self.reader.vtbl::<mf::SourceReaderVtbl>();
            ((*vtbl).flush)(self.reader.0, mf::ALL_STREAMS);
            ((*vtbl).set_current_position)(
                self.reader.0,
                &raw const mf::GUID_NULL,
                &raw const position,
            );
        }
        self.current = None;
        self.pending = None;
        self.video_done = false;
        self.audio_done = false;
        self.anchor_time = target;
        self.anchor_instant = Instant::now();
        if let Some(out) = &mut self.audio {
            out.reset();
            if self.state == State::Playing {
                out.start();
            }
        }
        if self.state == State::Ended {
            self.state = State::Paused;
        }
    }

    /// Avance le média jusqu'à l'instant présent et rend l'image à montrer.
    ///
    /// À appeler à chaque redessin : c'est la boucle de dessin qui mène, et
    /// le lecteur ne fait rien entre deux appels.
    pub fn frame(&mut self) -> Option<&Frame> {
        let now = self.position();
        if self.state == State::Playing {
            self.fill_audio();
        }
        // Rattrape le retard : on jette les images déjà dépassées plutôt que
        // de les montrer en accéléré.
        loop {
            if self.pending.is_none() {
                self.pending = self.decode_video();
            }
            let Some(next) = &self.pending else { break };
            if next.time > now && self.current.is_some() {
                break;
            }
            self.current = self.pending.take();
            if self.state != State::Playing {
                break;
            }
        }
        if self.state == State::Playing && self.exhausted() {
            self.finish();
        }
        self.current.as_ref()
    }

    /// Vrai quand toutes les pistes présentes sont épuisées.
    ///
    /// Un média sans image ne finissait jamais : la fin se jugeait à la seule
    /// vidéo, et faute de vidéo la condition ne se réalisait pas. Le son doit
    /// en plus avoir été **joué**, pas seulement décodé — la carte son a
    /// toujours quelques dixièmes de seconde d'avance sur nous, et couper là
    /// mangerait la dernière note.
    fn exhausted(&self) -> bool {
        let video = self.size.is_none() || (self.video_done && self.pending.is_none());
        let audio = match &self.audio {
            Some(out) => self.audio_done && out.drained(),
            None => true,
        };
        video && audio
    }

    /// Marque la fin de la lecture.
    fn finish(&mut self) {
        self.anchor_time = self.duration;
        if let Some(out) = &mut self.audio {
            out.stop();
        }
        self.state = State::Ended;
    }

    /// Décode l'image suivante, ou `None` à la fin du flux.
    fn decode_video(&mut self) -> Option<Frame> {
        if self.video_done || self.size.is_none() {
            return None;
        }
        let (width, height) = self.size?;
        let mut flags = 0u32;
        let mut timestamp = 0i64;
        let mut sample: *mut c_void = std::ptr::null_mut();
        // SAFETY : lecteur valide ; `sample` reçoit l'échantillon, confié à
        // `Com` juste après.
        let hr = unsafe {
            let vtbl = self.reader.vtbl::<mf::SourceReaderVtbl>();
            ((*vtbl).read_sample)(
                self.reader.0,
                mf::FIRST_VIDEO_STREAM,
                0,
                std::ptr::null_mut(),
                &raw mut flags,
                &raw mut timestamp,
                &raw mut sample,
            )
        };
        if hr < 0 {
            self.video_done = true;
            return None;
        }
        if flags & mf::END_OF_STREAM != 0 {
            self.video_done = true;
        }
        let sample = Com(sample);
        if sample.is_null() {
            return None;
        }
        #[allow(clippy::cast_precision_loss)] // instant en centaines de nanosecondes
        let time = timestamp as f64 / TICKS;
        let bgra = self.copy_pixels(&sample, width, height)?;
        Some(Frame {
            width,
            height,
            bgra,
            time,
        })
    }

    /// Recopie les pixels d'un échantillon dans un tampon à nous.
    fn copy_pixels(&self, sample: &Com, width: u32, height: u32) -> Option<Vec<u8>> {
        let mut buffer: *mut c_void = std::ptr::null_mut();
        // SAFETY : `sample` est un `IMFSample` valide ; `buffer` reçoit un
        // tampon contigu, confié à `Com`.
        let hr = unsafe {
            let vtbl = sample.vtbl::<mf::SampleVtbl>();
            ((*vtbl).convert_to_contiguous_buffer)(sample.0, &raw mut buffer)
        };
        if hr < 0 || buffer.is_null() {
            return None;
        }
        let buffer = Com(buffer);
        let row = width as usize * 4;
        let mut out = vec![0u8; row * height as usize];
        // SAFETY : `lock` rend un pointeur valide sur au moins `length`
        // octets jusqu'à `unlock`, que l'on appelle sans faute ensuite. Les
        // copies sont bornées par la plus petite des deux tailles.
        unsafe {
            let vtbl = buffer.vtbl::<mf::MediaBufferVtbl>();
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut max = 0u32;
            let mut length = 0u32;
            if ((*vtbl).lock)(buffer.0, &raw mut data, &raw mut max, &raw mut length) < 0
                || data.is_null()
            {
                return None;
            }
            let available = length as usize;
            for y in 0..height as usize {
                // Une image à l'envers se lit de la dernière ligne vers la
                // première.
                let source_line = if self.flipped {
                    height as usize - 1 - y
                } else {
                    y
                };
                let start = source_line * self.stride;
                if start + row > available {
                    continue;
                }
                let source = std::slice::from_raw_parts(data.add(start), row);
                out[y * row..(y + 1) * row].copy_from_slice(source);
            }
            ((*vtbl).unlock)(buffer.0);
        }
        Some(out)
    }

    /// Fournit au son de quoi tenir jusqu'au prochain passage.
    fn fill_audio(&mut self) {
        let Some(out) = &self.audio else { return };
        if self.audio_done {
            // Piste finie : il reste le reliquat à confier à la carte. Tant
            // qu'il n'y a pas de tampon libre, on repassera au prochain tour.
            if let Some(out) = &mut self.audio {
                out.flush();
            }
            return;
        }
        let mut needed = out.needed_bytes();
        while needed > 0 {
            let Some((bytes, _)) = self.decode_audio() else {
                break;
            };
            if bytes.is_empty() {
                break;
            }
            needed = needed.saturating_sub(bytes.len());
            if let Some(out) = &mut self.audio {
                out.push(&bytes);
            }
        }
        if self.audio_done {
            if let Some(out) = &mut self.audio {
                out.flush();
            }
        }
    }

    /// Décode le morceau de son suivant.
    fn decode_audio(&mut self) -> Option<(Vec<u8>, f64)> {
        let mut flags = 0u32;
        let mut timestamp = 0i64;
        let mut sample: *mut c_void = std::ptr::null_mut();
        // SAFETY : comme pour la vidéo.
        let hr = unsafe {
            let vtbl = self.reader.vtbl::<mf::SourceReaderVtbl>();
            ((*vtbl).read_sample)(
                self.reader.0,
                mf::FIRST_AUDIO_STREAM,
                0,
                std::ptr::null_mut(),
                &raw mut flags,
                &raw mut timestamp,
                &raw mut sample,
            )
        };
        if hr < 0 || flags & mf::END_OF_STREAM != 0 {
            self.audio_done = true;
            return None;
        }
        let sample = Com(sample);
        if sample.is_null() {
            return Some((Vec::new(), 0.0));
        }
        let mut buffer: *mut c_void = std::ptr::null_mut();
        // SAFETY : `sample` valide ; `buffer` confié à `Com`.
        let hr = unsafe {
            let vtbl = sample.vtbl::<mf::SampleVtbl>();
            ((*vtbl).convert_to_contiguous_buffer)(sample.0, &raw mut buffer)
        };
        if hr < 0 || buffer.is_null() {
            return None;
        }
        let buffer = Com(buffer);
        // SAFETY : `lock` puis `unlock`, comme pour la vidéo.
        let bytes = unsafe {
            let vtbl = buffer.vtbl::<mf::MediaBufferVtbl>();
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut max = 0u32;
            let mut length = 0u32;
            if ((*vtbl).lock)(buffer.0, &raw mut data, &raw mut max, &raw mut length) < 0
                || data.is_null()
            {
                return None;
            }
            let copy = std::slice::from_raw_parts(data, length as usize).to_vec();
            ((*vtbl).unlock)(buffer.0);
            copy
        };
        #[allow(clippy::cast_precision_loss)] // instant en centaines de nanosecondes
        let time = timestamp as f64 / TICKS;
        Some((bytes, time))
    }
}
