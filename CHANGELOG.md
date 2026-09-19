# Journal des versions

Les numéros suivent [SemVer](https://semver.org/lang/fr/) : tant qu'Acrux est
en `0.x`, le deuxième nombre change quand des fonctions apparaissent, le
troisième quand on ne fait que corriger.

Chaque version est publiée par une étiquette `vX.Y.Z` poussée sur le dépôt ;
c'est la section correspondante de ce fichier qui devient la page de version.

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
