# `acrux-winres` — ressources Windows des exécutables

Ce module écrit le fichier `.res` que l'éditeur de liens incorpore dans
`acrux.exe` et `acr.exe` : l'**icône** du programme et le **bloc de version**
affiché par l'Explorateur.

Il ne dépend de rien : ni bibliothèque externe, ni `rc.exe` du SDK Windows.
Les trois formats concernés sont écrits à la main, comme le reste du projet.

## Utilisation

Dans le `build.rs` d'un exécutable :

```rust
use std::path::Path;

fn main() {
    let info = acrux_winres::VersionInfo {
        file_version: [0, 0, 1, 0],
        product_version: [0, 0, 1, 0],
        language: 0x040C, // français
        strings: &[("ProductName", "Acrux"), ("OriginalFilename", "acrux.exe")],
    };
    if let Err(error) = acrux_winres::emit("acrux", Path::new("../../branding/acrux.ico"), &info) {
        println!("cargo:warning=ressources Windows non incorporées : {error}");
    }
}
```

`emit` ne fait rien hors cible Windows/MSVC, et se contente d'un avertissement
si l'icône est absente : le logiciel se construit sans le dossier `branding/`.

## Les trois formats

### Le conteneur `.res`

Une suite d'enregistrements, chacun précédé d'un en-tête de 32 octets (taille
des données, taille de l'en-tête, type et nom sous forme d'ordinaux, langue,
drapeaux mémoire hérités de Windows 3). Les données sont complétées jusqu'à la
frontière de quatre octets suivante. Le fichier commence par un enregistrement
vide, de type 0 et de nom 0.

### Le groupe d'icônes

Un fichier `.ico` et la ressource `RT_GROUP_ICON` décrivent la même chose avec
une différence unique : dans le fichier, chaque entrée du répertoire finit par
la **position** de l'image sur 32 bits ; dans la ressource, par l'**identifiant**
sur 16 bits de la ressource `RT_ICON` qui la contient. Les images elles-mêmes,
PNG ou DIB, sont recopiées sans modification.

Le nombre de bits par pixel est relu dans l'image (en-tête `IHDR` d'un PNG,
`BITMAPINFOHEADER` d'un DIB) plutôt que cru sur parole : beaucoup d'outils
laissent des zéros dans le répertoire. `rc.exe` fait la même relecture.

### Le bloc de version

`VS_VERSIONINFO` est un arbre de nœuds `(longueur, longueur de la valeur, type,
clé, valeur, enfants)`, chaque membre aligné sur quatre octets. Le piège est la
longueur de la valeur : elle compte des **caractères** pour une valeur textuelle
et des **octets** pour une valeur binaire.

La racine porte la structure fixe `VS_FIXEDFILEINFO` (52 octets, signature
`0xFEEF04BD`), puis deux enfants : `StringFileInfo`, qui contient la table des
chaînes affichées, et `VarFileInfo`, qui déclare la langue et la page de codes.

## Vérifier le résultat

Les informations de version se lisent directement :

```powershell
(Get-Item target\release\acrux.exe).VersionInfo | Format-List *
```

Et l'icône réellement posée sur la fenêtre se contrôle sans rien afficher, en
interrogeant la classe de la fenêtre invisible du harnais `tools/headless.ps1`
(`GetClassLongPtrW`, indices `-14` et `-34`).

## Fichiers

| Fichier      | Rôle                                                        |
| ------------ | ----------------------------------------------------------- |
| `lib.rs`     | conteneur `.res` et assemblage des ressources               |
| `icon.rs`    | lecture d'un `.ico`, construction du `RT_GROUP_ICON`        |
| `version.rs` | construction du bloc `VS_VERSIONINFO`                       |
| `build.rs`   | raccordement aux scripts de compilation (`OUT_DIR`, Cargo)  |
