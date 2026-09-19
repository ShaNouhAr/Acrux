# Journal des versions

Les numéros suivent [SemVer](https://semver.org/lang/fr/) : tant qu'Acrux est
en `0.x`, le deuxième nombre change quand des fonctions apparaissent, le
troisième quand on ne fait que corriger.

Chaque version est publiée par une étiquette `vX.Y.Z` poussée sur le dépôt ;
c'est la section correspondante de ce fichier qui devient la page de version.

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
