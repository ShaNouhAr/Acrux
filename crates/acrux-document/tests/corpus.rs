//! Test d'intégration : chaque PDF de `tests/corpus/` (hors `private/`) doit
//! s'ouvrir, exposer un catalogue, au moins une page, et tous ses objets et
//! flux doivent être lisibles.
//!
//! Ajouter un fichier au corpus suffit pour qu'il soit couvert.

use std::path::{Path, PathBuf};

use acrux_document::{collect_pages, Document, Object, ObjectRef};

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("corpus")
}

fn pdf_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "private") {
                continue;
            }
            pdf_files(&path, out);
        } else if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
        {
            out.push(path);
        }
    }
}

/// Mot de passe d'un fichier de corpus, donné par convention dans son nom :
/// `xxx-mdp-<mot de passe>.pdf`.
fn corpus_password(path: &Path) -> Option<Vec<u8>> {
    let stem = path.file_stem()?.to_str()?;
    let (_, pw) = stem.rsplit_once("-mdp-")?;
    Some(pw.as_bytes().to_vec())
}

#[test]
fn every_corpus_file_is_fully_readable() {
    let mut files = Vec::new();
    pdf_files(&corpus_dir(), &mut files);
    assert!(
        !files.is_empty(),
        "aucun PDF dans {}",
        corpus_dir().display()
    );
    let mut failures = Vec::new();
    for path in &files {
        let name = path.display().to_string();
        let doc = match Document::load(path) {
            Ok(d) => d,
            Err(e) => {
                failures.push(format!("{name} : ouverture impossible ({e})"));
                continue;
            }
        };
        if let Some(pw) = corpus_password(path) {
            if let Err(e) = doc.authenticate(&pw) {
                failures.push(format!("{name} : mot de passe refusé ({e})"));
                continue;
            }
        }
        if doc.needs_password() {
            failures.push(format!(
                "{name} : chiffré sans mot de passe connu (nommer le fichier -mdp-<mdp>.pdf)"
            ));
            continue;
        }
        if doc.catalog().is_err() {
            failures.push(format!("{name} : catalogue illisible"));
        }
        match collect_pages(&doc) {
            Ok(p) if p.is_empty() => failures.push(format!("{name} : aucune page")),
            Ok(_) => {}
            Err(e) => failures.push(format!("{name} : pages illisibles ({e})")),
        }
        for n in doc.object_numbers() {
            let r = ObjectRef {
                number: n,
                generation: 0,
            };
            match doc.get(r) {
                Ok(o) => {
                    if matches!(&*o, Object::Stream { .. }) {
                        if let Err(e) = doc.stream_data(&o) {
                            failures.push(format!("{name} : flux {n} non décodable ({e})"));
                        }
                    }
                }
                Err(e) => failures.push(format!("{name} : objet {n} illisible ({e})")),
            }
        }
        if doc.was_repaired() && !name.contains("casse") {
            failures.push(format!(
                "{name} : réparation déclenchée sur un fichier censé être sain : {:?}",
                doc.warnings()
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
