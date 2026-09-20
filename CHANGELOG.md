# Journal des versions

Les numéros suivent [SemVer](https://semver.org/lang/fr/) : tant qu'Acrux est
en `0.x`, le deuxième nombre change quand des fonctions apparaissent, le
troisième quand on ne fait que corriger.

Chaque version est publiée par une étiquette `vX.Y.Z` poussée sur le dépôt ;
c'est la section correspondante de ce fichier qui devient la page de version.

## 0.15.0 — 20 septembre 2026

### On voit ce qu'on déplace

- **Un bloc de texte suit le pointeur pendant qu'on le déplace** : il est
  réellement réécrit en cours de geste, au plus une fois toutes les 70 ms.
  On voyait jusque-là la boîte bouger seule et le texte sauter au
  relâchement.
- **Une signature posée aussi** : l'original s'efface le temps du geste et
  son dessin suit le pointeur, à la bonne taille. Plus de doublon ni de boîte
  vide.
- Le geste part toujours de la **boîte d'origine** : sans cela, réécrire en
  cours de route faisait s'emballer le mouvement.
- Le bloc et sa boîte **restent dans la page**, et le glissement garde les
  repères de **sa** page même si le pointeur passe sur la suivante.
- Tout un déplacement ne fait qu'**une** opération d'annulation.

## 0.14.0 — 20 septembre 2026

### Ce qu'on pose devient un objet

- **Une signature posée se sélectionne, se déplace et se redimensionne** :
  huit poignées, comme pour un bloc de texte. C'est le rectangle de
  l'annotation qui change, et son dessin s'y remet à l'échelle — donc une
  signature agrandie reste nette.
- **L'outil revient au déplacement après chaque pose**, comme dans Acrobat :
  le panneau a désormais une entrée **« Déplacer »**, qui s'allume toute
  seule une fois l'élément posé, et l'élément en question est déjà choisi.
  Plus de pose en rafale involontaire.
- **On peut aussi glisser une signature** depuis le panneau jusqu'à l'endroit
  voulu, au lieu de la choisir puis de cliquer.
- En mode déplacement, **un clic sur un élément déjà posé le reprend**.

## 0.13.0 — 20 septembre 2026

### Paramètres

- **Les mises à jour s'y règlent** : version installée, résultat de la
  dernière recherche, « Rechercher maintenant », « Installer » quand une
  version attend, et la recherche au démarrage qui s'active ou se coupe. Rien
  n'est téléchargé sans accord, et rien n'est contacté quand la recherche est
  coupée.
- Les paramètres sont maintenant un **menu** : Langue, Mises à jour.
- **L'icône change** : deux curseurs de réglage au lieu d'une roue dentée,
  qu'on confondait avec le soleil du thème juste à côté.

## 0.12.0 — 20 septembre 2026

### Déplacer et redimensionner, comme dans Acrobat

- Dans « Modifier le PDF », **un clic sélectionne un bloc** : il s'encadre et
  reçoit huit poignées. On le **glisse** pour le déplacer, on tire une
  **poignée** pour changer sa boîte — et le texte **reflue** dans la nouvelle
  largeur au lieu de s'étirer. Un **double-clic** entre dans le texte, comme
  avant.
- **Repères d'alignement** : pendant le déplacement, le bloc s'aimante aux
  bords des autres blocs de la page (à quatre points près) et une ligne fine
  montre sur quoi il s'aligne.
- Le bloc **reste dans la page** : on ne peut plus le pousser dehors.
- Le déplacement est une opération d'historique comme les autres : Ctrl+Z le
  défait.

### Paramètres

- **Un engrenage** dans la barre du haut ouvre les paramètres — la langue
  pour l'instant. La palette y mène aussi.

## 0.11.0 — 20 septembre 2026

### On peut taper plus d'une lettre

- **Une zone de texte neuve se tapait lettre par lettre… et s'arrêtait à la
  première.** Le bloc était retrouvé, à chaque frappe, par le texte qu'il
  porte ; dès qu'il touchait un voisin — un libellé à gauche, un filigrane
  par-dessus, la ligne du dessous quand le texte déborde — l'extraction les
  lisait ensemble et la frappe était refusée. Deux corrections :
  - une **ancre** est posée dans le flux autour de chaque bloc qu'Acrux
    écrit ; il se retrouve par elle, quoi qu'en dise l'extraction ;
  - à défaut d'ancre (première frappe), les mots sont **appariés un à un**
    depuis la boîte, ce qui ignore le texte voisin.
- **La police d'un XObject** n'était pas cherchée au bon endroit : modifier
  un texte qui y vit échouait dès la deuxième frappe (« police /F4
  introuvable »). Les polices se cherchent maintenant dans les ressources du
  flux réécrit.
- Un test tape « Xyz » **dans chaque bloc du corpus**, lettre à lettre, et
  échoue au moindre refus : 43 blocs, aucun refus.

### Langue

- **Acrux parle français ou anglais**, selon la langue du système au premier
  lancement. Le choix se change dans **Paramètres** (palette de commandes,
  `Ctrl+Maj+P`) : Système, Français, English — et il est retenu.
- Ce qui n'est pas encore traduit reste en français, ce qui se lit toujours.

## 0.10.0 — 20 septembre 2026

### L'accueil se tient

- **Les vignettes s'affichaient vides** quand un document était ouvert
  derrière : elles ne se calculaient que sans document. Corrigé.
- **Les cartes et le bouton « Ouvrir » ne répondaient pas** dans le même
  cas : le clic filait au document caché. Corrigé.
- **La molette fait défiler l'accueil**, plus le document caché ; les cartes
  qui dépassent se rejoignent en glissant.
- **Cliquer un document déjà ouvert y retourne** au lieu d'en faire un
  second onglet.
- **Échap** revient au document.
- Sur l'accueil, la barre du haut et la colonne d'outils **se taisent** (rien
  à commander sans document affiché), et la barre d'état annonce l'accueil et
  le nombre de documents récents au lieu de la page du document caché.

### Harmonisation

- Le **voile** des fenêtres modales était écrit deux fois ; il n'existe plus
  qu'une fois, dans le module de dessin.
- **Coins arrondis** aussi pour la fenêtre de signature (carte, onglets,
  zone de tracé, boutons), les barres de mode, l'invite de saisie, les
  info-bulles, les onglets de documents et les lignes du panneau latéral :
  toute l'interface parle désormais la même langue.

## 0.9.0 — 20 septembre 2026

### Correction

- Les chaînes binaires d'un PDF enregistré s'écrivent en **hexadécimal** dès
  qu'un octet sort de l'ASCII imprimable, au lieu d'échappements octaux de
  longueur variable. Deux enregistrements du même document donnent ainsi deux
  fichiers de même taille — ce que l'identifiant `/ID`, tiré au hasard,
  mettait en défaut une fois sur deux (un test l'a attrapé).

### Le texte des formulaires se modifie enfin

- Beaucoup de documents — ceux des traitements de texte en particulier —
  n'écrivent pas leur texte dans la page mais dans un **XObject de
  formulaire** que la page appelle. Acrux refusait de les modifier ; il
  rouvre désormais ce flux comme s'il était la page et le réécrit. Sur le
  corpus, la part du texte modifiable passe de **88 % à 94 %**.
- Un test mesure cette couverture et **refuse de la laisser redescendre**.
- Restent à traiter : les blocs en biais (filigranes) et les rares
  opérations qui dessinent deux textes à la fois.

### Accueil

- **Un bouton maison**, tout à gauche de la barre : il montre l'accueil et
  ses documents récents **sans fermer** ce qui est ouvert ; un second clic
  revient au document. La commande « Accueil » existe aussi dans la palette.

## 0.8.0 — 20 septembre 2026

Une interface finie, et du texte qui se laisse modifier partout.

### On modifie aussi les lignes que les blocs refusaient

- Quand un bloc entier ne peut pas être recomposé — ses lignes sont mêlées à
  d'autres dans le flux du document, ce qui arrive souvent aux PDF produits
  par un traitement de texte —, **la ligne cliquée s'ouvre seule**. Mieux vaut
  modifier une ligne que rien du tout.
- Le survol signale aussi les lignes qu'aucun bloc ne couvre : elles se
  modifient comme le reste.

### L'aspect

- **Coins arrondis, ombres douces, aplats lissés** partout où l'interface
  pose une surface : fenêtres de question, panneau des signatures, cartes de
  l'accueil, pastilles de la colonne d'outils, survols de la barre du haut,
  ligne choisie de la palette. Le dessin passe par une fonction de distance,
  donc le lissage est exact jusque dans les angles.

### Les animations

- **Le défilement glisse** au lieu de sauter : molette, flèches, Page
  précédente et suivante. Deux coups de molette de suite s'additionnent sans
  à-coup, et le document s'arrête net en haut et en bas.
- **Les panneaux viennent du bord** : la colonne d'outils (F3), le panneau
  des vignettes et celui des signatures s'ouvrent et se ferment en glissant.
- **Les fenêtres de question apparaissent en fondu**, en montant de quelques
  pixels.
- **Les vignettes de l'accueil naissent du blanc de la page** à mesure
  qu'elles sont calculées.

## 0.7.1 — 20 septembre 2026

### Corrections

- **« Refaire » et « Retirer » répondent enfin** : les deux boutons sont posés
  sur la carte de la signature, qui couvre toute la largeur du panneau ; c'est
  elle qui prenait le clic. Le dernier élément dessiné l'emporte désormais.
- **La fenêtre suit le thème** : la barre de titre, son texte et la bordure
  prennent les couleurs de l'application — plus de bandeau blanc au-dessus
  d'une interface sombre. Le basculement de thème (T) les change aussi.

## 0.7.0 — 19 septembre 2026

Voir ce qu'on signe, et voir ce qu'on ouvre.

### Un panneau pour les signatures

- **« Remplir et signer » ouvre un panneau à gauche**, comme dans Acrobat, à
  la place de la barre du haut : on y **voit** ses signatures, dessinées par
  le code qui les écrira dans le PDF.
- **Plusieurs signatures** (trois) et un paraphe, qu'on peut enfin **gérer** :
  en ajouter une, en **refaire** une, en **retirer** une. La liste survit d'une
  session à l'autre.
- Le panneau rassemble aussi ce qu'on ajoute (texte, stylo, les cinq marques)
  et l'encre (couleur, épaisseur, pointe) : tout est nommé, plus rien à
  deviner.

### La signature se pose là où l'on clique

- **Un aperçu suit le pointeur** et montre exactement ce que le clic posera.
- **Ce qu'on pose se centre sur le pointeur** : on visait un coin invisible,
  la signature tombait à côté.

### Écran d'accueil

- **Les documents récents deviennent des cartes** avec la **vignette de leur
  première page**, leur taille et leur date : on reconnaît un document d'un
  coup d'œil au lieu de lire des noms de fichiers. Les vignettes se calculent
  une par une, sans bloquer la fenêtre.
- Un bouton **« Ouvrir un document… »** remplace le rappel de raccourci.

## 0.6.0 — 19 septembre 2026

Remplir un formulaire imprimé, comme on le ferait au stylo.

### Remplir et signer, revu de fond en comble

- **L'encre est noire par défaut** — c'est avec quoi l'on signe un papier —
  avec le bleu, le rouge et le vert à un clic.
- **Trois pointes et trois épaisseurs** : stylo, plume, feutre, en fin, moyen
  ou épais. Le choix se voit dans l'aperçu de la signature avant d'être posé,
  et il est retenu d'une session à l'autre.
- **Un stylo pour écrire directement sur la page** : on choisit « Dessiner »
  et l'on trace — une croix dans une case, un trait dans la marge, un
  paraphe. Le trait se dessine sous le pointeur tel qu'il sera enregistré.
- **Le texte se tape sur la page**, à l'endroit cliqué, au lieu de passer par
  une boîte de dialogue : on clique sur une ligne du formulaire, on tape, on
  clique sur la suivante. Le texte prend la police du document et se pose sur
  le trait du champ.
- **La fenêtre de signature** garde la ligne de base, montre le trait dans
  l'encre choisie, et sait défaire le dernier trait (bouton ou Ctrl+Z).
  Entrée applique.
- **Des pointeurs** pour le stylo et pour la pose.

### Modifier le PDF

- **Un clic dans une zone vide se voit** : le trait sur lequel le texte va
  s'écrire apparaît sous le curseur. Sans lui, cliquer dans du blanc semblait
  ne rien faire.

## 0.5.0 — 19 septembre 2026

Acrux ne demande plus rien au système : ses questions, ses pointeurs et ses
cadres d'édition sont à lui.

### Acrux pose ses propres questions

- **Plus de boîte de message du système.** « Enregistrer les modifications ? »
  s'affiche dans la fenêtre, dans le thème de l'application, et **nomme ce que
  fait chaque bouton** : Enregistrer, Ne pas enregistrer, Annuler — au lieu
  d'un « Oui / Non » qui laisse deviner. Entrée valide, Échap annule, Tab
  passe d'un bouton à l'autre. Les messages d'erreur, la suppression d'une
  page, l'application des biffures et l'installation d'une mise à jour passent
  par la même fenêtre.
- **Ouvrir un document ne demande plus rien** : le document en cours passe
  simplement dans un onglet, il n'y avait rien à perdre.

### Modifier le PDF : plus proche encore d'Acrobat

- **Plus de cadres partout.** La page reste la page ; seul le bloc survolé se
  signale d'un filet discret, et le bloc en cours d'un filet fin.
- **Une zone de texte neuve écrit dans la police, le corps et la couleur du
  texte voisin**, au lieu d'arriver en Helvetica — une valeur ajoutée à un
  formulaire imprimé ressemble au reste de la page. Le corps et la couleur
  choisis dans la barre l'emportent, s'ils ont été choisis.
- **Le texte se pose sur la ligne du champ.** Un clic sur un trait de
  formulaire (« Intitulé du compte : ______ ») écrit dessus, et non au-dessus
  ou en dessous.
- **Un clic dans une zone vide de la page suffit pour écrire** : plus besoin de
  reprendre « Ajouter du texte » à chaque valeur. Rien n'est écrit tant
  qu'aucune lettre n'est tapée.
- Une zone posée juste après un libellé, sur la même ligne, reste modifiable
  frappe après frappe : c'est bien elle qu'on recompose, le libellé ne bouge
  pas.
- Un paragraphe justifié de **deux** lignes est reconnu comme justifié : la
  première frappe ne le met plus en drapeau.

### Pointeurs

- **Des pointeurs dessinés par Acrux**, comme dans Acrobat : ajouter du texte,
  surligneur, note, biffure, déplacement. Le pointeur dit ce que fera le clic
  avant qu'on le fasse.

## 0.4.0 — 19 septembre 2026

On choisit un outil, on entre dans un mode, et c'est la page elle-même qui
devient l'outil — sans sélectionner d'abord.

### Modifier le PDF, comme dans Acrobat

- **On entre dans le mode, on clique, on tape.** Plus besoin de sélectionner
  d'abord : « Modifier le PDF » encadre tout le texte de la page, un clic dans
  un bloc y pose le curseur, et l'on édite comme dans un traitement de texte.
  Le bloc se recompose **à chaque frappe, dans sa vraie police et sur son vrai
  fond** : ce qu'on voit est ce qui sera enregistré. Retrait de première
  ligne, justification et interligne sont conservés.
- **Ajouter du texte** pose une zone neuve où l'on clique, dans la taille et la
  couleur choisies dans la barre du mode.
- **Annuler** défait toute une saisie d'un coup.
- Un paragraphe qui continue dans la colonne suivante, ou une ligne qui porte
  un titre à gauche et une date à droite, se modifient en blocs séparés —
  comme dans Acrobat, et pour la même raison : les recomposer d'un seul tenant
  coulerait le texte par-dessus l'autre colonne.
- **Surligner, Poser une note et Biffer sont devenus des outils** : on les
  choisit dans la colonne de droite, puis on glisse sur le texte ou on clique
  sur la page. Une barre dit l'outil en cours et comment en sortir.

## 0.3.0 — 19 septembre 2026

Une version d'interface : ce que le logiciel sait faire se voit enfin, et les
documents s'ouvrent à une taille qu'on peut lire.

### Interface

- **Barre des outils, à droite.** Tout ce qui sert à travailler — modifier le
  texte et les objets, surligner, poser une note, remplir et signer, pivoter,
  insérer, dupliquer, extraire ou supprimer une page, biffer, exporter,
  joindre, imprimer — est désormais nommé dans une colonne, au lieu d'être
  caché derrière des raccourcis. L'outil en cours s'y allume. `F3` l'ouvre et
  la ferme, et elle s'efface d'elle-même sur une fenêtre trop étroite.
- **Le zoom d'ouverture ne grossit plus les documents.** Ajuster à la largeur
  sur un écran large donnait 212 % et un corps de texte de deux centimètres.
  Le nouveau mode **automatique** ajuste à la largeur sans jamais dépasser
  100 %, comme Acrobat : une page plus étroite que la fenêtre s'affiche à sa
  taille réelle, centrée. Le bouton d'ajustement parcourt les trois modes —
  automatique, largeur, page entière — et la barre d'état dit lequel est actif.

## 0.2.0 — 19 septembre 2026

Cette version a deux sujets : **le multimédia**, qui n'existait pas, et **la
fidélité des polices**, qui avait des trous dont un seul se voyait.

### Multimédia

- **Les vidéos se lisent dans la page.** Les trois formes du multimédia PDF
  sont reconnues — `/Screen` avec action `/Rendition` (§13.2), `/RichMedia`
  (§13.7) et le `/Movie` de PDF 1.1 —, en descendant action → rendition →
  clip → spécification de fichier. Le décodage revient au système (Media
  Foundation) ; ce qui nous appartient, c'est que les images sont **composées
  dans la page** : la vidéo suit le défilement, le zoom et la découpe, sans
  fenêtre flottante posée par-dessus. Le son donne l'heure — la position vient
  de ce que la carte son déclare avoir joué, pas de l'horloge de la machine.
- **Le son seul**, y compris l'annotation `/Sound` (§13.3), qui ne porte pas un
  fichier mais des échantillons bruts. `acr media --extract` leur rend un
  en-tête WAV : octets retournés (le PDF est gros-boutien, le WAV
  petit-boutien), non signé recentré, lois µ et A de la téléphonie décodées en
  PCM 16 bits. Un média sans image reçoit un panneau d'onde et ses commandes.
- `acr media` liste les vidéos et les sons d'un document avec leur rectangle,
  leur type et leur taille.
- Un média **extérieur** au document — un chemin de disque, une adresse
  réseau — est listé mais jamais ouvert de lui-même. Le nom du fichier extrait
  est choisi par nous, jamais repris du document.
- Contrôle en amont : PDF/A et PDF/X refusent désormais le multimédia.

### Polices

- **Les quatorze polices standard ont leurs métriques.** Un PDF a le droit
  d'écrire `/BaseFont /Helvetica` sans largeurs ni descripteur : le lecteur est
  censé connaître Helvetica. Les tables AFM d'Adobe sont maintenant dans le
  moteur, une seule fois pour tout le projet. Sans elles, le même fichier ne se
  composait pas pareil d'une machine à l'autre.
- **Les polices installées sont retrouvées par leur nom.** On devinait des noms
  de fichiers, ce qui ne marche que sur Windows et y ratait presque tout : un
  document réclamant Calibri, Consolas ou Cambria recevait Arial. On lit
  désormais la table `name` de chaque police — quelques kilo-octets par
  fichier, jamais le fichier entier.
- **Un glyphe de substitution occupe la largeur déclarée.** Une police de
  remplacement n'a pas les chasses de l'absente ; dessinée telle quelle, chaque
  lettre flottait dans la place que le document lui réservait.
- **Une police de secours dessinée par nous**, pour les machines sans aucune
  police — un conteneur, une image de compilation minimale — où la page restait
  blanche alors que le fichier était valide. Elle ne sert qu'en dernier
  recours : tant qu'il y a de vraies polices, c'est l'une d'elles qui sert.
- Symbol garde son encodage propre : au code 0x61 il y a `alpha`, pas `a`.

### Corrections

- Trois erreurs dans les métriques standard, trouvées en les recoupant avec les
  polices du système : le « i » accentué se bâtit sur le « i » sans point, le
  `quotedblbase` de Times-Italic et le `œ` d'Helvetica-Bold. Huit glyphes
  manquaient (`Ð Þ ð þ × ÷ ø ß`).
- Un média sans piste vidéo ne se terminait jamais, et le reliquat de son en
  fin de flux n'était jamais joué : la dernière fraction de seconde manquait.
- Le réveil de la boucle d'événements repartait sans rien redessiner, et la
  vidéo restait figée sur sa première image alors que le lecteur tournait.

## 0.1.0 — 19 septembre 2026

Première version publiée : lecture, rendu, édition de texte et d'objets,
formulaires, signatures, accessibilité, contrôle en amont, remplir et signer,
comparaison, export. Installateur sans droits d'administrateur, archive
portable, et recherche de mise à jour.
