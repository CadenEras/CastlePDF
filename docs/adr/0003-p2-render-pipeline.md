# ADR-0003 — P2 : ordre des travaux, rasterisation, Flate et chaîne de rendu

Statut : accepté le 2026-11-07
Date : 2026-11-07
Responsable : Mélissa Gries (maintenance du dépôt)

## Contexte et contraintes

P1 (`v0.1.0-alpha.2`) lit la structure des PDF à table xref classique. P2 doit livrer
une première page rendue, une UI réactive et un worker à mémoire contrôlée
(`docs/ROADMAP.md`). Quatre constats conditionnent la façon d'y arriver :

- **Les PDF réels sont compressés.** Presque tous les flux de contenu utilisent
  `FlateDecode`, et la plupart des fichiers récents (PDF 1.5+) rangent leur table xref
  et une partie de leurs objets dans des flux compressés. La feuille de route place ces
  capacités en P3 : un rendu en P2 ne s'appliquerait qu'à nos fixtures non compressées.
- **Trois processus, une IPC.** Le rendu tourne dans le worker ; les pixels traversent
  le tube worker → shell, plafonné à 1 Mio par trame (ADR-0001), puis l'IPC Tauri vers
  la WebView. Envoyer des images en JSON ou en base64 est exclu (`docs/ARCHITECTURE.md`).
- **La rasterisation n'est pas de la sémantique PDF.** ADR-0002 prévoit de réutiliser
  une brique Rust pure, choisie entre `tiny-skia` et `vello_cpu` en P2.
- **Architectures réellement testées.** ADR-0001 met l'ARM64 hors périmètre, mais le
  runner `macos-latest` est Apple Silicon (`macos-26-arm64`) : la seule cible macOS
  testée est arm64, et macOS Intel n'est compilé nulle part.

Contraintes : `unsafe` interdit dans le projet, licence `Apache-2.0 OR MIT` et
politique `cargo-deny`, aucun code natif ajouté, budgets cumulatifs et annulation,
CI gratuite.

## Options

**Ordre des travaux.** (a) Garder la feuille de route : rendu sur PDF non compressés
en P2, Flate et flux xref en P3. (b) Avancer Flate et les flux xref/objets au début de
P2, avant l'interpréteur graphique. (c) Tout traiter en même temps.

**Rasterisation** (relevé crates.io du 2026-11-07) :

| Critère | `tiny-skia` 0.12.0 | `vello_cpu` 0.3.0 |
| --- | --- | --- |
| Licence | BSD-3-Clause | Apache-2.0 OR MIT (identique au projet) |
| `unsafe` dans la crate | 152 occurrences (SIMD) | `#![forbid(unsafe_code)]` ; SIMD délégué à des dépendances à inventorier |
| Maintenance | mature, mise à jour 2026-02, reprise par Linebender | active, mise à jour 2026-10, Linebender |
| Maturité | large usage (resvg) | API 0.x, en évolution |
| Utilisé par | resvg | hayro (moteur PDF Rust) |

**Décompression Flate :**

| Critère | `miniz_oxide` 0.9.1 | `zlib-rs` 0.6.8 |
| --- | --- | --- |
| Licence | MIT OR Zlib OR Apache-2.0 | Zlib |
| `unsafe` | `#![forbid(unsafe_code)]` ; les 3 mentions du mot sont des commentaires | 456 occurrences |
| Performance | correcte | plus rapide |
| API sortie bornée | inflate incrémental, taille de sortie contrôlable | idem |

**Transport des pixels vers la WebView.** (a) Commande Tauri renvoyant des octets bruts
(`tauri::ipc::Response`, reçus comme `ArrayBuffer`), dessinés via `ImageData` dans un
canvas. (b) Protocole d'URI personnalisé servant des tuiles encodées (PNG) à des
éléments `<img>` : cache du navigateur, mais coût d'encodage et de décodage. (c) JSON
ou base64 : exclu.

## Décision

| Sujet | Proposition |
| --- | --- |
| Ordre de P2 | Option (b), en tranches vérifiables : 1. filtre Flate (et prédicteurs PNG) sous budget ; 2. flux xref et flux d'objets (PDF 1.5) ; 3. interpréteur de contenu minimal (chemins, remplissages, contours, transformations, découpe) rendu par `vello_cpu` ; 4. tuiles, cache et IPC jusqu'à la première page affichée ; 5. polices (`skrifa`, ADR dédiée) et images DCT/Flate. La feuille de route est amendée en conséquence ; P3 garde texte, `ToUnicode`, recherche et les filtres secondaires |
| Rasterisation | `vello_cpu` 0.3, version exacte épinglée, `default-features = false` avec les seules features nécessaires (pas `png` ni `text`). Licence identique, crate sans `unsafe`, mainteneurs actifs. Risques acceptés : API 0.x (adaptateur interne unique, mise à jour par PR revue) et `unsafe` des dépendances SIMD, inventorié dans la PR d'introduction. `tiny-skia` reste l'alternative de retour arrière |
| Flate | `miniz_oxide` 0.9 (sans `unsafe`), inflate incrémental : chaque bloc de sortie est facturé à `max_decoded_bytes` avant d'être conservé ; arrêt au premier dépassement. Ratio et taille de sortie bornés, testés avec une bombe de décompression |
| Tuiles | 256 × 256 pixels d'appareil, RGBA8 non prémultiplié (format de `ImageData`) : 256 Kio par tuile, sous le plafond de trame de 1 Mio sans le relever. Clé : document, page, révision, rotation, zoom, rectangle, génération de requête (`docs/ARCHITECTURE.md`) |
| Transport | Option (a) : commande Tauri renvoyant les octets bruts, pas de JSON ni de base64. À valider par mesure sur les trois OS ; repli documenté : protocole d'URI personnalisé |
| Caches et annulation | Cache LRU de tuiles décodées dans le shell, compté en octets (128 Mio, `docs/PERFORMANCE.md`), buffers en vol comptés à part. Chaque changement de vue incrémente une génération ; le worker abandonne les tuiles d'une génération périmée |
| Oracles de rendu | `pdf.js` (Apache-2.0) et `pdftoppm` (poppler, GPL, outil de test jamais livré ni lié) en CI Linux, dans un espace séparé comme `fuzz/`. `hayro` sert à comparer l'interprétation, pas la rasterisation, puisqu'il utilise aussi `vello_cpu`. Goldens avec tolérance motivée et revue humaine |
| Architectures | Corriger ADR-0001 : macOS arm64 devient la cible macOS testée ; ajouter en P2 des jobs non requis pour les crates du moteur sur Linux arm64 et Windows arm64 (runners gratuits pour dépôt public) et un build macOS x86_64. Aucune compatibilité annoncée sans job vert |

## Conséquences et validation

- Chaque brique arrive dans sa propre PR, avec `cargo-deny`, l'inventaire de ses
  dépendances (y compris `unsafe` et code natif éventuel) et des tests de limites.
- Flate : limite exacte et limite + 1 sur `max_decoded_bytes`, flux tronqué, données
  corrompues, bombe de décompression, prédicteurs ; fuzzing d'une cible `flate`.
- Flux xref et d'objets : nouvelles fixtures générées, comparaison `qpdf --show-xref`,
  fichiers hybrides acceptés ; `Unsupported(XrefStreams)` disparaît.
- Rendu : goldens sur fixtures générées, comparaison à `pdftoppm` et `pdf.js` avec
  tolérance chiffrée ; aucune régénération pour masquer un écart.
- Mesures (`docs/PERFORMANCE.md`) : première page d'un document de 10 pages, mémoire du
  worker et du shell pendant un défilement, coût d'une tuile à travers les deux IPC.
  Le transport retenu est confirmé ou remplacé d'après ces mesures.
- Retour arrière : la rasterisation passe par un adaptateur interne, ce qui permet de
  basculer vers `tiny-skia` ; Flate est derrière l'interface de filtre de `pdf-syntax`.
- Acceptée le 2026-11-07 : `docs/ROADMAP.md` (contenu de P2 et P3) et la ligne
  « OS minimums » d'ADR-0001 sont amendés en conséquence.

## Amendement du 2026-11-10 : cellules de rasterisation

Le fuzzing de `render` a montré qu'une tuile et la page entière ne donnaient pas
exactement les mêmes pixels.
- **Cause :** `vello_cpu` calcule en simple précision, à partir de coordonnées absolues,
  la couverture d'un bord. Elle dépend donc de l'endroit où le bord tombe sur le canevas.
  Translater la page d'un nombre entier de pixels change l'arrondi.
- **Ampleur :** l'écart était d'au plus 1 niveau par dessin, mais il s'additionnait
  quand des bords se superposaient (2 pour un même tracé dessiné deux fois).
- **Essais sans effet :** caler les coordonnées sur une grille de 1/128 px, aligner la
  tuile sur la grille de 4 px et élargir la marge de découpe.

Décision : l'adaptateur découpe le canevas en **cellules de 256 px** (le côté des
tuiles du visualiseur), sur une grille fixée au coin supérieur gauche de la page.
- Chaque cellule a son propre contexte `vello_cpu` et sa propre origine.
- Chaque cellule aplatit et découpe elle-même les tracés en double précision à partir
  des coordonnées de la page, puis les ramène à son origine.
- `render_page` et `render_area` assemblent les mêmes cellules. Une zone quelconque
  donne donc **exactement** les pixels de la page. Les tests de tuiles, la cible de
  fuzzing et la documentation de `render_area` exigent maintenant l'égalité exacte.
- Une tuile du visualiseur est une seule cellule : elle ne coûte pas plus qu'avant.

Une page entière est rasterisée cellule par cellule. Un tracé est aplati une fois par
cellule qu'il touche, et l'estimation de travail (`max_path_work`) le compte. Une
découpe qui ne touche pas une cellule la masque jusqu'à son retrait, au lieu d'y être
poussée. Le coût est mesuré par `survey` (`docs/QUALITY.md`).
