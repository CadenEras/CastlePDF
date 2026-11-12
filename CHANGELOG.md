# Journal des versions

Format inspiré de [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/).
Versions selon [SemVer](https://semver.org/) ; règles propres à `0.x` dans
[docs/GIT.md](docs/GIT.md). Une préversion `alpha` marque la fin de chaque phase de la
[feuille de route](docs/ROADMAP.md).

## [Non publié]

### Ajouté

- Worker et shell : **couche de texte d'une page** (ADR-0006, tranche 3.6a). Requête
  `PageText` (protocole **v5**), réponse en lignes dans l'ordre de lecture, chaque mot avec sa
  ligne de base et sa taille sur la page rendue à l'échelle 1 (rotation comprise), en 1/64 de
  pixel. Réponse bornée à 960 Kio (`truncated` au-delà) ; décodage qui vérifie les nombres de
  lignes et de mots avant d'allouer. Commande Tauri `page_text` (permission
  `allow-page-text`), document courant, page existante et génération de vue seulement : une
  couche d'une vue périmée est écartée ou annulée comme une tuile. Le drapeau `incomplete`
  signale du texte manquant (police ou glyphe non lus) ; un mot de plus de 65 535 octets est
  coupé et marque `truncated`. Une réponse trop grande pour une trame devient un échec
  `Internal` au lieu d'être perdue. Polices et CMap viennent du cache de rendu du document.
- `pdf-render` : `page_transform` (de l'espace utilisateur aux pixels du rendu, sans la limite
  de taille d'un rendu) ; chaque
  `TextWord` donne sa ligne de base et sa taille.
- Fuzzing : cible `protocol` (requêtes et réponses du worker).

- `pdf-render` : **ordre de lecture** (`reading_lines`, ADR-0006, tranche 3.5b). Les morceaux
  d'une ligne horizontale montrés à part sont recollés (même ligne de base, moins d'une taille
  d'écart), puis les lignes sont ordonnées par découpes récursives le long des plus larges
  bandes vides (XY-cut) : rangées de haut en bas, colonnes de gauche à droite, la bande la plus
  large d'abord. Un titre sur deux colonnes passe avant elles, chaque colonne avant la suivante.
  Au-delà de 10 000 lignes, simple tri de haut en bas. `text_lines` commence désormais une
  ligne à tout saut de plus d'une taille (gouttière, cellule). `castle-pdf text` suit l'ordre
  de lecture. Fixture `columns.pdf`.

- `pdf-render` : **mots et lignes** (`text_lines`, ADR-0006, tranche 3.5a), dans l'ordre du
  contenu, en toute direction (horizontale, verticale, tournée). Un glyphe hors de la ligne de
  base, à contre-sens ou revenant en arrière de plus que sa taille commence une ligne ; une
  espace ou un écart de plus de 0,15 fois la taille sépare deux mots. Chaque mot donne ses
  glyphes et ses bornes. Ordre de lecture entre colonnes : tranche 3.5b.
- `pdf-render` : **`/ActualText`** (ISO 32000-2, 14.9.4) des séquences marquées, propriétés en
  ligne ou ressource `/Properties`, à travers les formulaires ; une plage imbriquée cède à la
  plage extérieure, une plage sans glyphe (une image de texte) est un mot à elle. Chaînes de
  texte décodées (UTF-16BE, UTF-8, PDFDocEncoding). Les plages comptent dans
  `max_text_glyphs`.
- CLI : `castle-pdf text` affiche ces mots et ces lignes. Fixture `layout.pdf` ; l'oracle
  `pdftotext` compare aussi les mots ; test ignoré `layout_corpus` pour un corpus local.

- `pdf-render` : **police « texte seulement »** (ADR-0006, tranche 3.4b). En extraction de texte,
  une police que le rendu ne dessine pas (non incluse et non remplacée, programme Type 1
  symbolique, programme endommagé) est lue sans programme : codes, largeurs (`/Widths`, `/W`,
  `/W2`) et texte (`/ToUnicode`, noms de glyphes) depuis son dictionnaire. Son texte et ses
  positions ne sont plus perdus. Le rendu la signale toujours (`Text`), même quand elle vient
  d'un cache partagé avec l'extraction. Seules les polices à CMap prédéfinie autre
  qu'`Identity` restent non lues (tranche 3.7b).

- `pdf-render` : **encodages des polices Type 0 au-delà d'`Identity-H`** (ADR-0006, tranche
  3.4a), pour le rendu comme pour le texte.
  - `Identity-V` et toute CMap `/WMode 1` : écriture verticale avec `/W2` et `/DW2` (ISO 32000-2,
    9.7.4.3), glyphes placés par leur vecteur de position, déplacement vers le bas, ajustement
    `TJ` vertical.
  - CMap incluses : codes découpés par leurs plages (un à quatre octets), CID par `cidchar` et
    `cidrange`, sinon par les plages notdef, sinon CID 0. Le parent d'un `usecmap` ou
    `/UseCMap` est fusionné : `Identity-H`/`-V` par nom, ou une autre CMap incluse, sur quatre
    niveaux au plus (une boucle s'arrête là). Une autre CMap prédéfinie attend la tranche 3.7b.
  - L'espacement des mots s'applique au code 32 d'un octet d'une police composite.
  - Fixture `cmaps.pdf`, identique à `pdftoppm` (page 1) et à `pdftotext` ; la cible de
    fuzzing `font` choisit aussi ces encodages.

- `pdf-render` : **extraction du texte** (`page_text`, ADR-0006, tranche 3.3). Chaque glyphe
  montré par la page, formulaires compris, avec son texte Unicode et sa position en espace
  utilisateur. Le texte vient de la CMap `/ToUnicode` de la police (lue au premier besoin, jamais
  par un rendu), puis du nom de glyphe de l'encodage (liste de glyphes d'Adobe, `uniXXXX`,
  `uXXXX`, ligatures `_`), y compris les noms du charset d'une police CFF à encodage intégré.
  Un glyphe sans source est **inconnu, signalé, jamais deviné**. Un code `/ToUnicode` d'une autre
  longueur que celui de la police est accepté et signalé ; une `/ToUnicode` illisible est
  comptée. Le contenu est interprété sans rien peindre ni décoder d'image. Nouvelle limite
  `RenderLimits::max_text_glyphs` (un million) ; chaque glyphe est imputé au budget.
  `page_text_cached` partage polices et CMap avec les autres pages et rendus du document. Les
  octets montrés dans une police que le moteur ne lit pas sont comptés (`unread_bytes`).
- CLI : **`castle-pdf text <fichier> [--page <n>]`** affiche le texte ; caractères de contrôle,
  contrôles bidirectionnels et séparateurs de ligne venus du document remplacés par U+FFFD.
  Une page en échec est signalée et les suivantes sont affichées (code 1). `probe` et `survey` mesurent le texte des premières pages.
- Fixture `text.pdf` : `/ToUnicode` (ligature, paire de substitution, codes d'une autre
  longueur), noms de glyphes, texte inconnu, `/ToUnicode` endommagée, texte invisible. Oracle
  `pdftotext` (workflow `oracle`) sur les pages dont tout le texte est connu ; les écarts voulus
  sont documentés dans `pdftotext_oracle.rs`. Les cibles de fuzzing `render` et `font` extraient
  aussi le texte.

- `pdf-render` : **lecteur de CMap** (`CMap`, ISO 32000-2, 9.7.5 et 9.10.3 ; ADR-0006,
  tranche 3.2), commun à `/ToUnicode` et aux encodages des polices Type 0. Il lit les plages de
  codes (comparées octet par octet), `bfchar`, `bfrange` (forme de base et tableau), `cidchar`,
  `cidrange`, les plages notdef, `usecmap` (nommé ; fusion par `CMap::inherit`, profondeur à la
  charge de l'appelant) et `/WMode`. Les codes sont cherchés avec leur longueur ; la dernière
  définition l'emporte, en temps logarithmique (plages aplaties en segments disjoints). Une
  entrée invalide est ignorée et comptée, jamais devinée : CID au-delà de 65 535, destination
  de plus de 512 octets, plus de 256 plages de codes ; un bloc dont les opérandes ne forment
  pas des entrées entières est ignoré en entier. Un bloc non fermé ou un jeton refusé est une
  erreur. Les plages ne sont jamais développées ; jetons des blocs, entrées et structures de
  recherche sont imputés au budget. Nouvelle cible de fuzzing `cmap`. Pas encore branché sur le
  texte ni le rendu (tranches 3.3 et 3.4).
- `pdf-render` : `page_text_profile` dit d'où viendrait l'Unicode du texte de chaque police
  d'une page (`/ToUnicode`, noms de glyphes, encodage intégré, collection CID...) et quels
  encodages ont ses polices Type 0, sans lire de CMap ni de programme de police (ADR-0006,
  tranche 3.1). `castle-pdf probe` l'affiche (`text=`, `cmaps=`) et `survey` en fait le
  décompte ; résultats sur le corpus dans `docs/QUALITY.md`.

### Corrigé

- `docs/QUALITY.md` : une partie du fichier avait été recopiée au milieu du tableau des
  contrôles manuels (motif `` $` `` d'un remplacement de texte), depuis `v0.1.0-alpha.3`.

## [0.1.0-alpha.3] — 2026-11-11

Fin de la phase P2 (lecteur utile). L'application desktop ouvre un PDF (glisser-déposer,
ligne de commande, Finder sous macOS) et l'affiche en défilement continu, rendu en tuiles
par le moteur du projet dans un worker séparé. Le moteur lit les fichiers PDF 1.5+
(flux xref et d'objets, Flate et filtres simples), reconstruit l'index d'un fichier abîmé,
et dessine chemins, texte des polices incluses ou remplacées, images, formulaires et
polices Type 3. Sur le corpus de mesure, la première page est rendue pour 1 920 fichiers
sur 2 000 (SafeDocs) et 2 229 sur 2 636 (UNSAFE), sans crash ni timeout. Toujours **aucun
binaire distribué** : version de développement, à construire depuis les sources.

### Limites connues

- Rendu partiel, signalé page par page dans l'application : dégradés, motifs, masques
  adoucis, transparence de forme, modes de fusion, découpe par le texte, fonctions de
  transfert, contenu optionnel et certains espaces de couleur ne sont pas dessinés. Les polices non incluses sont remplacées
  par des polices libres aux mêmes largeurs, pas par la police d'origine.
- Images en CCITTFax, JBIG2 ou JPX signalées, pas dessinées ; une page dont les images
  décodées dépassent 256 Mio est refusée (`limit/DecodedBytes`). Reportés en P3.
- Pas de texte sélectionnable, de recherche ni de copie (P3) ; pas d'écriture (P4).
- Fichiers chiffrés refusés avec une erreur de capacité explicite.
- Chaque tuile réinterprète le contenu de sa page ; une page très lourde peut prendre
  plusieurs secondes (maximum observé : 8 s). Reporté en P3, à mesurer.
- Vérifié à la main sous Windows seulement : temps par tuile côté WebView, mémoire en
  défilement, glisser-déposer. Sous macOS et Linux, l'affichage et l'ouverture depuis le
  Finder sont vus sur des captures CI, la mémoire est mesurée par le CI, mais aucun geste
  n'a été fait à la main.
- L'ouverture depuis le Finder n'existe que dans un `.app` empaqueté ; le packaging (worker
  inclus, signature) arrive avec la phase de diffusion.
- Mémoire mesurée sur runners CI partagés : ordres de grandeur, pas des seuils.

### Ajouté

- Shell : **ouverture depuis le Finder** (macOS) : « Ouvrir avec », double-clic ou dépôt
  sur l'icône du Dock (`RunEvent::Opened`). L'application empaquetée déclare le type PDF en
  lecteur de rang `Alternate`, sans devenir le lecteur par défaut. Seule une URL `file:`
  locale est ouverte, un fichier à la fois, comme pour le glisser-déposer. Une demande reçue
  au lancement, avant que le shell soit prêt, est gardée puis ouverte (le premier essai en CI
  paniquait) ; choix et ordre sont testés (`opening.rs`). Le workflow `screenshot` ouvre `forms.pdf` par `open -a` sous macOS
  et joint la capture.
- `scripts/measure-memory.mjs` mesure aussi l'application avec un document ouvert (argument
  facultatif). Le workflow `memory` mesure au repos puis avec `forms.pdf`.
- CI : **ARM64 et macOS Intel**. Les tests Rust et le build natif tournent aussi sur
  `ubuntu-24.04-arm`, `windows-11-arm` et `macos-15-intel` (`macos-latest` étant ARM64). Ce sont
  des runners standard, gratuits pour un dépôt public. rustup est installé s'il manque.
- CI : workflow **`screenshot`**, lancé à la main. Il fait afficher `truetype.pdf` et `forms.pdf` à
  l'application release sous Linux (affichage virtuel), Windows et macOS, et joint les captures
  au run pour relecture. Il sert à vérifier l'affichage sur les OS que personne ne regarde à la
  main (`scripts/screenshot-app.mjs`).
- UI : panneau **« Mesures »**, replié sous la barre d'outils. Il donne, pour les 2 000
  dernières tuiles, la médiane et le p95 du temps entre la requête et les données reçues, et
  du dessin. Il sert à mesurer le transport d'ADR-0003 dans l'application réelle.
- `pdf-syntax` : **filtres ASCIIHexDecode, ASCII85Decode, LZWDecode et RunLengthDecode**
  (ISO 32000-2, 7.4.2 à 7.4.5), avancés de P3 (amendement d'ADR-0003).
  - LZW lit les codes de 9 à 12 bits, avec `/EarlyChange` et les prédicteurs, et garde une
    table pleine jusqu'au code de remise à zéro.
  - Chaque octet décodé est imputé au budget avant d'être gardé : une expansion RunLength ou
    LZW s'arrête à `max_decoded_bytes`.
  - Une donnée invalide donne `MalformedKind::InvalidFilterData`.
  - Les images XObject et en ligne qui les utilisent sont dessinées.
  - Fixture `filters.pdf`, identique à `pdftoppm`, flux décodés identiques à qpdf.
  - La cible de fuzzing `flate` couvre les nouveaux filtres.
- UI : **défilement continu**. Les pages se suivent dans une zone qui défile, chacune ajustée
  à la largeur.
  - Seules les pages proches de la vue sont dans le document, avec un canevas. Les autres
    gardent leur place.
  - Les tailles sont mesurées à l'approche de la vue ; les pages pas encore mesurées sont
    estimées d'après la première.
  - Le défilement reste ancré sur la page du haut quand une page au-dessus est mesurée.
  - Chaque changement de pages visibles est une nouvelle vue : le shell abandonne les tuiles
    des pages quittées, et les tuiles déjà dessinées ne sont pas redemandées.
  - Les boutons Page précédente et Page suivante font défiler.
  - Une page est annoncée comme image dès qu'elle est proche de la vue.
  - La zone des pages se défile au clavier et occupe la hauteur de la fenêtre.
- Shell : **relance du worker**. Un worker qui meurt ou viole le protocole est relancé, au plus
  3 fois par minute.
  - La tâche qu'il exécutait échoue sans être renvoyée.
  - Le document courant est rouvert sous le même identifiant pour l'UI, si son fichier n'a
    pas changé.
  - Les tâches en attente continuent avec le nouveau worker.
  - Un worker mort au repos est remarqué avant qu'une tâche ne lui soit envoyée.
  - Les identifiants de document donnés à l'UI sont ceux du shell, et non plus ceux du worker.
- `pdf-render` : **cache de rendu par document** (`RenderCache`, `render_page_cached`,
  `render_area_cached`). Les rendus d'un document réutilisent ce qu'ils ont lu : polices et
  contours de glyphes, images décodées, formulaires et procédures Type 3, contenu décodé des
  pages.
  - Le cache est borné en octets (64 Mio par défaut).
  - Il est vidé quand un rendu commence alors qu'il est plein aux trois quarts, ou quand
    les limites de rendu changent.
  - Il reste utilisable après une erreur ou une annulation.
  - Le worker en garde un par document ouvert. Une première page en tuiles coûte 89 ms en
    médiane au lieu de 291 ms, et 0,34 s au p95 au lieu de 7,9 s.
  - Les résultats sont identiques au pixel près.
- `pdf-render` : **polices Type 3** (ADR-0005, étape 5.5). Chaque glyphe est une procédure de
  contenu, exécutée comme un formulaire. Elle prend la matrice de la police et la matrice de rendu
  du texte, et les ressources de la police ou, à défaut, celles du contenu appelant.
  - Le texte avance de la largeur du code multipliée par `/FontMatrix`, avec l'espacement, la
    mise à l'échelle, l'élévation et les ajustements `TJ`.
  - Un glyphe `d1` (non coloré) est peint dans la couleur du texte. Ses opérateurs de couleur
    sont ignorés, et ses images qui ne sont pas des masques sont signalées.
  - Un glyphe endommagé ou appelé par lui-même est sauté ou dessiné jusqu'à son erreur, et signalé
    (`Text`). Chaque glyphe compte comme une opération.
  - Formulaires et glyphes partagent l'entrée et la sortie du contenu imbriqué (`run_nested`).
  - Fixture `type3.pdf`, à 0,001 % de `pdftoppm`.
- `pdf-render` : **Symbol et ZapfDingbats** non inclus sont dessinés (ADR-0005, étape 5.3b)
  avec « CastlePDF Symbols ». Cette police de 55 Kio réunit des glyphes de Noto Sans Math,
  Noto Sans, Noto Sans Symbols et Symbols 2 (SIL OFL 1.1), choisis glyphe par glyphe et
  fusionnés hors ligne par `scripts/build-symbol-font.sh` (construction reproductible).
  - Les codes passent par l'encodage intégré, la liste de glyphes d'Adobe ou celle de
    ZapfDingbats, puis Unicode. Les points de code de la zone privée d'Adobe (pièces de
    crochets, ©, ®, ™) pointent vers leur équivalent.
  - Sans `/Widths`, les largeurs sont celles des métriques d'Adobe.
  - Le texte est signalé `FontSubstituted`. Les autres polices symboliques restent `Text`.
- `pdf-render` : **XObjects de formulaire** (ISO 32000-2, 8.10). `Do` dessine un
  formulaire :
  - avec sa `/Matrix`, découpé à sa `/BBox` ;
  - avec ses propres ressources s'il en a, sinon celles du contenu qui l'appelle ;
  - dans un état graphique sauvegardé que son contenu ne peut pas restaurer.

  Un **groupe de transparence** commence avec des alphas à 1 et se compose en entier avec
  l'alpha de remplissage, par une couche d'opacité de `vello_cpu`. Un groupe à
  élimination (`/K true`) est dessiné comme un groupe ordinaire et signalé (`BlendMode`).

  Un formulaire est lu une fois par rendu, et ses ressources sont relues une seule fois
  d'un appel à l'autre. La découpe et la couche comptent dans `max_clips` et
  `max_path_work`, et les opérations dans `max_operations`.

  Un formulaire endommagé, invalide, imbriqué à plus de 16 niveaux ou appelé par
  lui-même est sauté, ou dessiné jusqu'à son erreur. Il est signalé (`XObject`) et
  n'affecte que lui.

  Une image dont l'espace de couleur nomme une ressource n'est plus gardée en cache entre
  deux formulaires. Fixture `forms.pdf`, identique à `pdftoppm`.
- `pdf-render` : **tuiles identiques à la page**. Le canevas est découpé en cellules
  de 256 px (`CELL_SIDE`), sur une grille fixée au coin de la page, chacune rasterisée
  avec sa propre origine à partir des coordonnées de la page.
  - `render_area` donne maintenant exactement les pixels de `render_page`. Avant, une
    tuile pouvait s'écarter de 1 niveau par dessin, cumulé là où des bords se
    superposent (amendement d'ADR-0003).
  - `max_pixels` porte sur les cellules touchées par une zone.
  - `max_path_work` compte l'aplatissement une fois par cellule touchée.
- `pdf-render` : **images en ligne** (ADR-0005, étape 5.4b). `BI … ID … EI` est dessiné
  comme une image XObject, avec les mêmes espaces de couleur, profondeurs, masques de
  pochoir, filtres et limites. Les abréviations d'ISO 32000-2 (8.9.7) sont développées :
  clés (`/W`, `/CS`, `/F`…), filtres (`/Fl`, `/DCT`…) et espaces (`/G`, `/RGB`, `/CMYK`,
  `/I`, y compris la base d'un Indexed). Un espace de couleur peut être nommé dans les
  ressources. La fin des données vient de `/L` (PDF 2.0) quand il est présent, sinon de
  l'heuristique déjà utilisée pour sauter l'image. Une image en ligne est décodée à chaque
  dessin (pas de cache : elle n'a pas d'identifiant d'objet). Fixture `inline-images.pdf`,
  identique à `pdftoppm`.
- `pdf-render` : **images** (ADR-0005, étape 5.4a). Les XObjects `/Image` dessinés par `Do`
  sont rendus :
  - **données** passées par les filtres existants, ou décodées par `zune-jpeg` 0.5.15
    (`DCTDecode`, sans ses chemins SIMD `unsafe`) ;
  - **espaces de couleur** DeviceGray, RGB et CMYK (conversion ISO, comme pour les tracés),
    ICCBased réduit à `/N`, CalGray, CalRGB et Indexed ;
  - **échantillons** de 1, 2, 4, 8 et 16 bits, avec `/Decode` ;
  - **masques** : masques de pochoir (`/ImageMask`, couleur de remplissage), `/SMask`,
    `/Mask` en pochoir (rééchantillonnés si leur taille diffère) ou par clé de couleur ;
  - **JPEG** CMYK et YCCK, y compris l'inversion Adobe compensée par `/Decode`.

  Le dessin passe par la peinture image de `vello_cpu` et le même découpage en double
  précision que les tracés. Une image agrandie est échantillonnée au plus proche, sauf
  `/Interpolate` ; une image réduite l'est en bilinéaire. La taille (largeur × hauteur) est
  vérifiée avant toute allocation, contre la nouvelle limite `max_image_pixels`. Chaque
  rendu garde un cache d'images borné. Restent signalés : autres filtres (JPX, JBIG2,
  CCITT…) en `Image`, Lab, Separation et DeviceN en `ColorSpace`, un masque inapplicable
  en `SoftMask`, XObjects de formulaire en `XObject`. `/Matte` (couleurs prémultipliées)
  est restitué.

  Fixtures `images.pdf` et `jpeg.pdf`, identiques à `pdftoppm` hors CMYK. Nouvelle cible de
  fuzzing `image`.
- `pdf-render` : **polices de remplacement** (ADR-0005, étape 5.3a). Une police simple non
  symbolique que le PDF nomme sans l'inclure (les 14 polices standard, Arial, Times New
  Roman…), ou dont le programme est Type 1 ou endommagé, est dessinée avec Arimo, Tinos ou
  Cousine. Ces polices (SIL OFL 1.1), en quatre styles chacune et réduites aux caractères
  des encodages Standard, WinAnsi et MacRoman, pèsent environ 280 Kio et sont incluses
  derrière la fonction `standard-fonts`, active par défaut. Famille et style viennent du
  nom de la police et de son descripteur. Les largeurs sont celles du PDF, ou celles des
  remplaçantes, identiques aux métriques AFM d'Adobe (test). Le texte remplacé est signalé
  `FontSubstituted`. Le protocole du worker passe en v4 (bit `FONT_SUBSTITUTED`), et l'UI
  affiche « Polices remplacées » à part du rendu partiel. Symbol et ZapfDingbats restent
  signalés `Text` (étape 5.3b). Inventaire : `crates/pdf-render/fonts/README.md`.
- `pdf-render` : **texte des polices CFF et OpenType incluses** (ADR-0005, étape 5.2b).
  Sont pris en charge `/FontFile3` `/Type1C` (polices simples `/Type1`), `/CIDFontType0C`
  (polices Type 0 à descendante `CIDFontType0`) et `/OpenType`. Le CFF nu est lu par
  `read-fonts` (`ps::cff`), sans conteneur OpenType reconstruit. Les codes passent par les
  noms de glyphe du jeu de caractères CFF, ou par l'encodage intégré du CFF quand le PDF ne
  donne pas de nom (9.6.5.2). Les CID passent par le jeu de caractères d'un CFF à CID. Un
  nom absent est signalé. Le programme Type 1 (`/FontFile`) reste signalé `Text`.
  Fixture `cff.pdf` (sous-ensembles OFL de Source Sans 3 et Noto Sans CJK), identique à
  `pdftoppm` à 216 dpi ; la cible de fuzzing `font` couvre aussi le CFF.
- `pdf-render` : **texte des polices TrueType incluses** (ADR-0005, étape 5.2a). Sont pris
  en charge :
  - les polices simples `/TrueType` et les polices Type 0 `Identity-H` à descendante
    `CIDFontType2`, dont le programme `/FontFile2` est lu par `skrifa` 0.49.0 ;
  - encodages `/Encoding` (Standard, WinAnsi, MacRoman, MacExpert, `/Differences`), Adobe
    Glyph List, tables `cmap` (3,1), (3,0) et (1,0), `/CIDToGIDMap` ;
  - largeurs `/Widths`, `/W` et `/DW` ;
  - état du texte `Tc Tw Tz TL Ts Tf Tr`, positionnement `Td TD Tm T*` et `Tj TJ ' "` ;
  - modes de rendu remplissage, contour, les deux, invisible.

  Les glyphes sont dessinés sans hinting par le pipeline vectoriel. Toute autre police
  reste signalée `Text` (polices standard, non incluses, CFF, Type 1, Type 3). Une police
  absente des ressources ou illisible saute son texte sans faire échouer la page. Nouvelle
  limite `max_glyph_segments`. Chaque segment de glyphe compte comme une opération et est
  borné avant allocation. Cache des contours borné par rendu. Les noms de glyphe hors de
  l'Adobe Glyph List sont cherchés dans la table `post` ; un nom absent est signalé. Fixture
  `truetype.pdf` (sous-ensemble OFL d'Arimo, `tests/fonts`), comparée à `pdftoppm` avec
  une tolérance propre au texte. Nouvelle cible de fuzzing `font`.
- `pdf-render` : `page_font_kinds` classe les polices déclarées par une page (programme
  inclus Type 1, CFF, TrueType, OpenType, CID ; police standard ; non incluse ; Type 3)
  sans lire aucun programme de police. `probe` l'écrit (`fonts=`) et `survey` compte les
  premières pages par type de police (ADR-0005, étape 5.1).
- Application desktop : **« Document réparé »**. Le protocole du worker passe en v3
  (`PROTOCOL_VERSION` 3) : `DocumentOpened` et chaque `Tile` disent si la structure du
  document a été reconstruite (aucune, réparée, réparée avec des éléments peut-être
  manquants). Le shell le transmet (`repair` de l'événement `document`, octet 6 de
  l'en-tête des tuiles) et l'UI affiche une note dès l'ouverture, ou dès qu'une tuile
  révèle un objet retrouvé par balayage pendant le rendu.
- `pdf-syntax` : **reconstruction de l'index des objets** (ADR-0004). Quand la lecture
  stricte échoue sur la structure (`startxref` ou `%%EOF` absent, section xref ou trailer
  invalide, cycle `/Prev`, fichier tronqué), un balayage linéaire retrouve les en-têtes
  `n g obj`. Il saute les données de flux par `/Length`, ou jusqu'à `endstream`, et relit
  les flux d'objets trouvés. La dernière occurrence d'un objet l'emporte. Le trailer est le
  plus récent dont la `/Root` existe, sinon le dernier catalogue. Un objet absent de son
  offset dans un fichier ouvert strictement est cherché de la même façon, à la demande.
  Chaque en-tête est facturé au budget d'objets. Le chiffrement n'est jamais réparé :
  un `/Encrypt` dans un trailer, ou un dictionnaire de chiffrement parmi les objets
  (fichier coupé avant son trailer), refuse le fichier.
  La réparation est visible : `PdfFile::repair` (cause, objets, conflits, origine du
  trailer, flux d'objets illisibles et flux sans `endstream` qui peuvent cacher des
  objets, `is_incomplete`), ligne `repair:` de `inspect`, jeton `repaired=` de `probe`
  (`on-lookup/` pour un objet retrouvé à la demande) et compte à part dans `survey`. `OpenOptions::strict` / `PdfFile::open_strict` gardent la lecture
  stricte d'ADR-0002 (oracles, tests de structure). Fixtures abîmées dans
  `tests/corpus/damaged`, cible de fuzzing `repair`.
- `castle-pdf survey <répertoire>` : ouvre chaque fichier et rend sa première page, chacun
  dans un processus enfant (`castle-pdf probe`) tué après un délai ; rapport agrégé
  (taux d'ouverture et de rendu, catégories d'erreur, capacités non rendues, crashs,
  timeouts, temps), sans contenu ni nom de fichier sauf `--paths`. Code 1 si un fichier
  fait planter ou bloquer le moteur. Première mesure sur SafeDocs dans `docs/QUALITY.md`.
- `pdf-render` : `render_area` rend un rectangle de page (tuile) et `page_size` donne la
  taille en pixels d'une page à une échelle ; `max_pixels` s'applique à la zone, pas à la
  page. Les tuiles assemblent le rendu de la page à 1 près par canal (arrondi en simple
  précision du rasteriseur) ; la cible de fuzzing `render` le vérifie.
- `pdf-worker` : protocole v2 (`PROTOCOL_VERSION` 2) — `OpenDocument` (chemin donné par
  le shell), `PageSize`, `RenderTile` (au plus 256 × 256, pixels RGBA bruts dans la trame)
  et `CloseDocument` ; au plus 4 documents ouverts ; codes d'erreur distincts (chiffré,
  non pris en charge, malformé, illisible, document inconnu…). Une tuile pleine traverse
  le tube du worker en 0,5 ms environ (médiane, Windows, release, page simple).
- Application desktop : **première page affichée**. Le shell ouvre un PDF déposé sur la
  fenêtre ou passé en argument, sans que l'UI nomme de chemin ; il le transmet au worker, met
  les tâches en file (une à la fois, file bornée) et garde un cache LRU de tuiles de
  128 Mio comptés en octets. Les tuiles d'une génération périmée sont abandonnées ou
  annulées. Commandes `page_size`, `render_tile` (octets bruts reçus en `ArrayBuffer`, sans
  JSON ni base64), `current_document` et `begin_view` (générations numérotées par le
  shell, donc robustes à un rechargement de la page). Canvas limité à 16 Mpx ; une tâche de
  diagnostic est limitée à 60 s. Un worker perdu n'est pas encore relancé. L'UI affiche la page ajustée à la largeur, tuile
  par tuile dans un canvas, avec navigation page par page, état explicite (préparation,
  rendu en cours, prêt, échec) et mention du rendu partiel. Mesures dans
  `docs/perf/2026-11-08-p2-first-page.md`.
- ADR-0003 accepté : ordre de P2 (Flate et flux xref avant l'interpréteur),
  `vello_cpu`, `miniz_oxide`, tuiles 256 px en octets bruts, oracles de rendu, ARM64.
- `pdf-syntax` : filtre `FlateDecode` (ISO 32000-2, 7.4.4) via `miniz_oxide` 0.9.1
  (sans `unsafe`) — décodage incrémental, chaque fenêtre de 64 Kio facturée à
  `max_decoded_bytes` avant d'être conservée (bombes de décompression arrêtées à la
  limite), checksum vérifié, chaînes de filtres, prédicteurs PNG (10–15) et TIFF (2,
  composantes 8 bits) ; `PdfFile::decoded_stream_data` résout `/Filter` et
  `/DecodeParms` indirects. Autres filtres : `Unsupported(StreamFilter)`.
- `pdf-syntax` : flux de références croisées (ISO 32000-2, 7.5.8 ; `/W`, `/Index`,
  prédicteurs), fichiers hybrides (`/XRefStm` lu après la table de sa révision) et flux
  d'objets (7.5.7) : `XrefEntry::Compressed`, objets compressés résolus depuis leur flux
  décodé, gardé dans un cache borné (8 flux, 32 Mio) et facturé une fois par décodage ;
  un flux d'objets doit avoir un `/Length` non compressé (pas de récursion). Les PDF 1.5+
  ne sont plus refusés avec `Unsupported(XrefStreams)`. `SectionKind` décrit chaque
  section ; `castle-pdf inspect` compte les objets compressés.
- Fixtures `objstm.pdf` et `hybrid.pdf` ; l'oracle qpdf compare aussi les entrées compressées.
  Divergence connue : quand la table d'un fichier hybride liste comme libres les objets
  compressés, CastlePDF (comme qpdf 10) suit le flux `/XRefStm`, alors que qpdf 12 laisse
  l'entrée libre masquer l'objet ; la fixture évite cette forme, les tests unitaires la couvrent.
- Fixture `flate.pdf` (encodeur zlib déterministe du générateur), comparaison de chaque
  flux décodé avec `qpdf --filtered-stream-data`, cible de fuzzing `flate`.
- Crate `pdf-render` : interpréteur de flux de contenu (ISO 32000-2, 8) — état graphique
  (`q Q cm w J j M d ri i gs`), chemins, remplissages (non nul, pair-impair), contours
  (épaisseur, extrémités, jonctions, tirets découpés en espace utilisateur, ligne
  d'épaisseur 0), découpe, couleurs `DeviceGray`/`DeviceRGB`/`DeviceCMYK` (conversion
  ISO 10.4.2.4, sans gestion de couleur), alpha constant de `/ExtGState`, rotation de
  page, `CropBox` et `/UserUnit`. Rasterisation par `vello_cpu` 0.3.0 derrière un
  adaptateur unique. Texte, découpe par le texte, images, XObjects, dégradés, motifs,
  autres espaces de couleur (dont `Default*`), modes de fusion, masques, fonctions de
  transfert et contenu optionnel ne sont pas encore rendus : `Rendering::skipped` les
  liste, jamais en silence. Lecture stricte : erreurs de contenu localisées
  (`Error::Content`), opérandes vérifiés, contenu non terminé refusé (`Unterminated`),
  pas de réparation. `RenderLimits` borne pixels (16 384 px par côté au plus), opérations,
  imbrication `q`, découpes actives et travail estimé du rasteriseur (segments, tuiles
  traversées, aire couverte, contours, tirets) ; coordonnées appareil bornées à 10^12.
- `pdf-document` : `Page::user_unit` (`/UserUnit`, `InvalidUserUnit` si invalide).
- `castle-pdf render <fichier> <page> <sortie.ppm> [--scale <facteur>]` : rendu d'une
  page en PPM, sans jamais écraser un fichier.
- Fixture `shapes.pdf` (3 pages), oracle de rendu `pdftoppm` en CI (comparaison tolérante
  aux bords dans les deux sens, défauts contrôlés qui doivent être détectés), cible de
  fuzzing `render` (échelle variable).
  Divergence connue : DeviceCMYK, cyan (0, 255, 255) chez CastlePDF selon ISO 10.4.2.4
  contre (0, 173, 239) chez pdftoppm 22.02 ; à revoir avec la gestion de couleur.
- `pdf-document` : `StructureErrorKind::InvalidContents` pour un `/Contents` invalide.

### Corrigé

- Shell : sous Windows, le worker est lancé sans console (`CREATE_NO_WINDOW`). L'application,
  programme GUI, lui faisait attribuer une console à chaque démarrage, soit environ 0,4 s de
  plus et peut-être une fenêtre furtive. Démarrage médian : 435 → 29 ms.
- Fixtures de test : le sous-ensemble de Source Sans 3 (étape 5.2b) portait encore le nom
  réservé « Source » de sa licence OFL ; il est renommé « CastleTest Sans ».
- Une police TrueType malformée (glyphe sans point mais avec des données) faisait paniquer
  `read-fonts` 0.41.0 : passage à `skrifa` 0.49.0 / `read-fonts` 0.46.0, test de
  régression (trouvé par la cible de fuzzing `font`).
- `pdf-render` : une forme très lointaine (par exemple un rectangle cisaillé d'un facteur
  71 069, long de plusieurs millions de pixels) ne se rendait pas pareil dans une tuile et
  dans la page entière. Le rasteriseur garde ses segments en simple précision, d'où environ
  un quart de pixel d'écart à cette distance. Désormais, chaque remplissage, contour et
  découpe est transformé, aplati (même tolérance que `vello_cpu`) et découpé au canevas en
  double précision avant d'être passé à `vello_cpu`. Trouvé par la cible de fuzzing
  `render` ; test de régression `far_geometry_renders_the_same_in_a_tile`.
- `pdf-render` : `/Contents` référençant un tableau indirect de flux (ISO 32000-2,
  7.7.3.3) était refusé avec `InvalidContents` ; trouvé par la mesure sur corpus réel.

## [0.1.0-alpha.2] — 2026-11-07

Fin de la phase P1 (lecture structurée). Le moteur lit la structure de PDF à table
xref classique : objets, révisions incrémentales, catalogue et arbre des pages, et les
expose par la CLI de diagnostic `castle-pdf inspect`. **Aucun rendu** et aucun binaire
distribué : l'application desktop n'ouvre pas encore de PDF.

### Ajouté

- `pdf-syntax` : tokenizer strict (ISO 32000-2, 7.2–7.3) avec offsets, erreurs
  `Malformed`/`Truncated` localisées et chaînes/noms décodés comptés dans le budget ;
  source d'entrée en mémoire bornée par la nouvelle limite `max_input_bytes` (512 Mio).
- `pdf-syntax` : objets PDF (ISO 32000-2, 7.3) — tableaux, dictionnaires à clés
  uniques, références `n g R`, objets indirects et streams localisés par `/Length`
  (direct ou résolu par l'appelant) sans copie des données, `endstream` vérifié
  octet par octet ; profondeur bornée (pile explicite, plafond dur de 256), chaque
  objet compté dans le budget, erreurs localisées. Première erreur de capacité :
  `Unsupported(ExternalStreamData)` pour les streams à fichier externe (`/F`).
- `pdf-syntax` : structure de fichier (ISO 32000-2, 7.5) — trait `Source` à accès
  aléatoire, `PdfFile` : en-tête `%PDF-1.0` à `2.0`, `startxref`/`%%EOF`, tables xref
  classiques à entrées de 20 octets, trailer, mises à jour incrémentales (`/Prev`,
  cycles détectés), résolution d'objet avec contrôle d'identifiant et `/Length`
  indirect sans récursion ; référence vers un objet libre ou absent = `null`.
  Refus explicites : xref streams et fichiers hybrides, chiffrement, version inconnue.
- `pdf-document` (nouvelle crate) : `Document` — catalogue, arbre des pages parcouru
  itérativement dans l'ordre du document (profondeur bornée, cycles et nœuds partagés
  refusés, `/Parent` et `/Count` vérifiés), attributs hérités `MediaBox`, `CropBox`
  (intersectée avec `MediaBox`), `Rotate` et `Resources` ; erreurs nommant l'objet fautif.
- `pdf-cli` (nouvelle crate, binaire `castle-pdf`) : `inspect <fichier>` affiche la
  structure du fichier et les pages, jamais le contenu ; codes de sortie 0/1/2/3.
  `Document::from_file` permet de rapporter la structure même si l'arbre est invalide.
- Fixture `page-tree.pdf` (arbre imbriqué, héritage, rotation) et comparaison de
  l'ordre des pages avec `qpdf --show-pages`.
- Fixture `incremental.pdf` (deux révisions) et test différentiel contre `qpdf`
  (`--check`, `--show-xref`) dans le workflow non requis `oracle`.
- Tests de propriétés (`proptest`) et fuzzing (`cargo-fuzz`, cibles `lexer`, `objects`, `xref` et `document`, workflow
  `fuzz` borné en temps).
- ADR-0002 accepté : stratégie de lecture P1.
- CI : `cargo-deny` (contrôle requis `deny`, workflow `advisories` pour RustSec),
  Dependabot (Cargo, npm, actions), cache Rust écrit seulement depuis `main`.
- Licence : double licence `Apache-2.0 OR MIT` (`LICENSE-APACHE`, `LICENSE-MIT`,
  champs `license` des paquets Cargo et npm).

### Modifié

- Projet renommé CastlePDF (auparavant CastleEngine, homonyme d'un moteur de jeu) :
  dépôt, application, identifiant `io.github.cadeneras.castlepdf`, paquets et binaire
  `castle-pdf-desktop`.

### Limites connues

- Lecture stricte, sans réparation : un fichier abîmé (offsets faux, table xref
  irrégulière, `/Length` erroné) est refusé avec une erreur localisée.
- Non pris en charge, avec erreur `Unsupported` explicite : xref et object streams
  (fichiers PDF 1.5+ compressés, très courants), fichiers hybrides, chiffrement,
  données de stream externes. Beaucoup de PDF réels sont donc encore refusés.
- Filtres de flux non décodés (`FlateDecode` etc.) : les données sont localisées, pas lues.
- Pas encore pris en charge dans le modèle : `/UserUnit`, `BleedBox`/`TrimBox`/`ArtBox`,
  `/Version` du catalogue, libellés de pages.
- Budgets par défaut non calibrés sur un corpus réel : un document de plus d'un
  million d'objets ou d'entrées xref est refusé.
- Oracles : qpdf compare entrées xref et ordre des pages des 3 fixtures publiques ;
  boîtes et rotation ne sont pas encore comparées à un oracle indépendant.

## [0.1.0-alpha.1] — 2026-11-06

Fin de la phase P0 (fondations). **Aucune lecture de PDF** : cette préversion fixe
l'architecture, le worker, les budgets et la CI. Aucun binaire n'est distribué.

### Ajouté

- Workspace Cargo (édition 2024, Rust 1.99.1 fixé), lints stricts : `unsafe` interdit,
  `unwrap`/`expect`/`panic`/indexation refusés hors tests (#1).
- `pdf-syntax` : `ResourceLimits`, budget cumulatif et `CancellationToken` (#1).
- `pdf-worker` : trames préfixées par une longueur bornée avant allocation, protocole
  versionné (v1), job synthétique annulable, binaire et client avec file d'écriture
  bornée et arrêt forcé (#1, #3).
- Shell desktop Tauri 2 / React 19 / TypeScript 6 : lance le worker, commandes IPC
  validées côté Rust, permissions minimales, CSP stricte (#4).
- Fixture PDF synthétique déterministe et son manifeste (#5).
- Mesure du plancher mémoire au repos (`scripts/measure-memory.mjs`) : référence
  Windows, relevés indicatifs CI sur trois OS et WSL2 (#5, #7, #9).
- CI GitHub Actions sur Ubuntu, Windows et macOS : kit, Rust, frontend, build natif
  release ; workflow manuel de mesure mémoire (#2, #4, #7, #8).
- Protection de `main` et des tags `v*` documentée (#6).
- ADR-0001 accepté ; README du projet avec badges de versions vérifiés (#10).

### Limites connues

- Aucun PDF n'est lu ni rendu ; la lecture structurée arrive en P1.
- Pas d'installateur, de signature de binaires ni de mise à jour automatique.
- Isolation OS du worker (sandbox) non implémentée.
- Licence du projet non décidée.

[Non publié]: https://github.com/CadenEras/CastlePDF/compare/v0.1.0-alpha.2...HEAD
[0.1.0-alpha.2]: https://github.com/CadenEras/CastlePDF/releases/tag/v0.1.0-alpha.2
[0.1.0-alpha.1]: https://github.com/CadenEras/CastlePDF/releases/tag/v0.1.0-alpha.1
