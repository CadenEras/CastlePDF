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

Tranche 3.11, limites reportées de P2 (2026-11-15, même protocole, release, WSL ; `tile_cost`
donne aussi la page rendue d'un seul tenant et une passe d'interprétation seule, zone de
1 pixel en coin, médiane de 9 passes avec le cache chaud) :
- 193 premières pages SafeDocs, 8 528 tuiles : page entière médiane 23 ms, p95 184 ms, max
  338 ms, total 9,3 s ; en tuiles avec cache médiane 89 ms, p95 362 ms, max 4,4 s, total 31,6 s.
  Les tuiles coûtent 3,4 fois la page entière, soit 2,6 ms de plus par tuile en moyenne ;
- les huit pages les plus lentes en tuiles (35 à 513 tuiles) font 2 à 22 fois la page entière ;
  leur passe d'interprétation coûte 4,6 à 13,6 ms, ce que chaque tuile paie avant de dessiner
  ses cellules. Une tuile visible reste donc sous 20 ms sur ces pages ;
- les images en ligne, redécodées à chaque passe, sont comprises dans ce coût et ne le font
  pas ressortir ;
- `survey` : 2 premières pages SafeDocs et 2 UNSAFE refusées par `limit/DecodedBytes` (4 le
  2026-11-10), chacune avec 2 à 7 images de 8,7 à 32,9 Mpx. Le rasteriseur garde chaque
  image dessinée jusqu'à la fin de la page (`Canvas::finish`) : le budget cumulatif approche
  donc le pic réel, à la surestimation près des échantillons libérés après conversion en RGBA ;
- décision : aucune des trois optimisations n'est justifiée par la mesure (voir la feuille
  de route) ; elles restent reportées avec ces chiffres.

Tranche 4.5a, « Enregistrer sous » transactionnel (2026-11-17) : `tests/save.rs` sous Windows et
Linux (WSL) : écriture relue, autre fichier remplacé, source refusée (même chemin, autre écriture,
lien symbolique sous Unix), contenu illisible, tronqué ou d’un autre nombre de pages refusé,
dossier absent, destination dossier, destination en lecture seule (Windows) ou dossier en
lecture seule (Unix) : la destination reste telle quelle et aucun temporaire ne reste. Non
simulés : disque plein et arrêt de la machine pendant le renommage.

Tranche 4.4, fusion et découpe (2026-11-17, même protocole ; chaque document fusionné après le
précédent ouvert) :
- 1 939 et 2 281 fusions écrites, relues strictement avec toutes les pages ; première page du
  second document identique au pixel près pour les 1 919 et 2 228 qui se rendaient, aucune rendue
  autrement, aucune qui n’échoue qu’après ;
- relecture : `split` laissait les parties déjà écrites quand une suivante échouait (retirées
  désormais) ; `merge` lisait tous les fichiers avant de constater qu’ils dépassaient ensemble la
  limite (taille cumulée vérifiée avant lecture) ; chemins d’entrée affichés dans les erreurs
  (position désormais) ; `/Info` et réglages `D` des autres sources perdus sans être listés
  (listés) ; plan non borné (`MAX_PLAN_PAGES`) ;
- fuzzing `write` (fusion d’un document avec lui-même ajoutée) : 531 380 exécutions, aucun
  constat ;
- tests : `rewrite.rs` (fixtures fusionnées rendues comme leur source, même fichier deux fois,
  calques fusionnés et masqués selon `/OFF` et `/BaseState`, élagage par source, pertes des
  autres sources listées, version maximale, plans invalides), `pages.rs` (CLI `merge` et
  `split`, sortie existante, entrée nommée par sa position). Non testé : l’échec d’une partie
  de `split` après une autre réussie (aucun moyen simple de le provoquer depuis la CLI).

Tranche 4.3, plan de pages (2026-11-16, même protocole ; chaque document de deux pages ou plus
réécrit sans sa dernière page, ressources élaguées) :
- première page : identique au pixel près (et mêmes manques) pour les 1 334 et 1 437 qui se
  rendaient ; aucune rendue autrement, aucune qui n’échoue qu’après ; relues strictement avec
  une page de moins ;
- taille rapportée à l’entrée : médiane 0,93, p95 1,00 ;
- ressources gardées entières faute de certitude, sur au moins une page : 37 et 35 fichiers
  (listées par `castle-pdf pages`) ;
- une réécriture échoue en limite de décodage (fichier de 188 Mo, 152 pages) : limite ouverte ;
  avant le cache de résolution partagé entre pages, trois échouaient (relecture) ;
- relecture : une image dont `/ColorSpace` nomme une ressource perdait cet espace, et une image
  en ligne avec `/L` pouvait cacher la suite du contenu ; corrigés, avec un test qui échoue
  sans la correction ;
- tests : `rewrite.rs` (une image dessinée seulement par une page retirée n’est plus écrite,
  rendu identique, `Default*` gardés, cas gardés entiers : forme, motif, police Type 3,
  masque, apparence, contenu illisible, image sans fin, contenu en tableau avec `null` ; page
  donnée deux fois), `prune` (lecture des images en ligne comme au rendu), `pages.rs`
  (CLI : ordre, suppression, rotation, plans refusés).

Tranches 4.1 et 4.2, écriture (ADR-0008, 2026-11-16, release, WSL ; test ignoré `rewrite_corpus`) :
- chaque document des deux corpus réécrit inchangé : 1 940 sur 1 940 (SafeDocs) et 2 282 sur
  2 282 (UNSAFE) réécrits, relus par le lecteur strict avec le même nombre de pages ;
- première page : identique au pixel près (et mêmes manques signalés) pour les 1 920 et 2 229
  qui se rendaient ; aucune rendue autrement, aucune qui n'échoue qu'après ; 9 et 26 qui
  échouaient se rendent après la réécriture (fichier sain : flux terminés, table juste) ;
- taille de sortie rapportée à l'entrée : médiane 1,00, p95 1,02, maximum 1,37 et 1,85 ;
  temps de réécriture médian sous la milliseconde, maximum 0,7 s ;
- non conservé, par fichier : surtout `StructTreeRoot` et `MarkInfo` (documents balisés),
  `OpenAction`, `Outlines`, `Names`, `PageLabels`, `AcroForm` ; 18 et 15 documents signés ;
  `OCProperties`, d'abord perdu (78 et 125 fichiers : des calques masqués seraient apparus
  ailleurs), est désormais gardé ;
- oracles : `qpdf --check` sans erreur sur un fichier écrit et sur les 22 fixtures réécrites
  (qpdf 10.6 en local, 11 en CI, qui a relevé une page sans `/Resources`, désormais toujours
  écrit) ; propriété aller-retour des objets (`proptest`) ; réécriture déterministe, et
  réécrire un fichier réécrit redonne les mêmes octets ;
- tests : `rewrite.rs` (fixtures rendues à l'identique, page hors plan absente du fichier et
  lien vers elle nul, rotations et boîtes héritées, catalogue gardé et perdu, plans invalides,
  limite de taille, `OCProperties`, rien de ce qui est laissé de côté ne revient par un autre
  objet — catalogue, réponse `/IRT`, page orpheline d'une mise à jour incrémentale —, deux
  tests qui échouaient avant la relecture) ;
- limite connue : les ressources partagées entre pages (un `/XObject` hérité de l'arbre)
  sont copiées entières ; l'élagage aux noms utilisés vient avec la suppression de pages
  (tranche 4.3), avant qu'un plan qui retire des pages soit proposé.

Tranche 3.17, motifs de dégradé (`survey`, 2026-11-16, même protocole) :
- premières pages rendues inchangées (1 920 et 2 229), aucun crash ni timeout ; `Pattern` 39 → 24
  et 22 → 18, les autres décomptes inchangés ;
- douze premières pages SafeDocs qui ne signalent plus `Pattern`, comparées à `pdftoppm` à 72 dpi :
  0,5 à 4 % de pixels écartés sur la moitié d'entre elles ; vues à l'œil sur trois, écarts venant
  des bords des glyphes, d'un fond très clair et, sur les plus grands écarts, des masques adoucis
  signalés sur ces pages ;
- fuzzing `render` 10 min avec un motif de dégradé radial (matrice, fond) dans les ressources :
  205 177 exécutions, aucun crash ni délai ;
- tests : `shadings.rs` (remplissage par un motif, `/Matrix` et `/Background`, espace du
  formulaire parent, `cm` sans effet sur le motif, pair-impair, motif choisi 5 000 fois,
  motif pavé et contour signalés) ;
- relecture indépendante : avec un alpha de remplissage inférieur à 1, le fond transparaissait
  sous le dégradé (couleurs mêlées) : fond et dégradé sont désormais composés ensemble puis à
  l'alpha (test vérifié en échec avec l'ancien comportement) ; le chemin rasterisé pour le fond
  et la découpe est compté ; `SCN` et l'extraction de texte ne lisent plus de motif, et un motif
  choisi de nouveau ne prend pas d'entrée de plus.

Tranche 3.16, dégradés axiaux et radiaux (`survey`, 2026-11-16, même protocole) :
- recensement préalable (test ignoré `color_census`, `shadings_of_pages_reporting_shading`) des
  69 et 46 premières pages qui signalaient `Shading` : surtout axiaux (type 2), puis radiaux
  (type 3), presque toujours étendus aux deux bouts, avec des fonctions assemblées, en ICC, CMJN
  ou DeviceN ; une dizaine de maillés (type 7), aucun de type 1 ;
- premières pages rendues inchangées (1 920 et 2 229), aucun crash ni timeout ; `Shading` 69 → 9
  et 46 → 3, les autres décomptes inchangés ;
- deux régressions trouvées par cette mesure et corrigées avant la PR : chaque `sh` était calculé
  sur toute la surface du canevas, si bien que 8 et 2 premières pages riches en petits dégradés
  dépassaient `max_path_work` (désormais calculé sur le rectangle de la découpe) ; une page
  peignant 63 dégradés 1 403 fois dépassait `max_operations` (désormais lus une fois par rendu,
  table de 256 teintes). Chacune a son test, vérifié en échec sans la correction ;
- douze premières pages SafeDocs qui ne signalent plus `Shading`, comparées à `pdftoppm` à
  72 dpi : un fond radial à 0,9 % de pixels écartés (bords des glyphes), les autres écarts venant
  du texte et des limites déjà connues ; vues à l'œil sur trois pages ;
- fuzzing `render` 10 min, avec un dégradé axial et un radial dans les ressources : 220 111
  exécutions, aucun crash ni délai ;
- tests : unitaires de `shading.rs` (paramètres axiaux et radiaux, extension, cône), `shadings.rs`
  (axe, extension, rayons, découpe et `/BBox`, DeviceN et ICC, types signalés, tuiles identiques
  à la page, grande surface, coût d'une découpe, dégradé peint 500 fois) ;
- relecture indépendante : un texte en dégradé (`7 Tr`, puis `sh`) aurait peint un rectangle plein,
  la découpe par le texte n'étant pas appliquée : signalé désormais, sans peindre ; un même dégradé
  peint sous deux portées aux `Default*` différents reprenait les couleurs de la première : il
  n'est plus gardé là où la portée définit un `Default*`. Aussi : racines du cas radial calculées
  sans annulation, seuil du cas linéaire relatif, table de couleurs imputée au budget ; chaque
  correction a son test, vérifié en échec sans elle. Limites connues : au-delà de 1 Mpx de
  surface, l'image est réduite, si bien qu'une tuile et la page entière peuvent différer d'un
  niveau sur un dégradé lissé ; une fonction à ruptures nettes est échantillonnée sur 256 teintes.

Tranche 3.15, Separation, DeviceN et Lab (`survey`, 2026-11-16, même protocole) :
- recensement préalable (test ignoré `color_census`) des 91 et 75 premières pages qui signalaient
  encore `ColorSpace` : surtout Separation (fonctions de type 2, puis 0 et 4) et DeviceN
  (type 4), sur CMJN, ICC, gris et parfois Lab ;
- premières pages rendues inchangées (1 920 et 2 229), aucun crash ni timeout ; temps par fichier
  médian 31 et 19 ms ;
- `ColorSpace` 91 → 16 et 75 → 12 ; `Image` 64 → 65 sur UNSAFE ;
- douze premières pages SafeDocs qui ne signalent plus `ColorSpace`, comparées à `pdftoppm` à
  72 dpi : formes et positions justes, couleurs plus saturées que chez Poppler sur les tons
  directs remplacés en CMJN (formule d'ISO 32000-2 sans profil, contre la gestion de couleur de
  Poppler), écart déjà connu pour DeviceCMYK ; vues à l'œil sur trois pages ;
- fuzzing `tint` (programme PostScript et table d'échantillons arbitraires) 10 min : 966 755
  exécutions, aucun crash ni délai ;
- tests : unitaires de `function.rs` (calculatrice, erreurs, bornes, interpolation, assemblage),
  `colors.rs` (Separation et DeviceN sur CMJN, gris, RVB, ICC et Lab avec les types 0, 2, 3 et 4,
  couleur initiale, `/None`, fonction qui échoue, table au-delà de la borne, conversions Lab) ;
- relecture indépendante : un `/Domain` ou un `/Range` inversé (`[1 0]`) faisait **paniquer**
  le bornage des entrées (`f64::clamp`), donc le worker : de tels intervalles refusent désormais
  la fonction (test de régression, reproduit avant correction ; la cible `tint` fait varier ces
  tableaux, qu'elle fixait). Aussi corrigés : chaque évaluation compte ses étapes dans
  `max_operations` (100 `sc` d'un programme de 41 opérateurs dépassent 2 000), un tableau
  invalide refuse la fonction au lieu de prendre la valeur par défaut, des bornes d'assemblage
  hors du domaine sont refusées, la calculatrice refuse un booléen après un bloc en attente et
  `if` sur un nombre, `atan 0 0` n'a pas de valeur, un décalage de 64 bits ou plus donne 0, la
  borne des espaces par rendu est vérifiée avant leur lecture ; le test de `/None` peint sur
  fond blanc (il passait aussi si `/None` peignait), celui de la borne des tables fournit les
  données (2^22 échantillons lus, un de plus refusé).

Tranche 3.14, espaces de couleur des tracés et du texte (`survey`, 2026-11-16, même protocole) :
- premières pages rendues inchangées (1 920 et 2 229), aucun crash ni timeout ; temps par
  fichier médian 29 → 34 ms et 16 → 20 ms, p95 236 → 263 ms et 128 → 141 ms : ces pages
  peignent désormais ce qu'elles sautaient ;
- `ColorSpace` 392 → 91 (SafeDocs) et 417 → 75 (UNSAFE) ; `Image` 63 → 64 sur UNSAFE
  (probablement un masque de pochoir dont la couleur de remplissage est désormais lue, puis
  dont le décodage échoue) ;
- douze premières pages SafeDocs qui signalaient `ColorSpace` et ne le font plus, rendues à
  72 dpi et comparées à `pdftoppm` : 3 à 36 % de pixels écartés de plus de 24 sur un canal,
  écarts vus à l'œil sur trois d'entre elles : bords des glyphes (rasterisation de Poppler) et
  dégradés non dessinés (`Shading`), aucune couleur fausse ; Poppler applique les profils ICC,
  nous non (documenté dans `docs/SUPPORT.md`) ;
- fuzzing `render` 10 min avec les nouveaux espaces : 142 312 exécutions, aucun crash ni délai
  (un premier essai, lancé pendant les tests de l'espace de travail, a signalé un délai non
  reproductible : 0,13 s sur la même entrée, sur `main` comme sur la branche) ;
- tests : `colors.rs` (ICCBased, CalGray, CalRGB en remplissage et contour, Indexed et ses
  bornes, `Default*` de même nature ou non, espaces encore signalés ; 500 rendus d'une page
  et d'un formulaire en Indexed avec un même cache, et un rendu avec un cache vide) ;
- relecture indépendante : la première version gardait les palettes dans le cache du document,
  si bien qu'après environ 400 tuiles d'une page à dix palettes l'Indexed n'était plus dessiné,
  ni jamais avec un cache vide. Les palettes appartiennent désormais au rendu ; le test des
  500 rendus échoue avec la première version.

Tranche 3.13, signaler ce qui manquait en silence (`survey`, 2026-11-15, même protocole) :
- premières pages rendues inchangées (1 920 et 2 229), aucun crash ni timeout, temps par
  fichier inchangé (médiane 29 et 16 ms) ;
- nouvelles pages signalées : `Annotation` 42 (SafeDocs) et 40 (UNSAFE) ; `ColorSpace`
  380 → 392 et 409 → 417 (JPEG en Indexed, Separation, DeviceN ou Lab, `Default*` d'une
  image) ; `OptionalContent` 54 → 56 et 53 inchangé (image d'un calque) ;
- tests : `annotations.rs` (annotations montrées ou non, cachées, pop-up, liens, entrées
  abîmées, tuiles avec cache), `images.rs` (JPEG selon l'espace déclaré, `Default*` et `/OC`
  sur une image, à chaque rendu avec ou sans cache) ; chacun vérifié en échec sans le
  changement du moteur.

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

Police « texte seulement » (tranche 3.4b, même protocole, 2026-11-12) :
- premières pages dont du texte est en police non lue : 45 → 22 (SafeDocs, 135 889 → 90 691
  octets) et 31 → 15 (UNSAFE, 49 767 → 32 582 octets) ; il ne reste que des CMap prédéfinies
  autres qu'`Identity` (3.7b) et des dictionnaires abîmés ;
- glyphes extraits : 2 985 364 → 3 029 322 (SafeDocs) et 3 477 212 → 3 493 756 (UNSAFE) ; part
  des inconnus 0,69 % → 1,01 % et 0,78 % → 0,83 % : ces polices sans `/ToUnicode` ni noms
  utilisables sont désormais signalées au lieu d'être absentes ;
- rendu inchangé : 54 et 39 premières pages où du texte n'est pas dessiné ; taux, crashs et
  timeouts inchangés ; fuzzing `font` (le programme arbitraire, souvent illisible, passe par la
  police texte seulement) : 98 846 exécutions en 10 min, aucun crash.

Mots et lignes (tranche 3.5a, 2026-11-12, release, WSL) : test ignoré `layout_corpus`
(`CASTLE_CORPUS`, `CASTLE_PDFTOTEXT`), mots de la première page comparés en sac (ordre à part)
à ceux de `pdftotext` 22.02 (mode par défaut), F1 = 2 |communs| / (|nôtres| + |siens|) :

| Corpus | Pages comparées | F1 moyen | p10 | Médiane | ≥ 0,99 | ≥ 0,9 | Pages avec `/ActualText` |
| --- | --- | --- | --- | --- | --- | --- | --- |
| SafeDocs | 1 675 | 0,953 | 0,915 | 1,000 | 70,0 % | 90,8 % | 93 |
| UNSAFE | 2 003 | 0,961 | 0,938 | 1,000 | 66,8 % | 93,6 % | 70 |

- Texte d'un seul côté : 5 et 6 pages chez nous seulement, 3 et 2 chez Poppler seulement.
- Seuil entre mots (fois la taille) : 0,1 donne 0,954 et 0,961, 0,15 donne 0,953 et 0,961,
  0,2 donne 0,952 et 0,958, 0,25 donne 0,944 et 0,954. Retenu : 0,15, à peine moins bon que 0,1
  mais qui ne coupe pas un texte à espacement de lettres modéré (2 pt sur 18 pt dans
  `truetype.pdf`, soit 0,11).
- Les écarts restants mêlent glyphes inconnus (signalés chez nous, imprimés autrement par
  Poppler), coupures de mots différentes, texte dupliqué pour un faux gras (Poppler fusionne
  les glyphes superposés) et `/ActualText` (voir `pdftotext_oracle.rs`) ; non ventilés, faute
  de lire le contenu du corpus.
- Fixtures : mots identiques à `pdftotext -raw` sur les pages comparées par l'oracle.
- Fuzzing `render` (`text_lines` ajouté, la propriété : au plus un mot par glyphe ou plage) :
  101 646 exécutions en 10 min, aucun crash.

Ordre de lecture (tranche 3.5b, même test `layout_corpus`, 2026-11-12) : ordre des mots de la
première page comparé à celui de `pdftotext` (mode par défaut, son ordre de lecture),
2 LCS / (|nôtres| + |siens|), pages d'au plus 5 000 mots :

| Corpus | Ordre du contenu (`text_lines`) | Ordre de lecture (`reading_lines`) | Plafond (F1 des mots) |
| --- | --- | --- | --- |
| SafeDocs | moyenne 0,840, médiane 0,944, ≥ 0,9 : 58,6 % | moyenne 0,887, médiane 0,979, ≥ 0,9 : 70,7 % | 0,953 |
| UNSAFE | moyenne 0,873, médiane 0,970, ≥ 0,9 : 67,1 % | moyenne 0,907, médiane 0,984, ≥ 0,9 : 77,5 % | 0,961 |

- Seuil de recollage des morceaux de ligne (fois la taille) : 0,5 et 1,0 donnent 0,887 et
  0,907 ; 1,5, 2 et 3 dégradent (0,884 à 0,879 sur SafeDocs). Retenu : 1,0, qui garde entière
  une ligne justifiée aux espaces larges.
- Poppler n'est pas une vérité de l'ordre : sur `columns.pdf`, il lit la colonne droite après le
  pied de page. L'écart restant au plafond mêle de tels choix et des pages que la découpe XY
  lit mal (tableaux, encadrés) ; non ventilé, faute de lire le contenu du corpus.
- Fuzzing `render` (propriété : l'ordre de lecture garde chaque mot) : 95 690 exécutions en
  10 min, aucun crash.

Couche de texte du worker (tranche 3.6a, 2026-11-12) : aller-retour des messages `PageText`,
nombres de lignes et de mots refusés avant allocation, placement sur `columns.pdf` et sur une
page tournée de 90°, troncature d'environ 120 000 mots dans la borne (réponse encodée entre
960 Kio − 64 octets et 960 Kio), commande du shell de bout en bout avec le processus worker. Après la revue : couche d'une vue
périmée écartée (test déterministe, file occupée par une tâche synthétique), texte manquant
signalé (`incomplete`), mot de 70 000 octets coupé et signalé, page vide, document ou page
inconnus par les pipes, `page_transform` pour les quatre rotations, une crop box décalée,
`/UserUnit` et une page trop grande pour un rendu à l'échelle 1.
Fuzzing `protocol` : 144 150 598 exécutions en 10 min (WSL), aucun crash.

Recherche, moteur et worker (tranche 3.7, 2026-11-14) :
- comparaison repliée (casse, accents latins, ligatures, apostrophes, espaces, trait d'union
  conditionnel), occurrences entre deux lignes, sans chevauchement, borne d'occurrences et de
  requête testées à la limite et au-delà (`pdf_render::search`) ;
- `castle-pdf search` sur `columns.pdf` et `text.pdf` ; `SearchPage` par les pipes, chaque
  occurrence vérifiée contre la couche de texte de la même page ; commande du shell de bout
  en bout avec le processus worker ;
- temps de `castle-pdf search` (release, WSL, médiane de 3 exécutions, ouverture exclue) sur
  les 8 documents les plus longs de l'échantillon SafeDocs (449 à 1 844 pages) : de 192 ms
  (449 pages) à 1 214 ms (910 pages), soit 0,4 à 2,1 ms par page ; une requête absente
  coûte autant qu'une requête fréquente (`the`) : le temps est celui de l'extraction du texte ;
- après la revue : texte replié borné (`MAX_SEARCH_CHARS`, testé à la borne), recherche dans
  la couche de texte même (couche tronquée : 1 000 occurrences puis `truncated`, chacune sur un
  mot de la couche ; requête absente `truncated` aussi), une ligature ne donne qu'une
  occurrence par lettre repliée, minuscule avant accent (signe angström), page de recherche en
  attente remplacée par la suivante dans le shell ;
- interface (tranche 3.7, 2026-11-14) : réponses du shell validées (`parseSearchHits`,
  bornes et formes refusées), ordre de parcours depuis la page affichée, compte et rang en ordre
  du document, suivant et précédent en boucle, découpe d'un mot en morceaux surlignés en points
  de code ; de bout en bout avec le shell simulé : recherche page par page, page de l'occurrence
  amenée à l'écran et marquée, Échap, aucun résultat, recherche remplacée dont la page tardive
  est ignorée ; copie exacte à travers les marques ;
- fuzzing `search` : 1 091 533 exécutions en 2 min, puis 2 737 181 en 3 min après la revue (WSL),
  aucun crash ; `protocol` : 55 189 852 exécutions en 5 min, aucun crash.

CMap prédéfinies d'Adobe (tranche 3.7b, 2026-11-14) :
- choix mesuré : encodages nommés des premières pages (SafeDocs : `UniJIS-UCS2-H` 31 polices,
  `UniCNS-UCS2-H` 18, `UniKS-UCS2-H` 6, `UniGB-UCS2-H` 5, `90ms-RKSJ-H` 2 ; UNSAFE :
  `UniJIS-UCS2-H` 11, `UniJIS-UTF16-H` 4, `90ms-RKSJ-H` 3), avec leurs formes `-V`, et les quatre
  CMap UCS2 des collections : 16 fichiers d'Adobe, 735 886 octets compressés (2 084 051 octets) ;
- chaque fichier embarqué se lit sans entrée ignorée, valeurs de CID et d'Unicode vérifiées
  contre les sources d'Adobe ; `usecmap` d'une CMap incluse vers une CMap prédéfinie ; texte
  d'une police `Identity-H` dont la police descendante nomme Adobe-Japan1 ;
- `cjk.pdf` : huit polices, une ligne verticale, rendu sans élément ignoré et texte attendu ;
  comparaison à `pdftoppm` et `pdftotext` dans le workflow `oracle` (avec poppler-data, absent
  de la machine WSL de mesure) ;
- `survey` (même protocole) : premières pages dont du texte est en police non lue 22 → 0
  (SafeDocs) et 15 → 10 (UNSAFE, 32 582 → 21 963 octets) ; glyphes extraits 3 029 322 → 3 074 914
  et 3 493 756 → 3 499 093 ; inconnus 1,01 % → 0,82 % et 0,83 % → 0,57 % ; pages où du texte
  n'est pas dessiné inchangées (54 et 39 : polices CJK non incluses, sans police de remplacement) ;
  taux, crashs et timeouts inchangés, temps médian 27 et 15 ms ;
- fuzzing `font` (polices Type 0 aux encodages prédéfinis comprises) : 43 380 exécutions en 5 min
  (WSL), aucun crash ;
- après la revue : tables de CMap qu'une police détient seule comptées dans le cache des polices
  (une CMap incluse copiant un parent prédéfini pesait 4 Kio pour le cache), chargement des CMap
  prédéfinies sans réentrée ni échec gardé, `/CIDSystemInfo` en chaînes indirectes ou en noms,
  code d'une plage notdef sans texte de la collection, refus sans la fonction `cjk-cmaps` ;
  tests de `pdf-render` sans les fonctions par défaut : ceux de la recherche de collection passent,
  quatre tests de mise en page (`tests/layout.rs`) échouent faute de `standard-fonts`, comme sur
  `main` avant cette tranche.

Workflow `oracle` de la PR #75 (2026-11-14, poppler-data 0.4.12) : `cjk.pdf` identique à `pdftoppm`
à 216 dpi (0,000 % de pixels hors du voisinage) et à `pdftotext -raw`.

Type 1 inclus (tranche 3.8, 2026-11-14) :
- `type1.pdf` (police Type 1 synthétique : `eexec` binaire et hexadécimal, sous-routine,
  `seac`, encodage intégré, `/Differences`) identique à `pdftoppm` à 216 dpi (0,000 %) et à
  `pdftotext -raw` (WSL, Poppler 22.02) ; un programme illisible reste remplacé
  (`FontSubstituted`) ;
- `survey` (même protocole, avant et après sur la même machine, `main` dans un worktree) :
  premières pages qui déclarent une police Type 1 incluse 29 (SafeDocs) et 30 (UNSAFE) ;
  premières pages avec une police remplacée 666 → 661 et 934 → 922 (ces pages ont souvent
  aussi des polices non incluses) ; glyphes inconnus 25 156 → 16 393 (0,82 % → 0,53 %) et
  20 074 → 18 542 (0,57 % → 0,53 %) : les codes de l'encodage intégré d'une police Type 1 ont
  désormais un nom de glyphe ; taux, crashs et timeouts inchangés ;
- fuzzing `font` étendu au Type 1 (graine `simple-type1`) : 98 873 exécutions en 10 min (WSL),
  aucun crash ;
- après la revue : lecture bornée avant d'appeler `read-fonts` (noms d'encodage × glyphes au
  plus 10^8 comparaisons, la crate cherchant chaque nom linéairement, sans budget ni annulation ;
  testé à la borne et au-delà), mémoire de l'index des glyphes facturée et comptée au cache,
  largeurs du programme quand `/Widths` manque (régression évitée : la police de remplacement
  les donnait), programme Type 1 sous une police CID refusé avant lecture, code de l'encodage
  intégré sans glyphe non signalé (comme CFF) ; variante de `type1.pdf` à `/FontMatrix` inclinée
  de 2000 unités, toujours identique à `pdftoppm` et `pdftotext`.

CCITTFaxDecode (tranche 3.9, 2026-11-14) :
- décodeur du projet (`pdf-syntax`, `filter/ccitt.rs`) : tables T.4 vérifiées sans préfixe
  commun et retrouvées à leur longueur, rangées 1D, codes de complément, modes 2D (passe,
  horizontal, vertical), Group 3 à étiquette, EOL et bits de remplissage, fin de bloc,
  alignement sur l'octet, `BlackIs1`, données abîmées en erreur, extensions refusées comme non
  prises en charge, paramètres validés, chaque rangée facturée (limite testée) ;
- `ccitt.pdf` (encodeur T.4/T.6 du script de fixtures : Group 4 en gris, Group 3 2D avec EOL et
  `/Rows`, masque Group 4 peint en rouge, image en ligne, Group 3 1D de 2 600 colonnes avec EOL,
  alignement et `BlackIs1`) : les quatre images XObject décodées identiques bit à bit à
  `pdfimages` (Poppler 22.02, WSL ; workflow `oracle`) et dessinées pixel pour pixel comme le
  motif source (`tests/images.rs`) ;
- `pdftoppm` n'est pas l'oracle de cette fixture : à une image par pixel, il dessine les
  images 1 bit en gris avec des colonnes et rangées décalées d'un pixel (2 937 des 18 000 pixels
  de la première image diffèrent de ses données ; les nôtres, aucun ; les masques sont exacts
  chez lui aussi) ;
- `survey` (même protocole) : premières pages qui gardent une image non dessinée 113 → 48
  (SafeDocs) et 140 → 63 (UNSAFE) ; taux, crashs et timeouts inchangés, temps médian 29 et
  16 ms ;
- fuzzing `ccitt` (nouvelle cible, graines tirées de la fixture) : 11 264 956 exécutions en
  10 min (WSL), aucun crash ;
- après la revue : deux changements à la même colonne (plage de longueur nulle) s'annulent comme
  chez Poppler et pdf.js, ce qui borne une rangée à `columns` + 1 éléments (sinon une longue suite
  de modes verticaux sur place, derrière Flate, faisait grandir la mémoire hors budget) et garde
  la ligne de référence sans doublon ; un code de fin de ligne peut précéder une rangée Group 4,
  deux de suite terminent le bloc (même sans `/EndOfBlock`) ; des données qui s'arrêtent avant
  `/Rows`, ou avant `/Height` à l'affichage, laissent des rangées blanches, non noires ;
  l'extension 1D est non prise en charge, non abîmée ; décodage toujours identique à `pdfimages`,
  mesure sur le corpus inchangée (48 et 63), fuzzing `ccitt` 5 614 305 exécutions en 5 min.

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

Sélection et copie du texte (tranche 3.6b) :
7. Glisser la souris sur une ligne : la surbrillance couvre les mots dessinés.
8. Copier (Ctrl+C ou Cmd+C) une sélection de plusieurs lignes et la coller dans un éditeur :
   mots séparés d'une espace, lignes d'un saut de ligne, dans l'ordre de lecture.
9. Sur une page à deux colonnes, une sélection dans une colonne ne prend pas l'autre.
10. Faire défiler loin puis revenir : le texte d'une page revenue est de nouveau sélectionnable.
11. Lecteur d'écran (Narrateur, VoiceOver, Orca) : le texte de la page est lu.
12. Glisser à travers un vide (espace entre deux mots, interligne, marge, entre deux colonnes) :
    la sélection ne saute pas ailleurs dans la page.

Document de la procédure : texte synthétique de 12 pages produit par un script local, ni
document privé ni fixture (titre et deux colonnes, une colonne en Times, une page tournée de
90° avec deux tailles sur une ligne, pages de remplissage ; accents, ç, œ, €, « », ’, tiret).

| OS | Date | Points 7 à 12 |
| --- | --- | --- |
| Windows | 2026-11-14 | Première passe : la sélection sautait à la ligne du dessus sur tout vide (7, 9, 10) et le Narrateur lisait mot par mot (11). Après correction (une boîte par ligne, vrais espaces, élément de fin de contenu) : OK pour 7 à 10 et 12 ; 11 le Narrateur lit en continu d'une ligne à l'autre avec Verr. Maj + R (un clic sur un mot lit seulement sa ligne, comme tout élément cliqué). Une copie arrêtée au bout d'une ligne n'a pas d'espace final : le PDF n'en contient pas |
| macOS | à faire (pas de machine pour l'instant) | |
| Linux | à faire (pas de machine pour l'instant) | |

Recherche (tranche 3.7, même document ; sa page 2 déborde volontairement sous le bas de la
page et sur son pied de page) :
13. Ctrl+F (Cmd+F) met le curseur dans le champ de recherche.
14. Casse et accents ignorés par défaut (`chateau`, `coeur`, `NOEL`) : compteur et surlignage
    sur les bons mots, sur chaque occurrence, page tournée et page qui déborde comprises.
15. Entrée, Maj+Entrée, F3 et Maj+F3 parcourent les occurrences d'une page à l'autre, en
    boucle ; la page défile jusqu'à l'occurrence courante, centrée et entourée.
16. Une expression sur deux lignes (`épais de près de trois`) est surlignée sur ses deux lignes.
17. « Respecter les accents » et « Respecter la casse » restreignent les occurrences.
18. Échap efface la recherche ; le défilement reste fluide pendant une recherche, et une copie
    d'un texte surligné reste exacte.
19. Lecteur d'écran : le compteur de résultats est annoncé.

| OS | Date | Points 13 à 19 |
| --- | --- | --- |
| Windows | 2026-11-14 | Première passe : sur la page qui déborde, les occurrences 5 à 8 étaient surlignées une à trois lignes trop haut (`scrollIntoView` faisait défiler la couche de texte dans sa page). Après correction (couche en `overflow: clip`, seule la zone des pages défile) : OK pour 13 à 19, sans décalage sur aucune occurrence |
| macOS | à faire (pas de machine pour l'instant) | |
| Linux | à faire (pas de machine pour l'instant) | |

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
