//! Remplissage en place sur le formulaire du corpus
//! (`tests/corpus/synthese/formulaire-acroform-champs.pdf`) : ce que
//! l'application demande au moteur quand on ouvre le document, qu'on passe
//! d'un champ à l'autre au clavier, qu'on tape dans un peigne et qu'on efface
//! le formulaire.
//!
//! Le fichier est lu en mémoire : rien n'est jamais écrit dans le corpus.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use acrux_document::Document;
use acrux_features::forms::{
    list_fields, prepare_display, reset_fields, set_field_value, tab_order, Field, FieldValue,
};

fn corpus_form() -> Document {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus/synthese/formulaire-acroform-champs.pdf");
    Document::from_bytes(std::fs::read(path).unwrap()).unwrap()
}

fn value<'a>(fields: &'a [Field], name: &str) -> Option<&'a FieldValue> {
    fields
        .iter()
        .find(|f| f.name == name)
        .and_then(|f| f.value.as_ref())
}

/// Le seul widget sans apparence du fichier est celui du champ en lecture
/// seule : l'ouverture la lui donne, et ne touche à rien d'autre.
#[test]
fn l_ouverture_complete_le_seul_widget_sans_apparence() {
    let doc = corpus_form();
    assert_eq!(prepare_display(&doc).unwrap(), 1);
    let fields = list_fields(&doc).unwrap();
    assert!(fields
        .iter()
        .flat_map(|f| &f.widgets)
        .all(|w| w.has_appearance));
    // Une seconde fois : plus rien à faire.
    assert_eq!(prepare_display(&doc).unwrap(), 0);
}

/// Sans `/Tabs`, Tab suit l'ordre de `/Annots`, comme Acrobat ; le champ
/// figé n'est pas un arrêt, le groupe radio n'en fait qu'un, sur son
/// bouton coché.
#[test]
fn la_tabulation_suit_l_ordre_des_annotations() {
    let doc = corpus_form();
    let fields = list_fields(&doc).unwrap();
    let order: Vec<String> = tab_order(&doc, &fields)
        .unwrap()
        .into_iter()
        .map(|(fi, wi)| format!("{}#{wi}", fields[fi].name))
        .collect();
    assert_eq!(
        order,
        [
            "nom#0",
            "abonne#0",
            "taille#1",
            "pays#0",
            "langues#0",
            "adresse.rue#0",
            "adresse.ville#0",
            "code#0",
            "secret#0",
        ]
    );
}

/// Le peigne de cinq cases ne garde que cinq caractères, et effacer le
/// formulaire vide tout sauf le champ figé.
#[test]
fn peigne_puis_effacement() {
    let doc = corpus_form();
    set_field_value(&doc, "code", FieldValue::Text("7500123".into())).unwrap();
    let fields = list_fields(&doc).unwrap();
    assert_eq!(
        value(&fields, "code"),
        Some(&FieldValue::Text("75001".into()))
    );

    assert_eq!(reset_fields(&doc).unwrap(), 9);
    let fields = list_fields(&doc).unwrap();
    for name in ["nom", "code", "secret", "adresse.rue", "adresse.ville"] {
        assert_eq!(
            value(&fields, name),
            Some(&FieldValue::Text(String::new())),
            "{name}"
        );
    }
    assert_eq!(
        value(&fields, "abonne"),
        Some(&FieldValue::State("Off".into()))
    );
    assert_eq!(
        value(&fields, "taille"),
        Some(&FieldValue::State("Off".into()))
    );
    assert_eq!(value(&fields, "pays"), None);
    assert_eq!(value(&fields, "langues"), None);
    assert_eq!(
        value(&fields, "systeme.fige"),
        Some(&FieldValue::Text("non modifiable".into()))
    );
    // Le document effacé s'enregistre et se relit à l'identique.
    let again = Document::from_bytes(doc.save_incremental().unwrap()).unwrap();
    let reread = list_fields(&again).unwrap();
    assert_eq!(value(&reread, "pays"), None);
    assert_eq!(
        value(&reread, "code"),
        Some(&FieldValue::Text(String::new()))
    );
}
