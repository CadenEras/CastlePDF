# ADR-0005 — Polices, texte et images (tranche 5 de P2)

Statut : accepté le 2026-11-10
Date : 2026-11-10
Responsable : Mélissa Gries (maintenance du dépôt)

## Contexte et contraintes

ADR-0003 a placé en tranche 5 de P2 « polices (`skrifa`, ADR dédiée) et images DCT/Flate ».
La mesure sur corpus (`docs/QUALITY.md`, 2026-11-09) chiffre le manque. Sur les premières
pages rendues, le texte manque sur 1 660 des 1 913 pages SafeDocs et sur 1 991 des 2 206
pages UNSAFE. Les XObjects, c'est-à-dire surtout les images et les formulaires, manquent sur
1 347 et 1 416 pages. Une page sans son texte n'est pas lisible : c'est l'écart de fidélité
dominant.

Le travail se découpe ainsi :
- **Polices incluses dans le PDF.** `FontFile2` (TrueType), `FontFile3` avec `/Type1C`,
  `/CIDFontType0C` (CFF nu) ou `/OpenType`, et `FontFile` (Type 1 PostScript).
- **Polices non incluses.** Les 14 polices standard (Helvetica, Times, Courier, Symbol,
  ZapfDingbats) et les polices nommées mais absentes, comme Arial ou Calibri.
- **Polices Type 0 / CID** (`Identity-H`, `Identity-V`, CMap incluses) et **polices Type 3**,
  dont les glyphes sont des procédures de contenu.
- **Images.** XObjects `/Image` et images en ligne (`BI … ID … EI`), filtres Flate et DCT
  (JPEG).

Contraintes :
- tout fichier de police ou d'image est une entrée non fiable : budgets cumulés, annulation,
  aucun panic ;
- pas de `unsafe` (`unsafe_code = "forbid"` dans le workspace), et pas de code natif dans
  le cœur ;
- licences compatibles avec Apache-2.0 OU MIT, y compris pour les fichiers de police livrés ;
- résultat déterministe sur les trois OS, pour que les goldens restent comparables : aucune
  police du système ;
- chaque capacité absente reste signalée (`Unsupported`, bits `Skipped`), jamais rendue en
  silence comme si elle était exacte.

## Options

### Lecture des polices incluses

1. **Écrire nos propres lecteurs** TrueType et CFF. C'est cohérent avec « implémenter la
   sémantique PDF ici », mais ces formats sont des formats de police, pas la sémantique PDF.
   C'est un gros travail, et un risque de sécurité supplémentaire sans gain.
2. **`skrifa` 0.44.0** (Google, fontations ; MIT OU Apache-2.0). Elle lit les contours `glyf`,
   `CFF` et `CFF2`. `skrifa` 0.44.0 et sa dépendance `read-fonts` 0.41.0 déclarent
   `#![forbid(unsafe_code)]`. Elle est déjà verrouillée dans `Cargo.lock`, comme dépendance
   optionnelle de `vello_cpu` (fonction `text`, désactivée). Elle ne lit pas Type 1.
3. **`ttf-parser`** (MIT OU Apache-2.0). Il est sûr et lit TrueType et CFF, mais il est moins
   actif que fontations, sur lequel s'appuient Chrome et `vello`. Il ne lit pas Type 1 non plus.
4. **FreeType par FFI.** C'est le plus complet (Type 1 compris), mais c'est du code natif et
   une surface d'attaque historique. Refusé par les règles du projet.

### Rendu des glyphes

- **(a) La fonction `text` de `vello_cpu`** (`glyph_run`, crate `glifo`). Elle attend une
  police OpenType complète et gère son propre cache. Mais elle impose sa propre logique de
  transformation, de découpe et de mode de rendu, alors que le PDF les définit (matrice de
  texte, `Tz`, `Ts`, `Tr`, découpe par le texte).
- **(b) Convertir les contours `skrifa` en chemins `kurbo`,** remplis ou tracés par le
  pipeline vectoriel existant. Un seul chemin de rendu suffit pour les transformations, la
  découpe et les modes `Tr`, et les glyphes en cache restent en unités de police.

### Polices non incluses

1. **Polices du système.** Le rendu varie d'un OS à l'autre et les goldens ne sont pas
   reproductibles. De plus, lire les dossiers de polices étend la surface d'entrée.
2. **URW base35** (Nimbus, Ghostscript et Poppler). C'est le plus fidèle visuellement, mais
   sous AGPL : refusé pour une application distribuée sous licence permissive.
3. **Polices Foxit de PDFium** (BSD). pdf.js les a utilisées pendant des années, puis les a
   abandonnées le 2026-10-31 (commit `f16437f`).
4. **Le jeu actuel de pdf.js, entièrement sous SIL OFL 1.1.** Il comprend Arimo
   (Helvetica/Arial), Tinos (Times), Liberation Mono (Courier), et Noto Sans Symbols et
   Symbols 2 (Symbol, ZapfDingbats). Arimo et Tinos ont les mêmes largeurs qu'Arial et Times
   New Roman, Liberation Mono que Courier New. Il n'y a qu'une seule licence de police, et ce
   choix est éprouvé par pdf.js. Les fichiers complets pèsent environ 2 Mo par famille ; pdf.js
   n'en garde que les glyphes utiles, pour environ 0,8 Mo de données de glyphes au total.

### Images JPEG

1. **Écrire notre décodeur DCT.** C'est faisable, mais long et sans valeur propre au PDF.
2. **`zune-jpeg` 0.5.15** (MIT OU Apache-2.0 OU Zlib), déjà prévu par ADR-0002. Avec
   `default-features = false` et la seule fonction `std`, il déclare `forbid(unsafe_code)` :
   les chemins SIMD `x86` et `neon` sont alors exclus. Le décodage scalaire est plus lent
   (coût à mesurer).
3. **`jpeg-decoder`**. Il n'est plus maintenu, et son successeur annoncé est justement
   `zune-jpeg`.

## Décision

| Sujet | Proposition |
| --- | --- |
| Lecteur de polices | `skrifa = "=0.44.0"` (même version que celle déjà verrouillée ; amendée en 0.49.0, voir plus bas), sans fonctions par défaut sauf `std`. Utilisé seulement dans `pdf-render`, dans un module `text`. Un CFF nu (`/Type1C`, `/CIDFontType0C`) est lu par `read-fonts` comme table `CFF`. Si `skrifa` exige une police OpenType complète, on l'enveloppe dans un conteneur OpenType minimal construit en mémoire ; à valider au prototype |
| Rendu des glyphes | Option (b) : les contours sont convertis en chemins `kurbo` (crate déjà présente). Positions selon ISO 32000-2, 9.4 : matrice de texte, `Tc`, `Tw`, `Tz`, `TL`, `Ts`, et largeurs `/Widths`, `/W` ou métriques AFM des polices standard. Modes `Tr` : 0 à 2 et 7 rendus, 4 à 6 (découpe) toujours signalés par `TextClip`, 3 (invisible) rien. Pas de hinting. La fonction `text` de `vello_cpu` reste désactivée |
| Encodages | `/Encoding` (Standard, WinAnsi, MacRoman, MacExpert, `/Differences`), noms de glyphes vers Unicode via l'Adobe Glyph List (BSD-3-Clause), tables `cmap` des polices TrueType, y compris symboliques (3,0) et (1,0). Type 0 : `Identity-H`, `Identity-V` et CMap incluses. Les CMap prédéfinies non Identity (CJK) sont `Unsupported`, signalées |
| Type 1 inclus (`FontFile`) | Non lu par `skrifa`. En P2, la police est remplacée comme une police non incluse, et la page porte un nouveau bit `Skipped` (`FontSubstituted`). La première étape de la tranche ajoute à `survey` le décompte des types de police rencontrés. Un lecteur Type 1 maison (déchiffrement `eexec`, charstrings) fera l'objet d'une décision mesurée |
| Polices non incluses | Jeu OFL 1.1 de pdf.js : Arimo (Regular, Bold), Tinos (Regular, Bold, Italic, BoldItalic), Liberation Mono (Regular, Bold), Noto Sans Symbols et Symbols 2. Les obliques de Helvetica et Courier sont obtenues en inclinant la police droite, comme l'original. Une police nommée mais absente (Arial, Calibri…) est remplacée selon son nom et les drapeaux de `/FontDescriptor` (serif, chasse fixe, gras, italique), et signalée `FontSubstituted`. Les positions viennent toujours des largeurs du PDF ; seul le dessin du glyphe est remplacé |
| Livraison des polices | Fichiers réduits aux glyphes des encodages standard (Latin, Symbol, Dingbats) par un script reproductible (`fonttools` à version épinglée, lancé hors CI). Objectif : 1,5 Mo au plus, mesuré. Ils sont commités dans `crates/pdf-render/fonts/` avec leurs licences, sources et sha256, et inclus par `include_bytes!` derrière une fonction `standard-fonts`, active par défaut. Sans elle, un usage sans interface fournit ses propres polices ou obtient `FontSubstituted` sans dessin. Ces fichiers sont recensés dans un inventaire tiers séparé, car `cargo-deny` ne voit pas les fichiers de données |
| Type 3 | Les procédures de glyphe sont exécutées par l'interpréteur existant, avec une profondeur limitée (pas de Type 3 dans un Type 3) et le budget de la page. Étape après les polices incluses ; signalé `Text` d'ici là |
| Images | XObjects `/Image` et images en ligne. Filtres Flate (existant) et DCT par `zune-jpeg = "=0.5.15"`, `default-features = false`, `features = ["std"]`. JPX, JBIG2, CCITT, LZW, RunLength, ASCII85 et ASCIIHex restent `Unsupported` (bit `Image`), comme les filtres secondaires renvoyés en P3 par ADR-0003. Espaces de couleur DeviceGray, RGB, CMYK, Indexed, ICCBased réduit à `/N` (sans gestion de couleur) et Cal* ramenés à Device*. Separation, DeviceN et Lab : bit `ColorSpace`. 1, 2, 4, 8 et 16 bits par composante, `/Decode`, `/ImageMask`. `/SMask` d'une image de même taille appliqué comme alpha, sinon bit `SoftMask`. Dessin par la peinture image de `vello_cpu`, matrice de l'espace image vers la page ; qualité d'échantillonnage à fixer au prototype |
| Limites | Nouvelles limites de `RenderLimits` : pixels d'image décodés (largeur × hauteur vérifiées avant toute allocation), segments par contour de glyphe, octets du cache de polices et de glyphes par document. Le programme de police décodé est facturé à `max_decoded_bytes`. `zune-jpeg` reçoit des dimensions maximales dans ses options. Chaque glyphe et chaque image est facturé au budget d'objets |
| Caches | Une tuile réinterprète toute la page (limite ouverte de la tranche 4). Décoder chaque fois une police ou une image serait prohibitif. Le worker garde donc par document un cache LRU borné en octets, partagé entre les tâches du document : polices analysées, contours de glyphes et images décodées. Compté dans la mémoire du worker, vidé à la fermeture du document |
| Signalement | Les bits `Text` et `Image` ne sont levés que pour ce qui n'est pas rendu. Le nouveau bit `FontSubstituted` (protocole worker v4) apparaît dans la note « Rendu partiel » de l'UI : « polices remplacées » |

## Conséquences et validation

- **Fixtures synthétiques** (`scripts/make-fixtures.mjs`), sous-ensembles des polices OFL
  ci-dessus :
  - une police TrueType incluse avec `/Widths` ;
  - une police CFF nue (`/Type1C`) ;
  - une police Type 0 `Identity-H` avec `/W` ;
  - chacune des 14 polices standard non incluse ;
  - une police nommée absente (Arial) ;
  - une police Type 3 ;
  - une image JPEG (DCT) en niveaux de gris, RGB et CMYK ;
  - une image Flate indexée de 1 bit, une `/ImageMask` et une image avec `/SMask` ;
  - une image en ligne.
- **Oracles :**
  - `pdftoppm` (déjà en CI) sur ces fixtures, avec une tolérance motivée sur l'anticrénelage
    du texte. Les écarts dus à des polices de remplacement différentes sont documentés, pas
    masqués.
  - Positions des glyphes comparées à `pdftotext -bbox`, à 1/10 de point près.
- **Limites :** taille limite et limite + 1 pour les pixels d'image, les segments de glyphe
  et le cache. Une image de 100 000 × 100 000 est refusée avant allocation. Une police
  dont le CFF boucle sur ses sous-routines ou dépasse sa pile échoue proprement.
- **Fuzzing :** une cible `font` (octets arbitraires lus comme police incluse, puis
  rendu de quelques glyphes) et une cible `image` (octets arbitraires décodés comme image
  DCT ou Flate avec un dictionnaire arbitraire). Propriétés : aucun panic, travail borné.
- **Mesures :**
  - relancer `survey` sur le même corpus, pour mesurer la part de pages encore marquées
    `Text`, `Image` et `FontSubstituted` ;
  - coût par tuile d'une page riche en texte et d'une page avec une grande image, avant et
    après les caches ;
  - mémoire du worker sur un parcours de pages ;
  - temps du décodage JPEG scalaire.
- **Taille livrée :** taille du binaire `pdf-worker` avant et après, polices comprises,
  publiée avec la PR.
- **Risques :**
  - **Polices de remplacement.** Une police remplacée s'affiche différemment de l'original ;
    c'est signalé, jamais présenté comme exact.
  - **Type 1.** Les documents anciens qui incluent des polices Type 1 restent en
    remplacement jusqu'à la décision mesurée.
  - **JPEG scalaire.** Il est plus lent que la version SIMD, qu'on refuse à cause de
    l'`unsafe`.
- **Retour arrière :**
  - polices et images sont isolées dans des modules de `pdf-render` ;
  - retirer la fonction `standard-fonts` ramène au remplacement sans dessin ;
  - chaque dépendance est épinglée et vérifiée par `cargo-deny`.
- **Amendement du 2026-11-10 (étape 5.2a) :** la cible de fuzzing `font` a trouvé une panique
  dans `read-fonts` 0.41.0. Un glyphe simple qui a des données mais aucun point fait indexer
  un tableau vide (`glyf.rs`, `read_points_fast`). La correction est publiée depuis
  `read-fonts` 0.42.0. `skrifa` passe donc à `=0.49.0` (`read-fonts` 0.46.0), et les deux
  déclarent toujours `#![forbid(unsafe_code)]`. `Cargo.lock` garde `skrifa` 0.44.0 pour la
  dépendance optionnelle `glifo` de `vello_cpu`, qui n'est pas compilée (fonction `text`
  désactivée). Le cas est un test de régression de `pdf-render`
  (`a_glyph_without_points_does_not_panic`).
- **Acceptation (2026-11-10) :**
  - ADR-0002 est amendé sur la ligne « Réutiliser » : `skrifa` et `zune-jpeg` sont retenus,
    sans le code SIMD de `zune-jpeg` ;
  - `docs/ROADMAP.md` découpe la tranche 5 en étapes, dans cet ordre :
    1. décompte des polices dans `survey` ;
    2. polices incluses TrueType et CFF ;
    3. polices non incluses ;
    4. images ;
    5. Type 3.
