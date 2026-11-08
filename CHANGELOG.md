# Journal des versions

Format inspiré de [Keep a Changelog](https://keepachangelog.com/fr/1.1.0/).
Versions selon [SemVer](https://semver.org/) ; règles propres à `0.x` dans
[docs/GIT.md](docs/GIT.md). Une préversion `alpha` marque la fin de chaque phase de la
[feuille de route](docs/ROADMAP.md).

## [Non publié]

### Ajouté

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
