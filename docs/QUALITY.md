# Tests et définition de terminé

## Matrice de contrôles

| Niveau | Couverture attendue | Fréquence |
| --- | --- | --- |
| Kit agentique | JSON, règles, liens, hooks et cas négatifs | Chaque PR |
| Rust | fmt, Clippy, unitaires, intégration, doctests, features supportées | Chaque PR P0+ |
| UI | lint, types, tests interactions, build | Chaque PR P0+ |
| Desktop | compilation native, IPC, ouverture/fermeture/sauvegarde | Chaque PR pertinente, trois OS avant release |
| Parsing | corpus normal/malformé, limites, propriétés | Chaque changement de parseur |
| Rendu | goldens et comparaison à des moteurs indépendants | Chaque changement de rendu |
| Fuzzing | entrées arbitraires avec budgets, crashs minimisés | Smoke court en PR, campagne planifiée |
| Sécurité | dépendances, licences, secrets, capabilities et `unsafe` | Chaque PR concernée + périodique |
| Performance | temps, mémoire et allocations sur corpus versionné | Chaque changement critique + avant release |

## Corpus

Conserver des fixtures synthétiques minimales et des cas publics redistribuables.
Chaque entrée a un identifiant, hash, origine, licence, fonctionnalités et résultat
attendu. Les documents privés restent hors Git, hors logs et hors artefacts publics.
Un PDF « public sur le web » n'est pas nécessairement redistribuable.

Inclure progressivement : objets directs/indirects, xref classique et en flux,
object streams, révisions incrémentales, Unicode, arbres cycliques, filtres imbriqués,
images gigantesques, polices endommagées, chiffrement non supporté, fichiers tronqués
et incohérences de longueur. Un fichier non supporté doit échouer proprement.

### Mesure sur corpus externe

Les grands corpus web servent à mesurer, jamais de fixtures : leurs licences sont
inconnues, ils ne vont ni dans Git ni dans la CI. Procédure locale :

1. Télécharger des archives de [SafeDocs CC-MAIN-2021-31-PDF-UNTRUNCATED](https://digitalcorpora.org/cc-main-2021-31-pdf-untruncated/)
   (1 000 PDF par zip) et, pour la robustesse, de
   [CC-MAIN-2021-31-UNSAFE](https://corp.digitalcorpora.org/corpora/files/CC-MAIN-2021-31-UNSAFE/)
   (fichiers connus pour faire planter ou bloquer des parseurs).
2. Les extraire dans un répertoire dédié hors du dépôt, de préférence dans une VM ou WSL ;
   ne pas les ouvrir avec un lecteur PDF.
3. `castle-pdf survey <répertoire> --jobs 8` en build release, mémoire par processus
   bornée (`ulimit -v`). Ne publier que le rapport agrégé ; un cas utile est réduit en
   fixture synthétique, pas copié.

Mesure du 2026-11-08 (tranche corpus, correction `/Contents` indirect comprise, release, Linux WSL, `ulimit -v` 4 Gio,
`--jobs 8`) :

| Corpus | Fichiers | Ouverts | 1re page rendue | Crashs | Timeouts | Temps médian / max |
| --- | --- | --- | --- | --- | --- | --- |
| SafeDocs `0000.zip` + `0001.zip` | 2 000 | 93,1 % | 92,1 % (91,4 % avant correction) | 0 | 0 | 4 ms / 274 ms |
| UNSAFE `common-crawl-x0000.zip` | 2 636 | 78,1 % | 76,4 % (75,7 % avant correction) | 0 | 0 | 3 ms / 371 ms |

Causes principales de refus : chiffrement (86), `startxref` absent, surtout fichiers
tronqués (328), offsets xref faux (`ObjectIdMismatch`, `InvalidXrefEntry`, `MissingXref` :
183), filtres LZW, ASCII85 et CCITT (33), opérateurs mal placés (17). Sur les pages
rendues, le texte (`Text`) et les XObjects manquent presque partout : ce sont les écarts
de fidélité dominants.

Mesure du 2026-11-09, mêmes fichiers et même protocole, avec la reconstruction de l'index
(ADR-0004) :

| Corpus | Ouverts | dont par réparation | 1re page rendue | Crashs | Timeouts | Temps médian / max |
| --- | --- | --- | --- | --- | --- | --- |
| SafeDocs (2 000) | 97,0 % (93,1 %) | 78 (3,9 %) | 95,6 % (92,1 %) | 0 | 0 | 5 ms / 314 ms |
| UNSAFE (2 636) | 86,5 % (78,1 %) | 223 (8,4 %) | 83,6 % (76,4 %) | 0 | 0 | 3 ms / 395 ms |

Les réparations SafeDocs comptent 27 objets retrouvés à la demande (`ObjectIdMismatch`
dans un fichier qui s'ouvre strictement) ; les autres réparent l'index à l'ouverture.
Refus restants : chiffrement (42 + 52), arbre des pages abîmé (`InvalidNode`,
`MissingKid` : 2 + 180, hors ADR-0004), `startxref` absent sans catalogue retrouvable
(65 UNSAFE, surtout des fichiers coupés avant leurs objets), en-tête invalide (10 + 55).
Types de police déclarés par les premières pages (2026-11-10, même protocole, ADR-0005
étape 5.1 ; une page compte une fois par type) :

| Type | SafeDocs (1 940 ouverts) | UNSAFE (2 282 ouverts) |
| --- | --- | --- |
| TrueType inclus (simple / CID) | 848 / 846 | 648 / 892 |
| Non incluse (simple / CID) | 441 / 25 | 502 / 7 |
| 14 polices standard | 284 | 551 |
| CFF inclus (`Type1C` / `CidType0C`) | 256 / 48 | 248 / 47 |
| Type 1 inclus (`FontFile`) | 29 | 30 |
| Type 3 | 17 | 7 |
| OpenType inclus | 1 | 0 |
| Dictionnaire invalide | 1 | 26 |

Les polices TrueType et CFF incluses, lues par `skrifa`, et les polices de remplacement
couvrent l'essentiel. Le Type 1 inclus, que `skrifa` ne lit pas, touche environ 1,5 % des
premières pages ouvertes : il reste remplacé (ADR-0005).

Après la lecture des polices TrueType incluses (2026-11-10, ADR-0005 étape 5.2a, même
protocole) :
- premières pages rendues où du texte manque encore : 1 660 → 990 (SafeDocs), 1 991 → 1 238
  (UNSAFE) ;
- taux d'ouverture et de rendu inchangés, aucun crash ni timeout ;
- temps médian par fichier : de 5 ms à 8 ou 13 ms selon l'exécution (SafeDocs), car les
  polices sont relues à chaque rendu.

Après les polices CFF et OpenType incluses (étape 5.2b, même protocole) : 990 → 726
(SafeDocs) et 1 238 → 989 (UNSAFE) premières pages où du texte manque, sans crash ni timeout ;
`cff.pdf` identique à `pdftoppm` à 216 dpi (0,000 %) ; fuzzing `font` TrueType et CFF :
180 345 exécutions en 900 s, aucun crash. Le texte restant vient des polices standard, non
incluses, Type 1 et Type 3 (étapes 5.3 et 5.5).

Après les polices de remplacement (étape 5.3a, même protocole) :
- premières pages où du texte manque : 726 → 80 (SafeDocs), 989 → 61 (UNSAFE) ;
- pages au moins en partie dessinées avec une police remplacée, et signalées : 649 et 906 ;
- taux, crashs et timeouts inchangés ;
- temps médian par fichier : 9 à 12 ms (SafeDocs).

Le texte restant vient de Symbol, ZapfDingbats, polices symboliques, CID non incluses et
Type 3.

Après les XObjects image (étape 5.4a, même protocole) :
- premières pages où un XObject n'est pas rendu : 1 347 → 267 (SafeDocs), 1 416 → 248
  (UNSAFE). Le reste correspond surtout aux XObjects de formulaire, pas encore rendus ;
- images non dessinées et signalées :
  - filtre non pris en charge ou image trop grande (`Image`) : 123 et 169 pages ;
  - masque non applicable (`SoftMask`) : 82 et 41 ;
  - espace de couleur Lab, Separation ou DeviceN (`ColorSpace`) : 373 et 399, tracés compris ;
- premières pages rendues : 1 913 → 1 911 et 2 206 → 2 205. Les 3 pages perdues dépassent le
  budget de données décodées d'un rendu (256 Mio, `limit/DecodedBytes`) avec leurs images ;
- aucun crash ni timeout ;
- temps par fichier : médiane 12 → 21 ms, p95 43 → 211 ms (SafeDocs), car le décodage des
  images est refait à chaque rendu ;
- fuzzing `image` : 428 313 puis 96 969 exécutions (15 et 5 min), aucun crash ;
- `images.pdf` et page 1 de `jpeg.pdf` identiques à `pdftoppm` (0,000 %).

Après les images en ligne (étape 5.4b, même protocole, deux exécutions) :
- images non dessinées et signalées (`Image`) : 123 → 112 premières pages (SafeDocs),
  169 → 149 (UNSAFE). Le reste : surtout des filtres secondaires (ASCIIHex, ASCII85, LZW,
  RunLength, CCITT, JPX, JBIG2), renvoyés en P3 ;
- premières pages rendues inchangées (1 911 et 2 205) : un espace de couleur absent des
  ressources ne fait sauter que l'image (`ColorSpace`), pas la page ;
- aucun crash ni timeout ; temps par fichier : médiane 21 à 22 ms, p95 215 à 254 ms
  (SafeDocs ; la dispersion entre deux exécutions est du même ordre que l'écart) ;
- fuzzing `render` (graine d'images en ligne ajoutée) : 382 152 puis 348 808 exécutions
  (5 min chacune), aucun crash ;
- `inline-images.pdf` identique à `pdftoppm` (0,000 %) ; oracle qpdf : 12 fixtures,
  37 flux identiques.

Après les XObjects de formulaire et les cellules de rasterisation (même protocole) :
- premières pages où un XObject n'est pas rendu : 267 → 3 (SafeDocs), 248 → 13 (UNSAFE) ;
- signalés en plus, parce que les formulaires sont maintenant dessinés : modes de fusion et
  groupes à élimination (`BlendMode`) 38 → 75 et 17 → 32, masques adoucis 82 → 101 et
  41 → 51, contenu optionnel 40 → 54 et 40 → 53 ;
- premières pages rendues : SafeDocs inchangé (1 911), UNSAFE 2 205 → 2 204. La page
  perdue dépasse le budget de données décodées (256 Mio, `limit/DecodedBytes`) avec les
  images de ses formulaires ;
- aucun crash ni timeout ;
- temps par fichier (SafeDocs) : médiane 21 → 24 ms, p95 222 ms comme avant. Une page
  entière est maintenant rasterisée en cellules de 256 px ; une tuile du visualiseur reste
  une seule cellule ;
- `forms.pdf` identique à `pdftoppm` (0,000 %) ; les autres cas de l'oracle sont
  inchangés ;
- tuiles et page identiques au pixel près (tests `area`, cible `render`). Avant les
  cellules, la cible `render` trouvait en 5 min un écart de 2 niveaux : le même tracé
  dessiné deux fois. Avec les cellules : 537 906 puis 511 971 exécutions (10 min chacune,
  la seconde avec une zone placée selon l'entrée) sans écart.

Après le remplacement de Symbol et ZapfDingbats (étape 5.3b, même protocole) :
- premières pages au texte non dessiné (`Text`) : 82 → 77 (SafeDocs), 62 → 54 (UNSAFE).
  Le reste vient surtout des polices Type 3 (étape 5.5), des polices CID non incluses et des
  autres polices symboliques (Wingdings…) ;
- premières pages rendues inchangées (1 911 et 2 204), aucun crash ni timeout ;
- pas de comparaison à `pdftoppm` pour ces polices : il dessine Symbol et ZapfDingbats avec
  les polices URW du système, d'autres glyphes.

Après les polices Type 3 (étape 5.5, même protocole) :
- premières pages au texte non dessiné (`Text`) : 77 → 61 (SafeDocs), 54 → 49 (UNSAFE). Le
  reste vient surtout des polices CID non incluses et des polices symboliques autres que
  Symbol et ZapfDingbats ;
- premières pages rendues inchangées (1 911 et 2 204), aucun crash ni timeout, temps par
  fichier inchangé (médiane 24 ms, p95 222 ms sur SafeDocs) ;
- `type3.pdf` à 0,001 % de `pdftoppm` ;
- fuzzing `render`, avec une police Type 3 dont la procédure est l'entrée : 438 187 puis
  288 647 exécutions (10 min chacune, la seconde avec une procédure vide en plus), aucun crash.

Cache de rendu par document (test ignoré `tile_cost`, 284 premières pages SafeDocs rendues
en 12 803 tuiles de 256 px à l'échelle 2, comme le visualiseur, release, WSL) :
- sans cache entre les tuiles (comportement précédent) : médiane 291 ms par page, p95 7,9 s,
  total 588 s ;
- avec un `RenderCache` par document : médiane 89 ms, p95 336 ms, total 51 s, soit 11 fois moins ;
- résultats identiques au pixel près avec ou sans cache (test `cache`, fixtures de texte,
  d'images, de formulaires et de Type 3, pages et zones dans plusieurs ordres).

Défilement continu, vérifié à la main dans l'application sous Windows, sur un long PDF
(2026-11-11) :
- défilement à la molette et au clavier, suivi de la page dans la barre d'état, boutons page
  par page, redessin correct après un défilement rapide aller-retour : les quatre points OK ;
- mémoire relevée dans le Gestionnaire des tâches (mémoire privée), au repos puis pendant
  le défilement :
  - WebView2 (ensemble) : 74,8 Mo, puis 89,2 Mo (moteur de rendu CastlePDF : 14,8 puis
    27,3 Mo) ;
  - shell : 5,9 Mo, puis 47,1 Mo (cache de tuiles, 128 Mio au plus) ;
  - worker : 1,4 Mo, puis 36,8 Mo (cache de rendu, 64 Mio au plus par document) ;
  - total : environ 82 Mo au repos, 173 Mo en défilement.
- Une mesure à la main sur une machine : elle borne l'ordre de grandeur, pas une
  référence de régression (voir la limite ouverte de la feuille de route).

Après les filtres ASCIIHex, ASCII85, LZW et RunLength (même protocole) :
- premières pages rendues : 1 911 → 1 920 (SafeDocs) et 2 204 → 2 229 (UNSAFE). Les échecs
  `unsupported/StreamFilter` (contenus de page dans ces filtres) passent de 9 et 25 à 0 ;
- premières pages avec une image non dessinée (`Image`) : 113 et 140 (UNSAFE : 152 → 140 ;
  SafeDocs reste à 113, car les pages désormais rendues ont leurs propres images) ;
- aucun crash ni timeout ;
- oracle qpdf : 15 fixtures, 51 flux décodés identiques, dont ceux de `filters.pdf` ;
  `filters.pdf` identique à `pdftoppm` (0,000 %) ;
- fuzzing `flate`, étendu aux nouveaux filtres : 2 401 353 exécutions en 10 min, aucun crash.

Temps par tuile côté WebView (panneau « Mesures » de l'application, release, Windows,
2026-11-11), sur un PDF de 11 pages et 804 Ko avec formulaires, images et texte :
- première page dessinée (20 tuiles, à froid) : réception médiane 7,1 ms et p95 92,7 ms ;
  dessin médian 0,1 ms et p95 0,2 ms. Le p95 vient des premières tuiles, qui lisent et
  décodent polices et images avant que le cache ne serve les suivantes ;
- après plusieurs défilements, dont des défilements rapides (1 368 tuiles) : réception
  médiane 5,0 ms et p95 9,9 ms ; dessin médian 0,0 ms et p95 0,1 ms ;
- la réception comprend le rendu par le worker, l'attente dans la file du shell (4 requêtes à
  la fois) et le transport IPC des 256 Kio d'une tuile. Le dessin (`ImageData` puis
  `putImageData`) est négligeable. Le transport d'ADR-0003 (`ipc::Response` vers
  `ArrayBuffer`) n'est pas un goulet d'étranglement sous Windows.

Démarrage du worker (tests ignorés `measure_worker_start`, release, Windows, 20 démarrages
de suite, 2026-11-11) :
- vu du shell (programme GUI, comme l'application) : médiane 435 ms avant correctif. Windows
  donnait une console au worker, programme console lancé par un programme sans console. Avec
  `CREATE_NO_WINDOW` : médiane 29 ms, p95 37 ms, premier démarrage 116 ms ;
- vu d'un programme console (`pdf-worker`, étape par étape) : lancement 9 ms, réponse à la
  poignée de main 12 ms en médiane ;
- les 491 ms mesurées à la tranche 4 étaient donc surtout cette console.

La cible de fuzzing `font` a trouvé une panique de `read-fonts` 0.41.0, corrigée
par la mise à jour (amendement d'ADR-0005). Après correction : 134 791 exécutions en 600 s,
aucun crash. Comparaison à `pdftoppm` 22.02 : 0,010 % de pixels hors tolérance pour
`truetype.pdf` à 216 dpi, et 0,28 % à 72 dpi, dû au calage des fûts sur la grille de pixels
par FreeType. Le texte est donc comparé à 216 dpi, avec la tolérance documentée dans
`pdftoppm_oracle.rs`.

Le fuzzing de la cible `repair` (600 s, environ 2,5 millions d'exécutions, WSL) n'a trouvé
aucun crash. Sur les fixtures abîmées, qpdf 10.6 reconstruit la même liste d'objets quand
il trouve un trailer. Sans trailer, il refuse le fichier là où CastlePDF prend le dernier
catalogue. Il indexe aussi un en-tête `10 0 obj` placé dans les données d'un flux à
`/Length` indirecte, que CastlePDF saute volontairement (`header-in-stream.pdf`).

Mémoire mesurée par le workflow `memory` (application release, runners GitHub partagés,
2026-11-11, run 43078129487), au repos puis avec `forms.pdf` ouvert, médiane de plusieurs
relevés, en Mio :

| OS (runner) | Shell | Worker | WebView | Total retenu, au repos → avec document |
| --- | --- | --- | --- | --- |
| Windows x64 (working set) | 28,1 → 32,2 | 3,5 → 4,8 | 275,3 → 308,5 | mémoire privée : 102,0 → 115,9 |
| macOS ARM64, M1 virtuel (RSS) | 93,6 → 97,6 | 1,7 → 3,8 | 74,9 → 113,8 | RSS : 170,3 → 215,3 |
| Linux x64 (RSS) | 184,9 → 193,6 | 3,1 → 5,1 | 359,8 → 443,2 | PSS : 339,4 → 432,8 |

- Le worker reste sous 6 Mio partout. Sous Linux, WebKitGTK et le shell dominent ; le runner
  n'a pas de GPU et dessine en logiciel sous Xvfb, ce qui peut gonfler ces chiffres. À
  comparer avec une machine Linux réelle.
- Le working set de Windows compte des pages partagées : la mémoire privée est la mesure à
  comparer au relevé à la main (environ 82 Mo au repos).
- Runners partagés : ordres de grandeur, pas des seuils de régression.

Affichage vérifié sur captures du workflow `screenshot` (2026-11-11, run 43076099447) :
`truetype.pdf` et `forms.pdf` rendus correctement sous Linux (Xvfb) et macOS ARM64 (texte,
espacement, échelle horizontale, formes pivotées, couleurs). Sous Windows, la console du
worker recouvrait la fenêtre ; elle a disparu après `CREATE_NO_WINDOW` (#53, run 43085372188
sur `main`). Sous macOS, `forms.pdf` ouvert par `open -a` depuis le `.app`, comme par le
Finder, est affiché (#55) ; le premier essai avait révélé une panique au lancement, corrigée.

Sources de l'Unicode du texte (ADR-0006, tranche 3.1 ; `survey`, release, Linux WSL,
2026-11-12, même protocole). Une première page compte une fois par source ; une page a
souvent plusieurs polices, donc plusieurs sources :

| Source | SafeDocs (1 940 ouverts) | UNSAFE (2 282 ouverts) |
| --- | --- | --- |
| `/ToUnicode` | 1 230 | 1 194 |
| Noms de glyphes (`/Encoding` ou encodage standard) | 1 105 | 1 404 |
| Encodage intégré (police symbolique Type 1 ou CFF, Symbol, ZapfDingbats) | 23 | 18 |
| TrueType symbolique sans `/Encoding` (aucun nom de glyphe) | 2 | 3 |
| Type 3 sans `/ToUnicode` | 7 | 4 |
| CID `Adobe-Japan1` / `GB1` / `CNS1` / `Korea1` sans `/ToUnicode` | 21 / 3 / 7 / 2 | 15 / 0 / 1 / 1 |
| CID `Identity` ou autre collection sans `/ToUnicode` | 3 | 26 |
| Dictionnaire invalide | 0 | 23 |

Encodages des polices Type 0 : `Identity-H` 881 et 937 premières pages, CMap prédéfinie
nommée 22 et 6, `Identity-V` 4 et 2, CMap incluse 5 et 0.

- `/ToUnicode` et les noms de glyphes couvrent l'essentiel : la tranche 3.3 les traite.
- Les CMap prédéfinies d'Adobe serviraient à au plus 33 premières pages SafeDocs (1,7 % des
  ouvertes) et 17 UNSAFE (0,7 %) pour l'Unicode, et à 22 et 6 pages pour le rendu des
  encodages nommés, aujourd'hui non dessinés (tranche 3.4).
- Sans `/ToUnicode`, une police CID `Identity` n'a pas de correspondance standard : son
  texte sera signalé inconnu (3 et 26 pages), sauf repli signalé sur la table `cmap` de
  la police incluse.

Lecteur de CMap (tranche 3.2, 2026-11-12) : 21 tests (exemple de la norme 9.10.3, plages de
codes de 9.7.6.2, chevauchements, entrées et blocs invalides, bornes de CID, de destination et
de plages de codes, `usecmap`, budget à la limite et à la limite moins un pour la lecture et la
fusion, bloc d'un million de jetons arrêté par le budget, 65 536 recherches sous une plage large
avec 50 000 exceptions, annulation). Fuzzing `cmap` : 2 269 700 exécutions, puis 2 521 311
après la revue (10 min chacune, WSL), aucun crash. La revue a relevé une mémoire de blocs non
imputée et une recherche linéaire par code (quadratique sur une page) : corrigées avant fusion.

Extraction du texte (tranche 3.3, `survey`, release, WSL, 2026-11-12, même protocole) :

| Corpus | Glyphes des premières pages | Inconnus | Code `/ToUnicode` d'une autre longueur | Octets en police non lue | Échecs |
| --- | --- | --- | --- | --- | --- |
| SafeDocs | 2 980 514 sur 1 675 pages | 17 776 (0,60 %) sur 58 pages | 0 | 144 562 sur 52 pages | 18 |
| UNSAFE | 3 476 082 sur 2 014 pages | 26 482 (0,76 %) sur 96 pages | 541 (0,02 %) | 52 027 sur 33 pages | 51 |

- Les échecs ont les catégories des échecs de rendu (contenu ou flux mal formés) ; aucun crash
  ni timeout, taux d'ouverture et de rendu inchangés. Temps médian par fichier : 28 ms
  (SafeDocs, contre 18 ms) et 16 ms (UNSAFE), la sonde extrayant aussi le texte.
- Oracle `pdftotext` (Poppler 22.02, `-raw`) : caractères identiques, hors espaces, sur les
  9 pages de fixtures dont tout le texte est connu ; écarts voulus documentés dans
  `crates/pdf-render/tests/pdftotext_oracle.rs`.
- Fuzzing, `page_text` ajouté aux cibles `render` (la `/ToUnicode` d'une police Type 3 est
  l'entrée) et `font` : 125 694 et 75 806 exécutions (10 min chacune), aucun crash.
- La revue a demandé : compter les octets en police non lue, `/ToUnicode` `null` ou nommée
  tenue pour absente, contrôles bidirectionnels remplacés dans la sortie de la CLI, une page
  en échec n'arrêtant plus les suivantes, polices et CMap partagées entre les pages
  (`page_text_cached`). Fait avant fusion.

Encodages Type 0 au-delà d'`Identity-H` (tranche 3.4a, même protocole, 2026-11-12) :
- premières pages dont du texte est en police non lue : 52 → 45 (SafeDocs, 144 562 → 135 889
  octets) et 33 → 31 (UNSAFE, 52 027 → 49 767 octets) ; les CMap prédéfinies CJK restent pour
  3.7b ;
- glyphes inconnus : 0,60 % → 0,69 % (SafeDocs) et 0,76 % → 0,78 % (UNSAFE) : les polices
  désormais lues sans `/ToUnicode` sont comptées, leur texte signalé inconnu au lieu d'être
  absent ;
- premières pages rendues où du texte n'est pas dessiné (`Text`) : 54 (SafeDocs) et 39 (UNSAFE),
  référence pour les tranches suivantes ; taux, crashs et timeouts inchangés ;
- `cmaps.pdf` identique à `pdftoppm` à 216 dpi (page 1, texte vertical compris) et à
  `pdftotext` (deux pages) ; fuzzing `font` avec ces encodages : 101 792 exécutions en 10 min,
  aucun crash.

## Contrôles manuels par OS

Ce que la CI ne voit pas : un geste dans l'application release
(`npm --prefix apps/desktop run tauri -- build --no-bundle -- --locked`). Le choix du fichier
déposé ou ouvert est testé en Rust (`apps/desktop/src-tauri/src/opening.rs`) ; le geste lui-même
ne l'est que par cette procédure. Consigner la date, l'OS et le résultat de chaque point.

Glisser-déposer :
1. Déposer un PDF : son nom s'affiche et la première page est rendue.
2. Déposer un second PDF alors qu'un document est ouvert : il le remplace.
3. Déposer deux PDF à la fois : rien ne change, le document courant reste affiché.
4. Déposer un fichier qui n'est pas un PDF (un `.txt`) : un échec explicite s'affiche, et un
   PDF déposé ensuite s'ouvre normalement.
5. Déposer un PDF dont le chemin contient des espaces et des lettres accentuées : il s'ouvre.

Finder (macOS, application empaquetée `.app`) :
6. « Ouvrir avec » CastlePDF sur un PDF, application fermée puis déjà ouverte : le document
   s'ouvre. CastlePDF ne devient pas le lecteur PDF par défaut.

Défilement : voir le relevé du 2026-11-11 ci-dessus (quatre points).

| OS | Date | Points 1 à 5 | Point 6 |
| --- | --- | --- | --- |
| Windows | 2026-11-11 | OK : 1 et 2 ; 3 aussi au démarrage, sans document ouvert ; 4 « fichier PDF invalide ou endommagé », puis un PDF s'ouvre ; 5 avec espaces, accents et `$` | sans objet |
| macOS | à faire (pas de machine pour l'instant) ; la CI ouvre un PDF par `open -a` (workflow `screenshot`) | | |
| Linux | à faire (pas de machine pour l'instant) | | sans objet |

## Principes de tests

Vérifier les comportements, pas les détails de l'implémentation. Un bug reproductible
devient un test qui échoue avant la correction, quand techniquement possible.
Pour les règles documentaires, citer la clause PDF applicable dans le test ou sa notice.
Les propriétés de round-trip portent sur la sémantique du sous-ensemble supporté ;
l'ordre des objets, les offsets et la compression peuvent changer sans perte visible.

Une comparaison qpdf ne vérifie pas le rendu ; une image de référence ne prouve pas
la présence du texte ni son accessibilité. Croiser structure, contenu, rendu et
manipulations. Gérer les variations de polices/antialiasing avec un environnement
fixé et des tolérances déclarées, sans accepter aveuglément de nouveaux goldens.

Tests de sauvegarde : disque plein simulé, permission refusée, interruption, fichier
verrouillé et remplacement selon OS. Vérifier que l'original reste lisible et inchangé
après un échec. Tester undo/redo et l'invalidation des caches après édition.

Les fuzzers et oracles tournent sans réseau et avec temps/mémoire bornés. Au crash :
isoler, minimiser, consigner seed/version/limites, corriger puis conserver un test sûr.
Miri couvre les composants compatibles ; les sanitizers et les codecs natifs ont
des environnements dédiés. Une campagne finie ne prouve jamais l'absence de défaut.

## Sortie d'une tâche

Résultat demandé couvert, diff relu, tests pertinents réussis, limites et erreurs
documentées, aucune donnée privée, contrats mis à jour. Pour une modification critique,
joindre une revue distincte et les mesures avant/après. Mentionner explicitement les
contrôles indisponibles ou en échec, sans les transformer en succès.

Ne pas imposer un pourcentage de couverture universel : suivre les branches de parsing,
erreurs et limites réellement exercées. La couverture aide à trouver un manque ; elle
ne remplace ni assertions utiles ni corpus représentatif.
