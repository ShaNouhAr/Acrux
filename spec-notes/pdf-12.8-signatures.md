# Signatures numériques (ISO 32000-2 §12.8)

## En deux phrases
Une signature PDF est un champ de formulaire `/FT /Sig` dont la valeur `/V` est un
dictionnaire portant deux choses : `/ByteRange`, qui dit **quels octets du fichier** sont
signés, et `/Contents`, qui contient une enveloppe **CMS** (RFC 5652) détachée — la
signature proprement dite. Tout le reste (nom du signataire, date, motif) n'est qu'une
déclaration du signataire, sans valeur tant qu'on n'a pas vérifié la première partie.

## Le format

### Le champ et sa valeur
```
30 0 obj                                        % champ + widget fusionnés
<< /Type /Annot /Subtype /Widget /FT /Sig /T (Signature1)
   /V 29 0 R /P 3 0 R /F 132 /Rect [230 70 390 130]
   /AP << /N 31 0 R >> >>
endobj

29 0 obj                                        % le dictionnaire de signature
<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached
   /ByteRange [0 1646 34416 1161                ]
   /Contents <308209...00000000000000>          % réservation de 16 Ko
   /M (D:20260520120000Z) /Name (Camille Test)
   /Reason (Approbation) /Location (Paris) >>
endobj
```
Le catalogue porte `/AcroForm << /Fields [30 0 R] /SigFlags 3 >>` : bit 1
`SignaturesExist`, bit 2 `AppendOnly` (« ce fichier ne doit être modifié que par ajout »).

### `/ByteRange`
Quatre entiers, deux couples `(décalage, longueur)` :
```
/ByteRange [0 1646 34416 1161]
            └──┬──┘ └────┬───┘
      du début  jusqu'à   de la fin du /Contents
      au début  l'octet   jusqu'à la fin du fichier
      du /Contents 1645
```
Les octets couverts sont la **concaténation** des deux plages. L'intervalle laissé libre
(ici 1646 à 34415) doit contenir exactement la chaîne hexadécimale `<…>` du `/Contents`,
délimiteurs compris : c'est le seul endroit du fichier que la signature ne peut pas couvrir,
puisqu'elle y est écrite.

### L'enveloppe CMS (`adbe.pkcs7.detached`)
```
ContentInfo
 ├ contentType = id-signedData (1.2.840.113549.1.7.2)
 └ [0] SignedData
      ├ version = 1
      ├ digestAlgorithms = { sha-256 }
      ├ encapContentInfo = { id-data }          ← pas de eContent : « détachée »
      ├ [0] certificates = { signataire, intermédiaires, racine }
      └ signerInfos = { SignerInfo }
           ├ sid = issuerAndSerialNumber
           ├ digestAlgorithm = sha-256
           ├ [0] signedAttrs                    ← SIGNÉ sous forme SET OF
           │    ├ contentType   = id-data
           │    ├ signingTime   = UTCTime
           │    └ messageDigest = SHA-256(octets du /ByteRange)
           ├ signatureAlgorithm = rsaEncryption
           └ signature = RSASSA-PKCS1-v1_5(SHA-256(signedAttrs))
```

**Le piège central.** Quand `signedAttrs` est présent — c'est toujours le cas en PDF — la
signature RSA ne porte **pas** sur le document, mais sur les attributs signés. Le seul lien
avec le document est l'attribut `messageDigest`. Un vérificateur qui contrôle la signature
RSA sans recalculer le condensé des octets du `/ByteRange` et le comparer à `messageDigest`
ne vérifie rien : on peut recopier tels quels les `signedAttrs` et la signature d'un
document authentique dans un autre document.

Deuxième subtilité : les `signedAttrs` apparaissent dans le fichier avec le tag
`[0] IMPLICIT` (0xA0), mais ce qui est signé est leur forme `SET OF` (0x31). Il faut
re-taguer avant de condenser. La longueur ne change pas, donc un simple échange du premier
octet suffit — c'est ce que fait `cms::parse_signer`.

### Les sous-filtres
| `/SubFilter` | `/Contents` | Nous |
|---|---|---|
| `adbe.pkcs7.detached` | CMS détaché | lu et **écrit** |
| `ETSI.CAdES.detached` | CMS détaché, profil PAdES | lu |
| `adbe.pkcs7.sha1` | CMS dont l'`eContent` est le SHA-1 du document | lu (obsolète) |
| `adbe.x509.rsa_sha1` | signature RSA brute, certificat dans `/Cert` | lu (obsolète) |
| `ETSI.RFC3161` | jeton d'horodatage de document | reconnu, non vérifié |

## Les pièges

### 1. Une signature qui ne couvre pas tout le fichier
Rien n'oblige `/ByteRange` à aller jusqu'au dernier octet. Un attaquant ajoute une mise à
jour incrémentale après la zone couverte : la signature reste **cryptographiquement
valide**, mais le document affiché n'est plus celui qui a été signé. C'est la famille des
*shadow attacks* / *incremental saving attacks* (Mladenov & al., 2019). Nous la traitons
comme deux verdicts distincts :
- **couverture** : `byte_range[0].0 == 0`, l'intervalle libre est exactement le `/Contents`,
  et `fin de la dernière plage == taille du fichier` ;
- **modifications** : on relit le fichier **tronqué à la fin de la zone couverte** comme un
  document à part entière et on compare objet par objet avec le document complet. Ce qui a
  changé est nommé.

### 2. Un intervalle libre plus grand que le `/Contents`
Si l'intervalle non couvert dépasse la chaîne hexadécimale, les octets en trop ne sont ni
signés ni visibles dans le `/Contents` : ils peuvent contenir des objets entiers. Nous
refusons tout intervalle qui n'est pas exactement `<` + hexadécimal + `>`.

### 3. `/ByteRange` écrit après coup, sans décalage
On ne peut pas calculer `/ByteRange` avant d'écrire le fichier, ni réécrire le fichier après
l'avoir calculé. La seule issue est la **réservation à largeur fixe** :
1. écrire la mise à jour incrémentale avec `/ByteRange [0 1000000000 1000000000 1000000000]`
   et un `/Contents` de *n* octets nuls (donc `2n + 2` caractères hexadécimaux) ;
2. relire les décalages réels dans les octets produits ;
3. récrire les quatre entiers **au même endroit, sur la même largeur**, complétés par des
   espaces — le PDF admet des espaces surnuméraires dans un tableau ;
4. condenser les octets couverts, fabriquer le CMS, l'écrire en hexadécimal dans la
   réservation, et compléter par des zéros. La longueur DER de l'enveloppe en délimite la
   fin ; les zéros qui suivent sont ignorés par tout lecteur.

### 4. Plusieurs révisions signées
Chaque signature couvre sa propre révision. Une deuxième signature ajoutée par mise à jour
incrémentale est valide et couvre tout le fichier ; la première voit forcément des octets
après elle. Acrobat le tolère quand les changements sont autorisés par `/DocMDP` ; nous nous
contentons pour l'instant de **le dire** : verdict « couverture » négatif sur la première
signature, avec la liste des objets ajoutés. Voir « Ce qui reste à faire ».

### 5. Certificats
- Le nom de l'émetteur d'un `SignerInfo` doit être comparé **octet pour octet** à celui du
  certificat (RFC 5652 §5.3). Re-normaliser un nom distinctif, c'est risquer d'apparier deux
  identités différentes ; nous conservons donc les octets DER de chaque `Name`.
- Une extension **critique** que l'on ne sait pas traiter doit faire échouer la validation
  (RFC 5280 §6.1.3 f). C'est contre-intuitif mais c'est le seul comportement sûr : une
  extension critique dit « si tu ne me comprends pas, refuse ».
- `keyUsage` absent vaut « tous usages » ; présent, il doit porter `digitalSignature` ou
  `contentCommitment` pour une feuille, `keyCertSign` pour une autorité.

### 6. DER, pas BER
Tout ce qui précède est spécifié en DER : longueurs définies et minimales, booléens 0x00 ou
0xFF, `SET OF` trié. Notre lecteur refuse la forme à longueur indéfinie et les longueurs non
minimales. Accepter du BER laxiste, c'est accepter que deux analyseurs lisent deux
structures différentes dans les mêmes octets — et c'est là que naissent les contournements.

## Où c'est implémenté

| Rôle | Module |
|---|---|
| Entiers de grande taille, Montgomery | `acrux_document::crypt::bigint` |
| RSA : EMSA, PKCS#1 v1.5, PSS, clés, génération | `acrux_document::crypt::rsa` |
| SHA-1 (lecture seule) | `acrux_document::crypt::sha1` |
| DER : lecteur strict, écrivain | `acrux_document::asn1` |
| Certificats X.509 : lecture, vérification, émission | `acrux_features::signature::x509` |
| CMS `SignedData` : lecture, fabrication | `acrux_features::signature::cms` |
| Inventaire des champs `/Sig` | `acrux_features::signature::list_signatures` |
| Les cinq verdicts | `acrux_features::signature::verify` |
| Pose d'une signature | `acrux_features::signature::sign` |
| `acr signatures` / `verify` / `sign` | `acrux_cli` |

## Les tests

- `crypt::bigint::tests` : addition, soustraction, produit, division de Knuth (dont le cas où
  l'estimation du quotient doit être corrigée), inverse modulaire, Montgomery contre une
  exponentiation naïve.
- `crypt::rsa::tests::dotnet_interoperability_vector` : une clé RSA 2048 et une signature
  produites par **.NET** (`RSA.Create(2048)`, `SignData(…, SHA256, Pkcs1)`) sont figées dans
  le test. Nous acceptons cette signature et, `RSASSA-PKCS1-v1_5` étant déterministe, nous
  produisons **exactement les mêmes octets**.
- `crypt::rsa::tests::digest_info_prefixes_match_our_der_writer` : les préfixes `DigestInfo`
  figés de la RFC 8017 §9.2 sont reproduits par notre écrivain DER.
- `asn1::tests` : longueurs courtes et longues, refus du BER indéfini et des longueurs non
  minimales, entiers signés minimaux, OID à plusieurs octets (dont `2.100.3`), imbrication,
  tags à plusieurs octets, dates `UTCTime` / `GeneralizedTime` avec fuseau.
- `signature::x509::tests` : certificat auto-signé fabriqué puis relu, chaîne émetteur →
  feuille, extension critique inconnue enregistrée, paramètres PSS.
- `signature::cms::tests` : aller-retour d'un `SignedData` détaché, tolérance au bourrage de
  zéros du `/Contents`.
- `signature::corpus` : le corpus de synthèse (autorité de test, certificat de signataire,
  attestation signée) et les scénarios d'attaque — un octet modifié invalide le condensé,
  une mise à jour incrémentale postérieure est détectée, une double signature garde les deux
  champs.

**Contrôle par un tiers.** L'enveloppe produite pour `signature-attestation-signee.pdf` a été
relue par `System.Security.Cryptography.Pkcs.SignedCms` de .NET en mode détaché :
`CheckSignature` l'accepte, rejette un contenu modifié d'un bit, et `X509Chain` en mode
`CustomRootTrust` valide notre certificat de signataire jusqu'à notre racine **sans aucun
statut d'erreur**. Nos certificats et notre CMS sont donc conformes, pas seulement
auto-cohérents.

## Ce qui reste à faire

| Manque | Pourquoi ça compte |
|---|---|
| **Horodatage RFC 3161** | Sans jeton d'horodatage, la date de signature est déclarative. Un signataire peut l'antidater, et une signature devient invérifiable dès que son certificat expire. Il faut fabriquer une `TimeStampReq`, la poster à une TSA (HTTP), et ranger la `TimeStampResp` dans l'attribut non signé `id-aa-timeStampToken`. |
| **Révocation OCSP / CRL** | Nous ne savons pas si un certificat a été révoqué. Un certificat volé puis révoqué continuerait de « valider » chez nous. Demande un client HTTP et l'analyse des réponses OCSP (RFC 6960) et des CRL (RFC 5280 §5). |
| **LTV / PAdES B-LT et B-LTA** | `/DSS` (`/Certs`, `/OCSPs`, `/CRLs`) et horodatages de document en cascade, pour qu'une signature reste vérifiable des années après. |
| **`/DocMDP`, `/FieldMDP`, `/UR`** | Pose d'une signature de **certification** et évaluation fine des modifications autorisées après signature : aujourd'hui toute modification postérieure est signalée, sans distinguer un remplissage de formulaire légitime d'une réécriture du contenu. |
| **ECDSA et Ed25519** | Seul RSA est implémenté. Les certificats à clé elliptique sont de plus en plus courants. |
| **Signer un document chiffré** | Le `/Contents` doit rester en clair alors que l'enregistrement chiffre les chaînes ; il faut exempter cet objet, comme `/Encrypt` l'est déjà. |
| **PKCS#12 / magasin de certificats de Windows** | Aujourd'hui, clé PKCS#8 non chiffrée et certificat DER en fichiers séparés. |
| **Générateur d'aléa du système** | `rsa::SeededRandom` est déterministe et ne convient qu'au matériel de test. Une vraie génération de clé exige l'entropie du système d'exploitation (`BCryptGenRandom` sur Windows, `getrandom` ailleurs) — ce sera le premier usage de `acrux-app/src/platform/`. |
