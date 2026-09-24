# Journal des versions

Les numéros suivent [SemVer](https://semver.org/lang/fr/) : tant qu'Acrux est
en `0.x`, le deuxième nombre change quand des fonctions apparaissent, le
troisième quand on ne fait que corriger.

Chaque version est publiée par une étiquette `vX.Y.Z` poussée sur le dépôt ;
c'est la section correspondante de ce fichier qui devient la page de version.

## 0.23.0 — non publiée

- **Ctrl+molette zoome vers le pointeur**, comme dans Acrobat : ce qui est sous
  la souris y reste, au pixel près. Avant, seul le centre gardait sa hauteur et
  la page glissait de côté à chaque cran — viser un coin de tableau le faisait
  sortir de l'écran. Les boutons −/+, les touches, la palette et la liste
  gardent, eux, le centre de la vue ; les ajustements aussi, et « Ajuster à la
  page » montre la page courante en entier. Sur un pavé tactile, les petits
  crans s'additionnent au lieu d'être perdus.
- **La case du zoom se déroule.** Le « 125 % » de la barre d'outils, jusque-là
  inerte, ouvre une liste : 50, 75, 100, 125, 150, 200, 300 et 400 %,
  l'ajustement automatique, à la largeur, à la page, et un champ où taper
  n'importe quel niveau (« 137 », « 137,5 % »), validé par Entrée — une saisie
  illisible s'entoure de rouge. Le niveau en vigueur est coché et la liste
  s'ouvre dessus ; flèches, Entrée et Échap s'y emploient, et la case
  s'atteint au clavier (F6 puis les flèches). Palette : « Choisir le niveau de
  zoom… ». Sans document, la case montre « – % » grisé au lieu d'un trou entre
  les deux loupes.
- **La barre d'état se clique.** La page prend le champ de page (comme
  Ctrl+G), le zoom déroule la même liste au-dessus de lui, la disposition passe
  à la suivante ; chacun s'éclaire au survol, avec une info-bulle placée
  au-dessus de la barre au lieu de la couvrir. La durée de rendu « 23 ms »,
  une mesure de développement, quitte la barre pour le journal.
- **Zoomer ne fait plus clignoter la page en blanc** : en attendant son rendu
  à la nouvelle échelle, la page est montrée étirée depuis l'image
  précédente, puis nette dès que le rendu arrive.
- **Le zoom ne peut plus épuiser la mémoire.** Une page est rendue d'un bloc :
  Ctrl+molette montait jusqu'à 1600 %, soit deux gigaoctets pour une page A4
  sur un écran à 150 %. Le zoom est désormais plafonné pour qu'une page rendue
  tienne dans 256 Mo (environ 570 % pour de l'A4 sur un écran à 150 %, bien
  plus pour un petit format), et la barre d'état le dit quand on bute dessus.
  Le plafond tient aussi pour un zoom venu d'ailleurs — les réglages
  enregistrés, un autre onglet aux pages plus petites, un écran plus dense —
  et pour l'ajustement à la largeur d'une page très haute. Le cache des pages
  rendues est aussi borné en octets (96 Mo au-delà des pages affichées), plus
  seulement en nombre d'images : revenu à 100 %, l'image d'une page vue à
  400 % ne reste plus en mémoire.
- « Afficher la page entière » s'appelle « Ajuster à la page », à côté
  d'« Ajuster à la largeur ».
- Le harnais invisible gagne `Wheel … -Ctrl` (Ctrl+molette), les fractions de
  cran (`Wheel x y 0.25`) et `ShotNow`, une capture sans attendre les rendus.
- **Annuler et Rétablir dans la barre d'outils**, en tête du groupe des
  modifications : grisés quand il n'y a rien à défaire ou à refaire, ils
  s'allument dès la première frappe dans « Modifier le PDF ». Comme Ctrl+Z, ils
  défont d'abord **dans le bloc en cours de saisie**, étape par étape ; la
  palette et le clic droit font désormais de même, alors qu'ils refermaient le
  bloc et emportaient toute la frappe d'un coup.
- **Les bascules disent leur état.** Les boutons du panneau latéral et des
  outils s'allument (fond et icône d'accent) quand ce qu'ils montrent est
  affiché. Le bouton du thème montre où l'on va : la lune en thème clair, le
  soleil en sombre, avec « Passer au thème sombre (T) » en info-bulle ; celles
  des bascules disent aussi ce que le clic fera (« Masquer le panneau
  latéral », « Tous les outils »).
- **Enregistrer est une disquette** : la flèche sur un bac se lisait
  « télécharger ».
- **Un bouton grisé se voit grisé**, nettement plus pâle qu'avant, et n'a plus
  d'info-bulle qui promettait une action refusée au clic. Au clavier, Entrée ne
  déclenche plus un bouton devenu grisé (Annuler, une fois tout annulé), et la
  flèche repart de sa place au lieu de sauter au bord de la barre.
- **Le champ de page est encadré** au repos — on voit qu'on peut y taper — et
  dit au survol « Aller à une page (Ctrl+G) ». Atteint au clavier (F6 puis les
  flèches), il porte enfin l'anneau de focus ; celui des boutons est arrondi
  comme eux.
- **Fenêtre étroite** : les boutons secondaires (disposition, ajustement,
  imprimer, pivoter, accueil, puis le zoom et les pages, par paires)
  s'effacent au lieu de repousser la recherche, les outils, les paramètres et
  le thème hors de la fenêtre. Quand la colonne des outils n'y tient plus, son
  bouton se grise au lieu de basculer la préférence sans rien montrer.
- **Onglets** : le survol montre le chemin complet du document (deux
  « facture.pdf » se distinguent enfin) et « modifications non enregistrées »
  s'il y a lieu ; la croix dit « Fermer l'onglet », sous la croix survolée même
  quand on passe directement de l'une à l'autre. Le **clic du milieu ferme
  l'onglet**, en proposant d'abord d'enregistrer un document modifié ; il ne
  fait plus glisser la page quand on l'emploie dans les barres.
- Le harnais invisible gagne `MiddleClick` et `Resize` (fenêtre étroite, sans
  jamais l'afficher).
- **Sécurité : la clé d'un document protégé ne se devine plus.** Jusqu'ici, la
  clé AES-256, les sels et `/Perms` sortaient d'un générateur semé par « taille
  du fichier ^ heure » : qui connaissait l'heure d'enregistrement retrouvait la
  clé sans le mot de passe. Ils viennent maintenant d'un générateur
  cryptographique écrit ici, **ChaCha20** (RFC 8439, vecteurs officiels
  vérifiés), semé par l'entropie du système — celle que la bibliothèque
  standard tire de Windows, recueillie sur plusieurs fils, et, dans
  l'application, `BCryptGenRandom` en renfort. Même générateur pour les IV,
  l'identifiant `/ID` et la génération de clés RSA
  (`acrux_document::crypt::random`). Deux protections du même fichier ne
  partagent plus rien.
- **Protéger par mot de passe : deux mots de passe et de vraies permissions.**
  La fenêtre (palette : « Protéger par mot de passe ») demande un mot de passe
  **d'ouverture**, facultatif, et un mot de passe **des permissions** ; chacun se
  tape deux fois, et une jauge dit sa force (faible, moyen, bon, fort) pendant
  la frappe. On y règle l'impression (non, basse résolution, haute
  résolution), la modification, la copie, les commentaires, le remplissage des
  formulaires, l'extraction pour l'accessibilité et l'assemblage. Avant, un
  seul mot de passe servait aux deux : quiconque ouvrait le document avait tous
  les droits.
- **Les permissions d'un document protégé sont respectées.** Ouvert avec le seul
  mot de passe d'ouverture, un document dit à l'ouverture ce qu'il interdit ;
  l'impression refusée ne s'ouvre pas, la « basse résolution » imprime à
  150 ppp, la copie et l'export sont refusés sans la permission de copier, et
  chaque modification exige son droit (assembler pour pivoter ou supprimer une
  page, commenter pour annoter, remplir pour les champs, modifier pour le
  reste). Chaque refus propose de saisir le mot de passe des permissions, qui
  lève tout. Sans la permission de copier, ni « Extraire la page » ni
  « Insérer des pages » (depuis un document à ouverture libre) ne recopient
  son contenu dans un fichier sans protection, et Ctrl+C / Ctrl+X restent
  refusés dans « Modifier le PDF ». Supprimer une page ou appliquer les
  biffures est refusé avant la confirmation, et non après.
- **Seul le mot de passe des permissions change ou retire la protection**, dans
  l'application comme avec `acr unprotect` : un simple lecteur ne peut plus
  retirer les restrictions en reprotégeant le document. Les fichiers protégés
  par les versions précédentes (même mot de passe pour tout) s'ouvrent toujours
  avec tous les droits.
- Un `/P` retouché à la main pour s'accorder des droits est repéré :
  `/Perms`, scellé par la clé, est vérifié (algorithme 13) et c'est la valeur la
  plus stricte qui s'applique.
- **`acr protect`** : `--print none|low|high`, `--no-fill`,
  `--no-accessibility` et `--no-assemble` rejoignent les options, et affiche
  la force des mots de passe. **Changement** : chaque option ne retire plus que
  sa permission — `--no-copy` laisse l'extraction pour l'accessibilité,
  `--no-modify` laisse l'assemblage, `--no-annotate` laisse le remplissage (à
  retirer avec `--no-fill`). Des restrictions sans `--owner` distinct de
  `--user` sont refusées : elles ne protégeraient rien.
- **`acr info`** dit le chiffrement (« AES-256 (R6) »), l'accès (propriétaire
  ou utilisateur), les permissions et le niveau d'impression, et l'identifiant
  de version. `--password` donne enfin les droits du propriétaire sur un
  document qui s'ouvre sans mot de passe (`acr unprotect` échouait).
- Les questions d'Acrux finissent d'apparaître d'elles-mêmes : ouvertes au
  clavier, elles pouvaient rester à demi transparentes jusqu'au prochain
  mouvement de la souris.
- **« Paramètres » devient une vraie fiche.** La langue, l'apparence (clair ou
  sombre, qui n'y figurait pas) et les mises à jour tiennent sur une seule
  carte : chaque réglage est un groupe de cases dont la case allumée dit l'état,
  et un clic le change sur-le-champ — la fiche se repeint aussitôt dans la
  langue ou le thème choisi. On y lit la version installée et l'issue de la
  dernière recherche, avec « Rechercher maintenant » et, quand une version est
  disponible, « Installer ». Avant, c'était une question à icône « ? » qui
  menait à d'autres questions. Au clavier : Tab d'un groupe à l'autre, les
  flèches changent la valeur, Entrée ou Échap ferment.
- **Une invite ouverte garde pour elle le clavier et la souris.** Ctrl+W fermait
  l'onglet sous l'invite du mot de passe, Ctrl+Maj+P ouvrait la palette
  par-dessus, la molette faisait défiler la page dessous, et une invite ouverte
  alors que la barre d'outils avait le focus (F6) ne recevait plus rien de ce
  qu'on tapait. Seuls son champ et ses boutons répondent désormais ; Ctrl+V
  colle toujours dans le champ.
- **Les boutons agissent au relâchement**, pointeur dessus, et non plus à
  l'appui : dans les questions, les invites, la fiche « Paramètres », la
  fenêtre « Protéger par mot de passe », la carte de recherche et la palette de
  commandes. On se ravise en glissant hors du bouton, comme partout sous
  Windows, et un bouton enfoncé se voit. Un clic dans le champ de la palette ne
  la ferme plus.
- **Les invites de saisie** (note, mot de passe, champ de formulaire, peigne,
  commentaire…) **prennent la carte des questions** : même titre, mêmes
  marges, même fondu. Leur libellé se coupe sur plusieurs lignes au lieu d'être
  tronqué. « Valider » passe avant « Annuler », groupés à droite comme sous
  Windows — la carte de recherche (« Remplacer », « Tout remplacer ») et
  « Protéger par mot de passe » suivent le même ordre. Survol, pointeur main,
  et Tab qui va du champ aux boutons : Entrée ou Espace pressent celui qui a le
  focus. Une saisie refusée (mauvais mot de passe) remet le focus dans le champ.
- **Carte de recherche** : Tab atteint aussi « Remplacer » et « Tout remplacer »,
  qui s'éclairent au survol ; la carte a le rayon et l'ombre des autres, et un
  clic sur elle, hors des champs, ne sélectionne plus le texte de la page
  dessous.
- **L'anneau de focus se voit autour du bouton principal** : il s'écarte de deux
  pixels du bouton au lieu de s'y coller, et ne se confond plus avec lui (même
  couleur d'accent). **En thème sombre, le survol éclaircit** un bouton
  secondaire au lieu de l'assombrir, comme sous Windows 11 — de même pour les
  cases des contrôles segmentés.
- **Ombres** : une bande claire soulignait chaque carte (questions, invites,
  palette, info-bulles, cartes de l'accueil) — l'ombre, décalée vers le bas,
  laissait son intérieur intact. Elle est maintenant pleine sous la carte.
- Les fondus d'apparition se terminent toujours sur une image opaque, même si la
  première peinture tarde, et pour toutes les cartes (invites, « Paramètres »,
  « Protéger ») : le fil des animations est armé dès l'événement qui ouvre la
  carte, et se tait une fois l'image finale peinte.
- Le résultat d'une recherche de mise à jour s'affiche dès qu'il arrive ; il
  attendait jusqu'ici le prochain mouvement de souris.
- Les titres et libellés des invites, et la question « Enregistrer les
  modifications ? » à la fermeture d'un onglet, suivent la langue de
  l'interface.
- La barre d'espace qui presse le bouton d'une carte ne va plus, ensuite, au
  document : valider à l'espace l'invite d'un champ de formulaire la rouvrait
  aussitôt, l'espace réactivant le champ qui avait le focus. La fiche
  « Paramètres » garde aussi la même hauteur quand on change la recherche des
  mises à jour, au lieu de sauter de quelques pixels sous le pointeur. Le
  harnais invisible relâche ses touches comme le vrai clavier : il tapait deux
  espaces pour une barre d'espace.

- **La palette de commandes défile, et Entrée ne lance plus une commande que
  l'on ne voit pas.** Elle n'affichait que douze lignes d'une liste de
  cinquante-cinq : la flèche descendait plus bas que la carte, et Entrée lançait
  une commande invisible. La liste suit désormais la sélection ; un ascenseur fin
  dit où l'on est et se saisit à la souris ; la molette la fait défiler, trois
  lignes par cran ; Page préc. / Page suiv. avancent d'un écran ; Ctrl+Origine
  et Ctrl+Fin (Origine et Fin seules quand le champ est vide) vont aux deux
  bouts. Dans une fenêtre basse, la carte tient dans la fenêtre.
- **Les commandes dans l'ordre de l'usage**, comme dans Acrobat : Ouvrir,
  Enregistrer, Rechercher, Imprimer, Remplir et signer, Modifier le PDF, Poser
  une note… — « Protéger » et « Retirer la protection » ouvraient la liste, et
  « Paramètres » la ferme maintenant. **Les cinq dernières commandes lancées
  depuis la palette viennent en tête**, séparées du reste par un trait, et sont
  retenues d'une séance à l'autre (`commande-recente=` dans les préférences).
- **Les lettres trouvées s'éclairent** dans la couleur d'accent : taper « enrs »
  montre pourquoi « Enregistrer sous » est là. Les synonymes, qu'on ne voit pas,
  ne se trouvent plus que d'un seul tenant : « enrs » faisait monter vingt
  commandes, dont « Accueil » et « Aller à une page », parce que leurs synonymes
  contenaient ces lettres dans l'ordre ; il en reste deux. Le champ ne saute
  plus à chaque frappe : la carte raccourcit par le bas.
- **La palette parle anglais.** Quarante libellés restaient en français dans
  l'interface anglaise, avec le texte d'invite, « Aucune commande » et les
  raccourcis (« Ctrl+Maj+S » s'écrit « Ctrl+Shift+S »). On cherche dans ce qui
  est affiché — « open » trouve « Open a document » —, et le libellé français se
  trouve encore. « Annuler » et « Rétablir » deviennent « Annuler la dernière
  action » et « Rétablir l'action annulée » : le même mot servait aux boutons
  « Annuler » (« Cancel »). Les info-bulles de la barre et de la colonne
  d'outils, qui reprennent ces textes, suivent.
- **La molette ne fait plus défiler le document derrière la palette**, ni ne le
  zoome (Ctrl+molette), ni derrière la fenêtre de capture d'une signature : elle
  va à la liste de la palette, comme elle allait déjà à la liste des polices.
- **La molette vise ce qui est sous le pointeur.** Windows donne sa position en
  coordonnées d'écran, qu'Acrux prenait pour une position dans la fenêtre : dès
  que la fenêtre n'était pas collée au coin de l'écran, la molette sur le
  panneau des vignettes, sur la colonne d'outils ou sur un modèle 3D faisait
  défiler le document.
- Ctrl+V colle dans le champ de la palette ; il collait dans la recherche restée
  ouverte dessous, ou nulle part. Les autres raccourcis Ctrl s'arrêtent à la
  palette, comme à une invite : Ctrl+W fermait l'onglet dessous (la palette
  proposait ensuite « Enregistrer » sans document), Ctrl+Z défaisait une
  modification sous le voile. Ctrl+Maj+P la referme.
- Sur un pavé tactile, les petits crans de molette s'additionnent : un geste
  lent faisait défiler la palette, alors qu'il la laissait immobile.
- Harnais invisible : `Wheel x y crans` (la molette, en coordonnées d'écran
  comme le vrai Windows) et `KeyCtrl` (Ctrl+Fin, Ctrl+Origine).

- **Les lignes à remplir se reconnaissent** — « Nom : ____________ ». Dans
  « Remplir et signer » (et là seulement), la place du texte s'encadre
  au-dessus de la ligne survolée ; un clic y ouvre la saisie, **sans avoir à
  choisir l'outil Texte**, et le texte part du début de la ligne, posé dessus.
  Sont reconnues les lignes tracées d'un trait fin **et les suites de tirets
  bas tapées au clavier**, que l'outil texte ne savait pas viser ; un trait
  qui fait le bord d'un cadre n'en est pas une (`fillsign::boxes::field_lines`).

- Le texte d'une ligne à remplir se pose **au-dessus du trait**, jambages
  compris — et c'est vrai aussi du **deuxième clic** : tant qu'une zone de
  saisie était ouverte, le clic suivant passait par l'éditeur de texte, qui
  écrivait à l'endroit brut du clic, sur le trait. Il repasse maintenant par
  « Remplir et signer », qui sait viser la ligne, la case ou le peigne.
- **Ctrl+V colle dans les champs de saisie** : l'invite d'un peigne (un IBAN
  se copie d'ailleurs, il ne se retape pas — ses espaces sont ignorés à la
  répartition), une note, la recherche et le remplacement, la recherche d'une
  police, le code d'une couleur. Aucun champ ne savait coller jusqu'ici ; seul
  le texte de la page le pouvait.
- Le harnais invisible a son presse-papiers d'essai (`ACRUX_CLIPBOARD`), pour
  éprouver le collage sans toucher à celui de la personne.

- **Le clic droit ouvre un menu**, là où l'on travaille. Sur la page : Copier,
  Surligner la sélection, Poser une note ici (au point du clic, pas sous le
  menu), Tout sélectionner, Pivoter la page, Imprimer…, Propriétés du document.
  Sur une vignette du panneau : Pivoter, Dupliquer, Extraire…, Supprimer — la
  page **de la vignette**, pas celle qu'on regarde. Sur un onglet : Fermer,
  Fermer les autres onglets, Copier le chemin, Ouvrir le dossier du fichier —
  de **cet** onglet, même s'il n'est pas au premier plan. Sur un document récent
  de l'accueil : Ouvrir, Copier le chemin, Ouvrir le dossier du fichier,
  Retirer de la liste. Chaque élément lance la commande de la palette et
  affiche son raccourci ; ce qui ne s'applique pas est grisé (Copier sans
  sélection, Imprimer quand les permissions l'interdisent, Supprimer la
  dernière page).
- Le menu se conduit comme ceux de Windows : il s'ouvre au pointeur et bascule
  à gauche ou en haut quand la place manque ; flèches (en boucle, grisés et
  séparateurs sautés), Origine, Fin, Entrée, Échap ; l'initiale d'un élément
  le choisit ; on peut enfoncer le bouton droit, glisser jusqu'à l'élément et
  lâcher ; un clic dehors le referme sans rien faire d'autre — pas même
  désélectionner. **La touche « menu » du clavier et Maj+F10** l'ouvrent aussi,
  sur la page sous le pointeur ou sur la vignette qui a le focus dans le
  panneau.
- **Propriétés du document** (Ctrl+D, palette ou clic droit) : nom, dossier,
  taille, nombre de pages, version PDF, puis titre, auteur, sujet, mots-clés,
  application, convertisseur et dates, et la protection avec ce qu'elle
  interdit — ce que dit `acr metadata`, dans une fenêtre d'information.
- **Fermer les autres onglets** garde ceux qui ont des modifications non
  enregistrées, et le dit : on ne perd pas un document modifié d'un seul geste.
  **Copier le chemin du fichier** et **Ouvrir le dossier du fichier**
  (l'Explorateur s'ouvre sur le fichier sélectionné) sont aussi dans la
  palette. Un document de l'accueil se **retire de la liste** seul, sans vider
  tout l'historique ; le fichier n'est pas touché.
- **Relâcher le bouton droit n'interrompt plus un geste.** Pendant le tracé
  d'une signature, un trait d'encre, le déplacement d'un objet ou d'une
  signature posée, le relâchement du bouton droit terminait le geste comme
  celui du gauche.
- Les tailles de fichier de l'accueil s'écrivent « 1,5 Mo » en français et
  « 1.5 MB » en anglais (c'était « 1.5 Mo » dans les deux langues).
- Harnais invisible : `RightClick x y` et `ShiftF10`. Un lien suivi en mode
  invisible n'ouvre plus le navigateur de la personne (le journal note
  « adresse : … »), et « Ouvrir le dossier du fichier » n'ouvre pas
  l'Explorateur (« dossier : … »).
- Le mode invisible n'ouvre plus aucun dialogue du système sans réponse
  imposée : « Imprimer… » (sans `ACRUX_PRINTER`), l'ouverture ou
  l'enregistrement d'un fichier (« Extraire… », sans `ACRUX_OPEN_FILE` ni
  `ACRUX_SAVE_*`) et les confirmations (sans `ACRUX_CONFIRM`) sont tenus pour
  annulés, et le journal le note. « Imprimer… » du clic droit faisait surgir
  la fenêtre d'impression de Windows à l'écran pendant un essai.

## 0.22.0 — 20 septembre 2026

- **Les peignes se remplissent** — BIC, IBAN, date : ces rangées de cases
  alignées, une par caractère. Dans « Remplir et signer » (et là seulement),
  la main nue ou l'outil texte au-dessus d'un peigne, il s'encadre d'un bout à
  l'autre ; un clic demande le texte, qui se répartit **un caractère par case,
  centré dans la sienne**. C'est le mode « peigne » d'Acrobat, sans la poignée
  d'espacement à régler : les cases sont détectées (`fillsign::boxes::scan`,
  `Item::Comb`). Une rangée d'au moins trois cases serrées est un peigne — on
  n'y coche plus rien par mégarde — ; ses cellules peuvent être rectangulaires,
  seule une case à cocher doit être carrée. Le tout reste une annotation :
  déplaçable, supprimable, aplatissable.
- **Cocher sans rien choisir**, comme dans Acrobat : « Remplir et signer »
  ouvert, la main nue (« Déplacer »), une case dessinée s'encadre au survol
  avec la coche qu'un clic y poserait — et le clic la pose. Plus besoin d'aller
  chercher la ✓ dans le panneau ; on enchaîne les cases. Un clic sur une coche
  déjà posée la reprend en main, comme avant.
- Une **info-bulle ne reste plus plantée** : elle se réévalue à chaque
  mouvement, et un clic ou la sortie de l'accueil la fait taire. Celle d'un
  document récent survivait à l'ouverture du document.
- Les cases à cocher **tracées trait par trait** sont reconnues aussi : quatre
  filets qui ferment un carré font une case, même noyés dans le grand tracé
  qui dessine tout le cadre d'une page (la façon de Word et de bien des
  générateurs de formulaires). Les cellules d'un peigne (IBAN, date) en sont.
- Sur l'accueil, **survoler un document récent** affiche son chemin complet —
  que la carte tronque —, sa taille et sa date.
- « Remplir et signer » s'ouvre **sans rien en main** : plus de fenêtre de
  signature imposée à l'ouverture, ni de signature collée au pointeur. On y
  vient autant pour cocher une case que pour signer ; on choisit dans le
  panneau.

## 0.21.0 — 20 septembre 2026

- **Les cases à cocher dessinées se reconnaissent**, comme dans Acrobat. Un
  document qui n'est pas un formulaire — un papier numérisé, un export de
  traitement de texte — a ses cases tracées dans la page. Dans « Remplir et
  signer », avec la coche, la croix ou le point en main, la case **s'encadre
  au survol** et la marque **se cale dedans**, centrée et à sa taille, même si
  l'on clique de travers. La détection n'est qu'une suggestion : elle ne pose
  rien d'elle-même, et cliquer hors d'une case pose librement comme avant.
  Sont reconnus les petits tracés carrés cernés (5 à 40 points, contour ou
  fond clair — un carré plein et sombre est une puce) et les caractères
  ☐ □ ▢ ◻ ❏ ❐ ❑ ❒ (`acrux_features::fillsign::boxes`). Les cases des vrais
  formulaires (AcroForm) se cochaient déjà d'un clic.
- Une **marque reste en main** après la pose : on coche plusieurs cases à la
  suite sans revenir au panneau. Une signature, elle, se pose toujours une
  fois puis se laisse ajuster.
- **Vider l'historique** : sur l'écran d'accueil, un lien au bout de
  « Documents récents » efface la liste, après confirmation. Les fichiers
  eux-mêmes ne sont pas touchés.

## 0.20.0 — 20 septembre 2026

- La barre « Modifier le PDF » **dit la police du bloc ouvert** : « Georgia »,
  le gras allumé si le titre est gras — au lieu d'un « Police du texte » muet.
  Le nom vient du document (`ABCDEF+Georgia-Bold` se lit « Georgia », gras ;
  `sysfonts::describe`), et la liste des polices **s'ouvre sur elle**, mise en
  avant au milieu de la fenêtre. Retirer le gras d'un titre gras rend bien son
  romain.
- Le bouton de couleur montre un **« A » souligné de l'encre** du bloc, cerné
  d'un liseré clair : une pastille sombre sur une barre sombre ne se voyait pas.
- La **version** s'affiche en petit tout au bout de la barre d'état, en bas à
  droite : on sait d'un coup d'œil quelle version on a sous la main.

## 0.19.1 — 20 septembre 2026

### Correction

- **La liste des polices figeait la fenêtre.** Les noms se dessinent dans leur
  police quelques-uns par image ; pour demander l'image suivante, la boucle
  d'événements postait un réveil… à chaque réveil. La file de messages ne se
  vidait donc jamais, et Windows — qui ne repeint qu'une file vide — ne
  redessinait plus rien : six noms affichés, puis plus aucune réaction. Le
  réveil part maintenant de la peinture elle-même, un seul par image. Le mode
  invisible des tests, qui peint de façon synchrone, ne pouvait pas le voir :
  la correction a été vérifiée sur la vraie fenêtre.

## 0.19.0 — 20 septembre 2026

### Toutes les polices du système, comme dans un traitement de texte

- Le bouton de police de la barre « Modifier le PDF » faisait défiler quatre
  familles à chaque clic. Il déroule maintenant **la liste de toutes les
  polices installées** sur la machine — plusieurs centaines sur un Windows
  ordinaire —, **chaque nom dessiné dans sa propre police**. Un champ de
  recherche a le focus dès l'ouverture : on tape « geo », il reste Georgia,
  Entrée la choisit. Les polices **récentes** viennent en tête, la police du
  bloc est marquée, les flèches, Page préc./suiv., la molette et Échap font ce
  qu'on attend.
- La police choisie est **incorporée pour de bon** dans le document
  (sous-ensemble TrueType, `/Type0` Identity-H, `/ToUnicode`) : ce qu'on voit
  est ce que tout autre lecteur montrera. L'aperçu pendant la frappe emploie
  le même fichier, donc les mêmes largeurs. Les quatorze polices standard ne
  servent plus que de repli. Nouveau module `acrux_features::sysfonts`
  (catalogue des familles par la table `name`, quatre dessins par famille, lu
  une fois en tâche de fond à l'entrée dans le mode).

### Un vrai nuancier

- Les cinq pastilles de couleur laissent la place à **un bouton** qui montre
  l'encre en cours et déroule un nuancier : les **couleurs du thème** avec
  leurs teintes claires et foncées, les **couleurs vives**, les **récentes**,
  puis une couleur **personnalisée** — carré saturation-luminosité, réglette
  de teinte, code hexadécimal. Un clic sur une pastille choisit et referme ;
  glisser dans le carré ou la réglette recolore le bloc **en direct**.
- La couleur s'applique maintenant **au bloc ouvert**, même s'il existait déjà
  dans le document (avant, seule une zone neuve et vide la prenait) : le
  paragraphe est réécrit avec son encre, un mot gras reste gras, et l'encre
  d'avant est rétablie derrière lui pour la suite de la page
  (`ParagraphFrame::ink`).

## 0.18.0 — 20 septembre 2026

### L'interface, peaufinée

Un passage sur ce qui avait l'air d'une maquette plutôt que d'un logiciel
fini — vu de l'extérieur, sur des captures, et corrigé pièce par pièce.

- **Les champs de saisie** ont des coins arrondis et disent le focus par un
  anneau d'accent, plus par un bord dur : recherche, palette, invites et
  champ de page héritent tous du même rendu.
- **La recherche** est une carte flottante — ombre, coins arrondis — qui
  réunit le champ, le champ de remplacement, le compteur et les boutons ;
  « Remplacer » est le bouton principal, en couleur d'accent, et les boutons
  se grisent quand il n'y a rien à remplacer. Fini l'empilement de boîtes
  bordées de noir.
- **La palette de commandes** est une carte posée sur le voile, avec un trait
  entre le champ et les commandes.
- **Les onglets** n'ont plus de cloisons ; la croix de fermeture est un
  glyphe lissé, discret sur les onglets inactifs, sur une pastille au survol.
- **Les invites** (mot de passe, note, champ de formulaire) ont deux boutons,
  « Annuler » et « Valider », comme les autres fenêtres : on peut cliquer,
  pas seulement taper Entrée.
- **Un seul bouton partout** (`ui::paint::button`) : fenêtres, recherche et
  invites partagent le même dessin — principal, survolé, focus, grisé.
- Les cartes de l'accueil flottent sur une ombre large et légère au lieu
  d'être cernées ; l'info-bulle porte une ombre.

### La barre « Modifier le PDF », redessinée

- Elle alignait des rectangles plats, tous du même gris. Ses réglages sont
  maintenant **groupés dans des creux arrondis** : les deux outils en
  contrôle segmenté, le corps et l'interligne en pas-à-pas « − valeur + »,
  gras, italique et alignement en segments, la police en bouton-menu avec
  son chevron, les couleurs en **pastilles rondes** cerclées d'accent.
- Le survol se voit sur chaque commande, et « Terminer » est le bouton
  principal, le même que dans les fenêtres.
- Sa place est **réservée** : sur une fenêtre étroite, ce sont les derniers
  réglages qui s'effacent, jamais lui. Les libellés redondants (« Taille »,
  « Couleur ») ont laissé la place aux valeurs elles-mêmes, et l'interligne
  se dit par une icône ; le titre du mode s'efface quand un bloc est ouvert.

### La barre des annotations et « Remplir et signer », au même dessin

- **Des contrôles communs** (`ui::controls`) : creux arrondi, contrôle
  segmenté, pastille de couleur. La barre « Modifier le PDF », la barre des
  annotations, le panneau « Remplir et signer » et la fenêtre de capture d'une
  signature les emploient tous — ils se ressemblent enfin.
- **La barre d'un outil d'annotation** (surligner, note, biffure) a la même
  hauteur que la barre « Modifier le PDF », une pastille d'accent qui porte
  **l'icône** de l'outil et son nom, et « Terminer » en bouton principal, avec
  son survol.
- **Le panneau « Remplir et signer »** : les marques dans un creux, l'encre en
  pastilles rondes cerclées d'accent, l'épaisseur et la pointe en contrôles
  segmentés, les boutons « Créer une signature », « Créer un paraphe » et
  « Terminer » en boutons communs.
- **La fenêtre de capture d'une signature** : sa ligne d'encre suit les mêmes
  contrôles, et ses quatre boutons sont ceux des autres fenêtres.

### Le harnais de test invisible sait faire Ctrl+lettre

- Une fenêtre invisible ne voit pas le vrai clavier : `GetKeyState` y lit
  toujours « rien d'enfoncé ». Un `WM_KEYDOWN` de Ctrl ou de Maj posté par le
  harnais est désormais retenu, et `Chord "h"` dans `tools/headless.ps1`
  envoie un raccourci comme le ferait le clavier. Les tests d'interface se
  font donc tous sans rien afficher.

### La 3D

Un PDF peut porter un objet en trois dimensions (§13.6) : c'était le dernier
grand type de contenu du format qu'Acrux ne savait pas lire. Il le lit
maintenant, **de zéro**, et le fait tourner à la souris.

- **Le format U3D est décodé entièrement** (ECMA-363) : le codeur arithmétique
  adaptatif à contextes de la norme — sans lequel pas un entier n'est lisible
  dans un bloc —, les chaînes de modificateurs, les nœuds et leurs matrices,
  le maillage de base, les nuanceurs et les matériaux. Le décodeur est éprouvé
  sur un **vrai fichier de CAO** venu d'ailleurs : un dé de vingt-deux objets
  et 4 716 triangles s'en lit avec ses couleurs.
- **Un moteur de rendu 3D maison** : projection, découpe au plan proche,
  tampon de profondeur, ombrage interpolé et lampe frontale — comme le reste
  d'Acrux, sans carte graphique ni bibliothèque. Environ **4 ms par image**
  pour cinq mille triangles en 500 × 900.
- **Dans l'application** : un clic active le modèle, glisser le fait tourner,
  la molette s'en approche, Maj+glisser le déplace, un double-clic revient à
  la vue du document et Échap referme. La page n'est pas rendue à nouveau
  pendant qu'on tourne : seul le modèle l'est.
- **La vue du document est suivie** : `/3DV`, la matrice caméra→monde
  `/C2W`, la distance d'orbite `/CO`, l'ouverture `/FOV` et la couleur de fond
  `/BG` placent la caméra comme le document le demande ; à défaut, le modèle
  est cadré automatiquement.
- **En ligne de commande** : `acr 3d fichier.pdf` liste les modèles avec leur
  géométrie lue et `--extract` en sort les fichiers U3D ; `acr create --3d
  modele.u3d` fait l'inverse — un PDF portant le modèle, sa vue, et **l'affiche
  rendue par notre propre moteur**, si bien que la page montre l'objet même à
  l'impression ou dans un lecteur qui ignore la 3D.
- Le **PRC** (ISO 14739) est reconnu et annoncé comme non lu, plutôt que
  deviné. Le raffinement progressif d'un maillage U3D, les textures et les
  animations ne sont pas lus non plus.

## 0.17.0 — 20 septembre 2026

### Ouvrir une image, et les scans TIFF

- **Une image ouverte devient un PDF**, comme dans Acrobat : on dépose un
  fichier sur la fenêtre, ou on l'ouvre par Ctrl+O, et il s'affiche en
  document. Le premier `Ctrl+S` demande où le ranger — rien n'est écrit dans
  le dossier de l'image.
- Formats lus, tous décodés **de zéro** : **BMP** (1 à 32 bits, palette,
  champs de bits, RLE4 et RLE8, lignes de bas en haut ou de haut en bas),
  **GIF** (palette locale, entrelacement, couleur transparente, LZW de la
  norme GIF), **TIFF** (sans compression, CCITT Huffman modifié, Groupe 3,
  Groupe 4, LZW, PackBits, Deflate, JPEG ; 1 à 16 bits par composante ;
  palette, niveaux de gris, RVB, CMJN ; prédicteur horizontal), en plus du
  PNG et du JPEG qu'Acrux lisait déjà.
- **Un TIFF multipage donne autant de pages** : c'est ce que produit un
  scanner, et c'était le seul moyen d'obtenir d'un coup le PDF d'une pile de
  feuilles numérisées. Le Groupe 4, compression habituelle des scanners,
  passe par le décodeur CCITT qui servait déjà aux flux PDF.
- La ligne de commande suit : `acr create --images scan.tif -o scan.pdf`.
  Les signatures importées, les tampons, les filigranes et le remplacement
  d'une image dans un document acceptent eux aussi les nouveaux formats.
- Les décodeurs sont éprouvés **au pixel près** contre des fichiers écrits
  par GDI+, l'encodeur d'images de Windows : un décodeur mis à l'épreuve des
  octets de son propre encodeur ne prouverait rien.

### Rechercher et remplacer

- **Ctrl+H** ouvre, sous le champ de recherche, un champ **« Remplacer par… »**
  et deux boutons : **Remplacer** (l'occurrence surlignée, puis on passe à la
  suivante) et **Tout remplacer** (le document entier, d'un seul geste
  annulable). La tabulation passe d'un champ à l'autre, Entrée dans le second
  remplace. C'était l'une des fonctions d'Acrobat qui nous manquaient.
- Le remplacement n'est pas une refonte du document : c'est la même écriture
  chirurgicale que la modification de texte. Chaque occurrence garde la
  **police, le corps et la couleur** de ce qu'elle remplace, et le reste de la
  page ne bouge pas d'un glyphe (test `replace_corpus`).
- Le bandeau **reste ouvert** après un remplacement, et le document est
  reparcouru : on enchaîne les occurrences sans rouvrir la recherche.

### Ctrl+Z pendant la frappe

- Annuler une lettre effacée en pleine modification de texte se comportait
  étrangement : Ctrl+Z ne défaisait pas la frappe, il **refermait le bloc** et
  annulait toute la séance. Le mode de saisie a désormais sa propre pile :
  Ctrl+Z défait la dernière frappe, Ctrl+Y la refait, et le bloc reste ouvert.
- Le découpage suit celui d'un traitement de texte : les caractères qui se
  suivent forment **une seule étape**, et l'on coupe quand on change de geste
  (taper puis effacer), qu'on déplace le curseur, ou qu'on tape un blanc ou
  une ponctuation — Ctrl+Z défait donc un mot, jamais toute une phrase.
- Quand il n'y a plus rien à défaire dans le bloc, Ctrl+Z le referme et rend
  la main à l'annulation du document, comme avant.

### Mettre le texte en forme, comme dans Acrobat

- La barre du mode « Modifier le PDF » porte désormais, dès qu'un bloc est
  ouvert : **police** (quatre familles qui défilent), **gras**, **italique**,
  **alignement** (gauche, centré, droite, justifié, dessinés en barres comme
  dans Acrobat) et **interligne**. Tout s'applique au bloc entier et se voit
  immédiatement, sans rien écrire dans le fichier avant la sortie du bloc.
- La barre se met au diapason du bloc ouvert : on y lit son corps, son
  alignement et son interligne au lieu de réglages valables pour un autre.
- Le bouton de police garde une **largeur fixe** : changer de famille ne
  déplace plus les boutons suivants sous le pointeur.

### Une ligature ne perd plus de lettres

- Changer la police d'un texte lui faisait perdre ses ligatures : le « ﬁ » de
  « bénéficiez », absent de la nouvelle police, disparaissait purement et
  simplement. Une ligature qu'une police ne connaît pas s'écrit désormais
  **en lettres** (« ﬁ » → « f » puis « i »), à l'écriture comme à l'aperçu.
  Même chose pour « œ », « æ », les guillemets courbes et les tirets longs.

### La fenêtre se rouvre comme on l'a laissée

- Agrandie à la fermeture, elle se rouvre **agrandie**. Elle mémorisait
  jusqu'ici la taille du bureau entier mais se rouvrait en fenêtre ordinaire,
  donc débordant de l'écran.

### Tout le texte se modifie

- Un paragraphe dessiné par une opération qui porte **aussi d'autres textes**
  était refusé (« une opération dessine à la fois le paragraphe et d'autres
  textes ») : c'est le cas de beaucoup de formulaires. On ne retire désormais
  de l'opération que **nos** glyphes — en plusieurs morceaux s'il le faut — et
  ce qui appartient aux voisins reste exactement où il est.
- Un texte **en biais** — un filigrane — se modifie lui aussi : son
  inclinaison est relevée, ses lignes se mettent en page dans son repère et se
  posent tournées.
- Sur le corpus d'épreuve, la part du texte modifiable passe de 94 % à
  **100 %** (186 blocs sur 186), et le test interdit qu'elle baisse.

### Protéger un document par mot de passe

- Nouvelle entrée **« Protéger par mot de passe »** dans le panneau, sous
  « Protéger » : le mot de passe est demandé deux fois (masqué), le document
  est chiffré puis enregistré. Il sera redemandé à l'ouverture.
- Sur un document déjà protégé, la même entrée propose de **changer le mot de
  passe** ou de **retirer la protection**.
- Le chiffrement existait dans la bibliothèque (`acr protect`) mais
  l'application ne savait pas s'en servir : c'était l'une des fonctions
  d'Acrobat qui nous manquaient.

### Les styles d'un texte survivent à sa modification

- Une ligne qui mêle du **gras**, de l'*italique*, un lien coloré ou deux
  corps différents gardait jusqu'ici un seul style quand on la modifiait :
  tout était aplati d'une même police. Le style de **chaque caractère** est
  désormais relevé à l'ouverture, reporté à chaque frappe — ce qui n'a pas
  bougé garde le sien, ce qu'on tape prend celui de son voisin de gauche — et
  rendu à l'écriture, police par police et couleur par couleur.
- L'aperçu les montre aussi : ce qu'on voit en tapant reste ce qu'on obtient.

### Écrire, puis déplacer, comme une signature

- **Une fois le texte écrit, le bloc reste posé** : son cadre et ses huit
  poignées demeurent, on le glisse, on le redimensionne, exactement comme une
  signature qu'on vient de poser. Un clic dedans rend la main au texte,
  curseur au point cliqué.
- On pose un bloc en cliquant à côté, ou avec **Échap** ; une seconde fois,
  il se referme.
- Sur un bloc posé : les **flèches** le déplacent au point près (dix avec
  Maj), **Entrée** rouvre le texte, **Suppr** l'efface.

### Corrections d'usage

- **Glisser dans un texte le sélectionne** au lieu de déplacer le bloc : le
  corps du cadre appartenait à tort aux poignées, et l'on ne pouvait plus
  sélectionner un mot à la souris.
- Le contour d'un glyphe est cherché dans **sa** police : une lettre d'un mot
  en gras pouvait disparaître de l'aperçu.
- Une zone de texte encore vide ne montre plus de cadre ni de poignées : un
  clic dans le blanc ne couvre plus la page de repères.

### La frappe est instantanée

- **Taper ne réécrit plus le document.** La police du bloc est chargée une
  fois à l'ouverture ; chaque lettre n'est plus qu'une mise en page en
  mémoire, et c'est l'application qui dessine les glyphes, par-dessus le
  texte d'origine masqué. Le fichier n'est écrit **qu'en sortant du bloc**.
  Une frappe coûtait une trentaine de millisecondes sur une page ordinaire,
  bien davantage sur une page chargée ; elle est désormais immédiate, quelle
  que soit la page.
- **Ce qu'on voit en tapant est ce qu'on obtient** : l'aperçu emploie la
  mise en page qui écrira, et tombe à moins d'un centième de point de
  l'écriture réelle (test `live_text_corpus`).
- **Les lettres qui manquent à la police du PDF s'affichent quand même.** Une
  police incorporée ne porte que les lettres déjà sur la page ; les autres
  sont empruntées à la police système de la même famille le temps de la
  saisie — celle-là même qui complétera le fichier à l'écriture.
- **Toute une séance ne fait qu'une opération d'annulation** : frappe,
  déplacement et redimensionnement compris.

### Le texte se modifie comme dans Acrobat

- **Un seul clic** ouvre un bloc et y pose le curseur. Il n'y a plus de
  double-clic, ni d'état « sélectionné » séparé de l'écriture.
- Le cadre porte **huit poignées rondes** ; sa bordure déplace le bloc, une
  poignée le redimensionne, et **le texte reflue sous les yeux** — sans que
  rien ne soit écrit avant la fin.
- Le pointeur dit ce que fera le clic : flèches de redimensionnement sur les
  poignées, croix de déplacement sur le bord, barre de texte dedans.
- La touche majuscule retient un déplacement sur un axe ; les repères
  d'alignement aimantent le bloc à ses voisins.
- « Cliquer à côté » valide, comme dans Acrobat.

### Modifier le texte d'une image

- Un document **scanné** ne porte pas de texte : Acrux le relit. Encre et
  papier séparés par le seuil d'Otsu, découpe en croix (colonnes, puis
  lignes), puis chaque tache comparée aux formes des polices installées.
- Dans « Modifier le PDF », **cliquer sur du texte d'image l'ouvre comme du
  vrai texte**. À l'écriture, les pixels d'origine sont couverts de la
  couleur du papier et le texte est posé par-dessus, dans une police, avec sa
  graisse et son corps : il devient sélectionnable et cherchable.
- Mesure sur une page réelle rendue en 300 ppp : **0,09 d'écart moyen**, la
  moitié des lignes lues exactement. En deçà de 250 ppp la lecture se dégrade
  — c'est la limite de toute reconnaissance, et la confiance rendue avec
  chaque ligne le dit : une ligne mal lue n'est pas proposée à la
  modification.
- Rien n'est appris ni deviné : les modèles sont les **vraies polices du
  système**, dessinées puis relues par le même chemin que l'image.

## 0.16.0 — 20 septembre 2026

### La page ne clignote plus quand on déplace

- Un bloc déplacé était **réécrit puis la page rendue à chaque pas** : c'est
  cette suite de rendus qui faisait clignoter. Désormais la page n'est rendue
  que **deux fois par geste** — une au début, une à la fin.
- Entre les deux, c'est une **photo du bloc**, prise sur la page rendue, qui
  suit le pointeur. Le bloc est effacé de la page le temps du geste, donc on
  ne le voit pas en double, et l'aperçu reste exact au pixel près.
- Les images que le fil de rendu renvoie pour la page en cours de
  déplacement sont **écartées** : venant d'un état dépassé, elles ramenaient
  le bloc en arrière l'espace d'une image.

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
