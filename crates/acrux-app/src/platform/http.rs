//! Une requête HTTPS, et rien de plus.
//!
//! Acrux n'embarque pas de pile réseau : il n'en a pas besoin, et écrire TLS
//! à la main serait déraisonnable. Il demande donc au système — **WinHTTP**,
//! qui est à Windows ce que `libcurl` est ailleurs : la vérification du
//! certificat, les redirections et le proxy de l'entreprise sont l'affaire de
//! Windows, pas la nôtre.
//!
//! Le seul usage est la recherche de mises à jour ([`crate::update`]).
//! Aucune donnée n'est envoyée : une requête `GET`, un en-tête
//! `User-Agent`, rien d'autre.

// Comme le reste de `platform/` : c'est ici, et nulle part ailleurs, que le
// projet appelle le système (CHARTE_PROJET.md §1.2).
#![allow(unsafe_code)]

use std::ffi::c_void;

use super::wide;

type Handle = *mut c_void;

const WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
const WINHTTP_FLAG_SECURE: u32 = 0x0080_0000;
/// `WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER`.
const QUERY_STATUS_NUMBER: u32 = 0x0013 | 0x2000_0000;

#[link(name = "winhttp")]
extern "system" {
    fn WinHttpOpen(
        agent: *const u16,
        access: u32,
        proxy: *const u16,
        bypass: *const u16,
        flags: u32,
    ) -> Handle;
    fn WinHttpConnect(session: Handle, host: *const u16, port: u16, reserved: u32) -> Handle;
    fn WinHttpOpenRequest(
        connect: Handle,
        verb: *const u16,
        object: *const u16,
        version: *const u16,
        referrer: *const u16,
        accept_types: *const *const u16,
        flags: u32,
    ) -> Handle;
    fn WinHttpSendRequest(
        request: Handle,
        headers: *const u16,
        headers_len: u32,
        optional: *const c_void,
        optional_len: u32,
        total_len: u32,
        context: usize,
    ) -> i32;
    fn WinHttpReceiveResponse(request: Handle, reserved: *mut c_void) -> i32;
    fn WinHttpQueryHeaders(
        request: Handle,
        info: u32,
        name: *const u16,
        buffer: *mut c_void,
        len: *mut u32,
        index: *mut u32,
    ) -> i32;
    fn WinHttpReadData(request: Handle, buffer: *mut c_void, want: u32, read: *mut u32) -> i32;
    fn WinHttpCloseHandle(handle: Handle) -> i32;
}

/// Poignée WinHTTP refermée à la sortie de portée, quel que soit le chemin.
struct Owned(Handle);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY : la poignée vient de WinHTTP et n'est fermée qu'ici.
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

/// Adresse découpée en ce qu'il faut pour WinHTTP.
struct Url {
    host: String,
    path: String,
    port: u16,
}

/// Découpe une adresse `https://hôte/chemin`.
fn split(url: &str) -> Result<Url, String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| String::from("seul HTTPS est accepté"))?;
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.split_once(':') {
        Some((h, p)) => (h, p.parse().unwrap_or(443)),
        None => (authority, 443),
    };
    if host.is_empty() {
        return Err(String::from("adresse sans hôte"));
    }
    Ok(Url {
        host: host.to_string(),
        path: path.to_string(),
        port,
    })
}

/// Récupère le contenu d'une adresse HTTPS.
///
/// Les redirections sont suivies par WinHTTP, et le certificat vérifié par le
/// magasin de certificats de Windows.
///
/// # Errors
/// Adresse mal formée, réseau indisponible, réponse dont le code n'est pas
/// 200, ou corps plus grand que `limit`.
#[cfg(windows)]
pub fn get(url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let parts = split(url)?;
    let agent = wide(&format!("Acrux/{}", env!("CARGO_PKG_VERSION")));
    let host = wide(&parts.host);
    let path = wide(&parts.path);
    let verb = wide("GET");

    // SAFETY : chaque poignée obtenue est confiée à `Owned`, qui la referme ;
    // toutes les chaînes passées sont terminées par zéro et vivent jusqu'à la
    // fin de l'appel ; les tampons annoncés ont la taille indiquée.
    unsafe {
        let session = Owned(WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        ));
        if session.0.is_null() {
            return Err(String::from("réseau : session impossible"));
        }
        let connect = Owned(WinHttpConnect(session.0, host.as_ptr(), parts.port, 0));
        if connect.0.is_null() {
            return Err(format!("réseau : {} injoignable", parts.host));
        }
        let request = Owned(WinHttpOpenRequest(
            connect.0,
            verb.as_ptr(),
            path.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if request.0.is_null() {
            return Err(String::from("réseau : requête impossible"));
        }
        if WinHttpSendRequest(request.0, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) == 0 {
            return Err(String::from("réseau : envoi impossible"));
        }
        if WinHttpReceiveResponse(request.0, std::ptr::null_mut()) == 0 {
            return Err(String::from("réseau : pas de réponse"));
        }
        let mut status: u32 = 0;
        let mut len = 4u32;
        WinHttpQueryHeaders(
            request.0,
            QUERY_STATUS_NUMBER,
            std::ptr::null(),
            (&raw mut status).cast::<c_void>(),
            &raw mut len,
            std::ptr::null_mut(),
        );
        if status != 200 {
            return Err(format!("réseau : réponse {status}"));
        }
        let mut out = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let mut read = 0u32;
            if WinHttpReadData(
                request.0,
                chunk.as_mut_ptr().cast::<c_void>(),
                u32::try_from(chunk.len()).unwrap_or(0),
                &raw mut read,
            ) == 0
            {
                return Err(String::from("réseau : lecture interrompue"));
            }
            if read == 0 {
                break;
            }
            let read = read as usize;
            if out.len() + read > limit {
                return Err(String::from("réseau : réponse trop longue"));
            }
            out.extend_from_slice(&chunk[..read]);
        }
        Ok(out)
    }
}

/// Hors Windows, la recherche de mises à jour n'est pas encore branchée.
///
/// # Errors
/// Toujours.
#[cfg(not(windows))]
pub fn get(_url: &str, _limit: usize) -> Result<Vec<u8>, String> {
    Err(String::from(
        "la recherche de mises à jour n'est disponible que sous Windows",
    ))
}

#[cfg(test)]
#[allow(clippy::expect_used)] // tests : l'échec de découpe est l'échec cherché
mod tests {
    use super::split;

    #[test]
    fn une_adresse_se_decoupe() {
        let u = split("https://api.github.com/repos/x/y/releases/latest").expect("adresse");
        assert_eq!(u.host, "api.github.com");
        assert_eq!(u.path, "/repos/x/y/releases/latest");
        assert_eq!(u.port, 443);

        let u = split("https://exemple.fr").expect("adresse");
        assert_eq!(u.path, "/");

        let u = split("https://exemple.fr:8443/a").expect("adresse");
        assert_eq!(u.port, 8443);
    }

    #[test]
    fn le_http_en_clair_est_refuse() {
        assert!(split("http://exemple.fr").is_err());
        assert!(split("ftp://exemple.fr").is_err());
        assert!(split("https://").is_err());
    }
}
