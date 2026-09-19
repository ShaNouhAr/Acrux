//! Incorpore l'icône et les informations de version dans `acrux-setup.exe`.
//!
//! Voir `crates/acrux-winres/` : les ressources Windows sont écrites par nos
//! soins, sans `rc.exe` ni bibliothèque externe.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let version = env!("CARGO_PKG_VERSION");
    let info = acrux_winres::VersionInfo {
        file_version: numbers(version),
        product_version: numbers(version),
        language: 0x040C, // français
        strings: &[
            ("CompanyName", "Contributeurs Acrux"),
            ("FileDescription", "Installateur d'Acrux"),
            ("FileVersion", version),
            ("InternalName", "acrux-setup"),
            ("LegalCopyright", "Contributeurs Acrux — licence Apache-2.0"),
            ("OriginalFilename", "acrux-setup.exe"),
            ("ProductName", "Acrux"),
            ("ProductVersion", version),
        ],
    };
    if let Err(error) =
        acrux_winres::emit("acrux-setup", Path::new("../../branding/acrux.ico"), &info)
    {
        println!("cargo:warning=ressources Windows non incorporées : {error}");
    }
}

/// Découpe « 0.0.1 » en quatre nombres, complétés par des zéros.
fn numbers(version: &str) -> [u16; 4] {
    let mut out = [0u16; 4];
    for (slot, part) in out.iter_mut().zip(version.split('.')) {
        *slot = part.parse().unwrap_or(0);
    }
    out
}
