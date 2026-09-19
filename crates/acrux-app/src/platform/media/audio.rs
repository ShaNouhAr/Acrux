//! Sortie audio, par l'API de forme d'onde de Windows.
//!
//! `waveOut` a trente ans et c'est précisément sa qualité : elle mélange,
//! rééchantillonne et tient l'horloge toute seule, en une douzaine d'appels.
//! WASAPI ferait mieux pour un studio d'enregistrement ; pour jouer la bande
//! son d'une vidéo dans un PDF, elle ne ferait que trois cents lignes de plus.
//!
//! Le point qui compte vraiment est ailleurs : [`Output::played_seconds`]
//! demande à la carte son **ce qu'elle a réellement joué**. C'est cette
//! valeur, et non l'horloge de la machine, qui donne l'heure au lecteur — une
//! machine chargée retarde le programme, jamais la carte son.

#![allow(unsafe_code)]

use std::ffi::c_void;

/// Nombre de tampons tournants. Trois suffisent à couvrir un hoquet de
/// l'interface sans ajouter de retard perceptible.
const BUFFERS: usize = 3;
/// Durée visée d'un tampon. Plus court, on risque des coupures ; plus long,
/// la mise en pause tarde à se faire entendre.
const BUFFER_SECONDS: f64 = 0.12;

type Handle = *mut c_void;

/// `WAVEFORMATEX` (mmreg.h).
#[repr(C)]
struct WaveFormat {
    tag: u16,
    channels: u16,
    samples_per_sec: u32,
    avg_bytes_per_sec: u32,
    block_align: u16,
    bits_per_sample: u16,
    size: u16,
}

/// `WAVEHDR` (mmsystem.h).
#[repr(C)]
struct WaveHeader {
    data: *mut u8,
    buffer_length: u32,
    bytes_recorded: u32,
    user: usize,
    flags: u32,
    loops: u32,
    next: *mut WaveHeader,
    reserved: usize,
}

/// `MMTIME` demandé en nombre d'échantillons.
#[repr(C)]
struct MmTime {
    kind: u32,
    value: u32,
    padding: u32,
}

/// PCM entier, le seul format qu'on demande.
const WAVE_FORMAT_PCM: u16 = 1;
/// Laisse Windows choisir le périphérique de sortie.
const WAVE_MAPPER: u32 = 0xFFFF_FFFF;
/// Aucun rappel : on interroge nous-mêmes l'état des tampons.
const CALLBACK_NULL: u32 = 0;
/// `WHDR_DONE` : le tampon a été joué et peut resservir.
const WHDR_DONE: u32 = 0x0000_0001;
/// `TIME_SAMPLES`.
const TIME_SAMPLES: u32 = 0x0000_0002;

#[link(name = "winmm")]
extern "system" {
    fn waveOutOpen(
        out: *mut Handle,
        device: u32,
        format: *const WaveFormat,
        callback: usize,
        instance: usize,
        flags: u32,
    ) -> u32;
    fn waveOutClose(handle: Handle) -> u32;
    fn waveOutPrepareHeader(handle: Handle, header: *mut WaveHeader, size: u32) -> u32;
    fn waveOutUnprepareHeader(handle: Handle, header: *mut WaveHeader, size: u32) -> u32;
    fn waveOutWrite(handle: Handle, header: *mut WaveHeader, size: u32) -> u32;
    fn waveOutPause(handle: Handle) -> u32;
    fn waveOutRestart(handle: Handle) -> u32;
    fn waveOutReset(handle: Handle) -> u32;
    fn waveOutGetPosition(handle: Handle, time: *mut MmTime, size: u32) -> u32;
}

/// Un tampon prêté à la carte son.
struct Slot {
    header: Box<WaveHeader>,
    data: Vec<u8>,
    prepared: bool,
}

/// La sortie audio d'un média.
pub struct Output {
    handle: Handle,
    slots: Vec<Slot>,
    /// Taille visée d'un tampon.
    chunk: usize,
    /// Échantillons déjà joués lors des remises à zéro précédentes : la
    /// position rendue par `waveOut` repart de zéro à chaque `reset`.
    played_before: f64,
    /// Restes d'un morceau décodé plus grand qu'un tampon.
    leftover: Vec<u8>,
    rate: u32,
    running: bool,
}

impl Output {
    /// Ouvre la sortie audio pour un format donné.
    ///
    /// # Errors
    /// Aucun périphérique, ou format refusé par le système.
    pub fn open(channels: u32, rate: u32, bits: u32) -> Result<Output, String> {
        let channels = u16::try_from(channels).unwrap_or(2).clamp(1, 8);
        let bits = u16::try_from(bits).unwrap_or(16);
        let block_align = channels * bits / 8;
        let format = WaveFormat {
            tag: WAVE_FORMAT_PCM,
            channels,
            samples_per_sec: rate,
            avg_bytes_per_sec: rate * u32::from(block_align),
            block_align,
            bits_per_sample: bits,
            size: 0,
        };
        let mut handle: Handle = std::ptr::null_mut();
        // SAFETY : `format` vit pendant l'appel ; `handle` reçoit le
        // périphérique ouvert, refermé par `Drop`.
        let code = unsafe {
            waveOutOpen(
                &raw mut handle,
                WAVE_MAPPER,
                &raw const format,
                0,
                0,
                CALLBACK_NULL,
            )
        };
        if code != 0 || handle.is_null() {
            return Err(format!("sortie audio indisponible (erreur {code})"));
        }
        let bytes_per_second = (rate as usize) * usize::from(block_align);
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let chunk = ((bytes_per_second as f64 * BUFFER_SECONDS) as usize)
            .max(usize::from(block_align))
            / usize::from(block_align)
            * usize::from(block_align);
        let slots = (0..BUFFERS)
            .map(|_| Slot {
                header: Box::new(WaveHeader {
                    data: std::ptr::null_mut(),
                    buffer_length: 0,
                    bytes_recorded: 0,
                    user: 0,
                    flags: 0,
                    loops: 0,
                    next: std::ptr::null_mut(),
                    reserved: 0,
                }),
                data: vec![0u8; chunk],
                prepared: false,
            })
            .collect();
        // La carte son démarre à l'arrêt : rien ne sort tant qu'on n'a pas
        // demandé la lecture.
        // SAFETY : `handle` vient d'être ouvert.
        unsafe {
            waveOutPause(handle);
        }
        Ok(Output {
            handle,
            slots,
            chunk,
            played_before: 0.0,
            leftover: Vec::new(),
            rate,
            running: false,
        })
    }

    /// Secondes réellement jouées depuis la dernière remise à zéro.
    #[must_use]
    pub fn played_seconds(&self) -> f64 {
        let mut time = MmTime {
            kind: TIME_SAMPLES,
            value: 0,
            padding: 0,
        };
        // SAFETY : `handle` est ouvert ; `time` a la taille annoncée.
        let code = unsafe {
            waveOutGetPosition(
                self.handle,
                &raw mut time,
                u32::try_from(std::mem::size_of::<MmTime>()).unwrap_or(12),
            )
        };
        if code != 0 || time.kind != TIME_SAMPLES || self.rate == 0 {
            return self.played_before;
        }
        self.played_before + f64::from(time.value) / f64::from(self.rate)
    }

    /// Nombre d'octets qu'il faudrait fournir pour garder tous les tampons
    /// pleins.
    #[must_use]
    pub fn needed_bytes(&self) -> usize {
        let free = self
            .slots
            .iter()
            .filter(|s| !s.prepared || s.header.flags & WHDR_DONE != 0)
            .count();
        free * self.chunk
    }

    /// Vrai si la carte son n'a plus rien à jouer : tous les tampons prêtés
    /// lui ont été rendus, et il ne reste aucun reste en attente.
    #[must_use]
    pub fn drained(&self) -> bool {
        self.leftover.is_empty()
            && self
                .slots
                .iter()
                .all(|s| !s.prepared || s.header.flags & WHDR_DONE != 0)
    }

    /// Ajoute du son à jouer.
    pub fn push(&mut self, bytes: &[u8]) {
        self.leftover.extend_from_slice(bytes);
        while self.leftover.len() >= self.chunk {
            let Some(index) = self.free_slot() else { break };
            let piece: Vec<u8> = self.leftover.drain(..self.chunk).collect();
            self.submit(index, &piece);
        }
    }

    /// Confie à la carte ce qui reste, même si cela ne fait pas un tampon
    /// plein.
    ///
    /// [`Output::push`] n'envoie que des morceaux entiers, pour ne pas hacher
    /// le son en petits bouts. À la fin d'un média il reste donc presque
    /// toujours un reliquat plus court qu'un tampon : sans ce vidage, il ne
    /// serait jamais joué — la dernière fraction de seconde manquerait, et le
    /// lecteur, attendant un son qui ne vient pas, ne se terminerait jamais.
    pub fn flush(&mut self) {
        while !self.leftover.is_empty() {
            let Some(index) = self.free_slot() else { break };
            let take = self.leftover.len().min(self.chunk);
            let piece: Vec<u8> = self.leftover.drain(..take).collect();
            self.submit(index, &piece);
        }
    }

    /// Indice d'un tampon libre.
    fn free_slot(&mut self) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| !s.prepared || s.header.flags & WHDR_DONE != 0)
    }

    /// Confie un morceau à la carte son.
    fn submit(&mut self, index: usize, piece: &[u8]) {
        let size = u32::try_from(std::mem::size_of::<WaveHeader>()).unwrap_or(0);
        let slot = &mut self.slots[index];
        // SAFETY : `slot.header` a été préparé par un appel précédent ; on le
        // libère avant de le réemployer, comme l'exige l'API.
        if slot.prepared {
            unsafe {
                waveOutUnprepareHeader(self.handle, std::ptr::from_mut(&mut *slot.header), size);
            }
            slot.prepared = false;
        }
        slot.data[..piece.len()].copy_from_slice(piece);
        slot.header.data = slot.data.as_mut_ptr();
        slot.header.buffer_length = u32::try_from(piece.len()).unwrap_or(0);
        slot.header.flags = 0;
        // SAFETY : le tampon pointé vit dans `slot.data`, qui n'est ni
        // déplacé ni libéré tant que l'en-tête est prêté ; `Drop` fait un
        // `waveOutReset` avant toute libération.
        unsafe {
            let header = std::ptr::from_mut(&mut *slot.header);
            if waveOutPrepareHeader(self.handle, header, size) == 0 {
                slot.prepared = true;
                waveOutWrite(self.handle, header, size);
            }
        }
    }

    /// Démarre ou reprend la sortie.
    pub fn start(&mut self) {
        if self.running {
            return;
        }
        // SAFETY : périphérique ouvert.
        unsafe {
            waveOutRestart(self.handle);
        }
        self.running = true;
    }

    /// Suspend la sortie sans perdre ce qui est en attente.
    pub fn stop(&mut self) {
        if !self.running {
            return;
        }
        // SAFETY : périphérique ouvert.
        unsafe {
            waveOutPause(self.handle);
        }
        self.running = false;
    }

    /// Vide tout ce qui est en attente, après un déplacement dans le média.
    pub fn reset(&mut self) {
        self.played_before = self.played_seconds();
        // SAFETY : `waveOutReset` rend tous les tampons prêtés ; on les
        // déclare libres juste après.
        unsafe {
            waveOutReset(self.handle);
            waveOutPause(self.handle);
        }
        let size = u32::try_from(std::mem::size_of::<WaveHeader>()).unwrap_or(0);
        for slot in &mut self.slots {
            if slot.prepared {
                // SAFETY : l'en-tête a été préparé et vient d'être rendu.
                unsafe {
                    waveOutUnprepareHeader(
                        self.handle,
                        std::ptr::from_mut(&mut *slot.header),
                        size,
                    );
                }
                slot.prepared = false;
            }
            slot.header.flags = 0;
        }
        self.leftover.clear();
        self.running = false;
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        if self.handle.is_null() {
            return;
        }
        let size = u32::try_from(std::mem::size_of::<WaveHeader>()).unwrap_or(0);
        // SAFETY : `waveOutReset` rend d'abord tous les tampons prêtés ; sans
        // lui, libérer `slot.data` laisserait la carte son lire de la mémoire
        // rendue au système.
        unsafe {
            waveOutReset(self.handle);
            for slot in &mut self.slots {
                if slot.prepared {
                    waveOutUnprepareHeader(
                        self.handle,
                        std::ptr::from_mut(&mut *slot.header),
                        size,
                    );
                    slot.prepared = false;
                }
            }
            waveOutClose(self.handle);
        }
        self.handle = std::ptr::null_mut();
    }
}
