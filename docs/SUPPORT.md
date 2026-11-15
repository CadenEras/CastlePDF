# Matrice de support

État au 2026-11-15, préparé pour `v0.1.0-alpha.4` (fin de P3). Elle dit ce que le moteur et
l'application font **aujourd'hui**, d'après les tests du dépôt et la mesure sur corpus. Elle
ne vaut ni conformité à ISO 32000, ni parité avec un autre lecteur. Toujours **aucun binaire
distribué** : version de développement, à construire depuis les sources.

## Lire cette matrice

| État | Sens |
| --- | --- |
| **Pris en charge** | Implémenté et testé sur des fixtures du dépôt, comparé à un oracle indépendant quand la colonne « Preuve » en cite un |
| **Partiel** | Implémenté avec la restriction indiquée |
| **Signalé** | Non dessiné ou non appliqué. Le rendu le dit (`Rendering::skipped`, mention « Rendu partiel » dans l'application), jamais en silence |
| **Refusé** | Le document ou la page n'est pas ouvert : erreur de capacité ou de limite explicite |
| **Absent** | Ni pris en charge, ni signalé. Cette catégorie est l'exception à la règle « jamais en silence » et chaque ligne dit pourquoi |

Les fixtures sont dans `tests/corpus/public` (`scripts/make-fixtures.mjs`). Les oracles sont
Poppler (`pdftoppm`, `pdftotext`, `pdfimages`, 22.02 en CI) et qpdf, lancés par le workflow
`oracle`. Un oracle peut se tromper : les écarts connus sont documentés dans chaque test.

## Mesure sur corpus

Sortie de `castle-pdf survey`, premières pages (release, WSL, 2026-11-15, détails dans
`docs/QUALITY.md`) :

| Corpus | Fichiers | Ouverts | Dont par réparation | Première page rendue | Crash, timeout |
| --- | --- | --- | --- | --- | --- |
| SafeDocs | 2 000 | 1 940 (97,0 %) | 78 | 1 920 (96,0 %) | 0, 0 |
| UNSAFE (Common Crawl) | 2 636 | 2 282 (86,6 %) | 223 | 2 229 (84,6 %) | 0, 0 |

Une page « rendue » peut être partielle. Le tableau suivant compte les premières pages
rendues qui signalent chaque manque (une page peut en avoir plusieurs) :

| Signalé | SafeDocs (1 920) | UNSAFE (2 229) |
| --- | --- | --- |
| `FontSubstituted` (police de remplacement) | 661 | 922 |
| `ColorSpace` | 380 | 409 |
| `SoftMask` | 91 | 34 |
| `BlendMode` | 75 | 32 |
| `Shading` | 69 | 46 |
| `AlphaIsShape` | 60 | 27 |
| `OptionalContent` | 54 | 53 |
| `Image` | 48 | 63 |
| `Pattern` | 39 | 22 |
| `Text` | 34 | 26 |
| `XObject` | 3 | 13 |
| `TextClip` | 7 | 2 |
| `TransferFunction` | 1 | 0 |

Texte des premières pages : 0,53 % de glyphes sans Unicode connu sur les deux corpus,
signalés et jamais devinés.

## Structure du fichier

| Capacité | État | Détail | Preuve |
| --- | --- | --- | --- |
| En-tête `%PDF-1.0` à `1.7` et `2.0` | Pris en charge | Autre version : refusée (`PdfVersion`) | Tests `pdf-syntax` |
| Table xref classique, mises à jour incrémentales | Pris en charge | Cycles `/Prev` détectés | `incremental.pdf`, qpdf |
| Flux xref, flux d'objets, fichiers hybrides | Pris en charge | Flux d'objets en cache borné (8 flux, 32 Mio) | `objstm.pdf`, `hybrid.pdf`, qpdf |
| Fichier abîmé | Partiel | Index reconstruit par balayage, signalé « Document réparé » (ADR-0004) ; certains objets peuvent manquer, ce qui est dit aussi | `tests/corpus/damaged`, fuzzing `repair` |
| Arbre des pages, attributs hérités | Pris en charge | `MediaBox`, `CropBox`, `Rotate`, `Resources`, `/UserUnit` | `page-tree.pdf` (ordre des pages comparé à qpdf) ; attributs par tests unitaires |
| `BleedBox`, `TrimBox`, `ArtBox`, libellés de pages | Absent | Non lus : la page est rendue selon sa `CropBox` comme le font les lecteurs | — |
| Chiffrement (`/Encrypt`) | Refusé | Erreur de capacité `Encryption` ; jamais réparé ni deviné. 42 et 52 fichiers du corpus | Tests `pdf-syntax` |
| Données de flux externes (`/F`) | Refusé | `ExternalStreamData` ; aucun accès à un autre fichier | Tests `pdf-syntax` |
| Contenu de page non conforme | Refusé (la page) | Opérateur mal placé, flux sans fin de ligne après `stream`, contenu non terminé : la page échoue avec une erreur localisée, sans réparation du contenu | `survey` |

## Filtres de flux

| Filtre | État | Détail | Preuve |
| --- | --- | --- | --- |
| `FlateDecode`, prédicteurs PNG et TIFF | Pris en charge | Prédicteur TIFF : composantes de 8 bits seulement | `flate.pdf` (PNG), qpdf ; TIFF par tests unitaires |
| `LZWDecode`, `ASCIIHexDecode`, `ASCII85Decode`, `RunLengthDecode` | Pris en charge | | `filters.pdf`, qpdf, `pdftoppm` |
| `DCTDecode` (JPEG) | Pris en charge | Images seulement, `zune-jpeg` ; une panique du décodeur est contenue | `jpeg.pdf`, `pdftoppm` |
| `CCITTFaxDecode` | Pris en charge | Images seulement, décodeur du projet ; `/DamagedRowsBeforeError` non suivi (image abîmée non dessinée) | `ccitt.pdf`, `pdfimages` |
| `JBIG2Decode` | Signalé (`Image`) | En attente d'une version de `hayro-jbig2` qui borne sa mémoire (ADR-0007, tranche 3.10a) | — |
| `JPXDecode` (JPEG 2000) | Signalé (`Image`) | Tranche 3.10b, ADR-0007 | — |
| `Crypt` (fichier non chiffré) | Signalé | Filtre inconnu (`StreamFilter`) : seul le flux est ignoré, une image signalée `Image`, un formulaire `XObject` | Tests `pdf-syntax` |

## Dessin

| Capacité | État | Détail | Preuve |
| --- | --- | --- | --- |
| Chemins, remplissage (non nul, pair-impair), contours, tirets, découpe | Pris en charge | Rasterisation `vello_cpu`, géométrie en double précision | `shapes.pdf`, `pdftoppm` |
| DeviceGray, DeviceRGB, DeviceCMYK | Partiel | Sans gestion de couleur : CMYK converti par la formule d'ISO 32000-2, 10.4.2.4 (cyan pur plus vif que chez Poppler) | `shapes.pdf` |
| Autres espaces pour les tracés et le texte (ICCBased, CalGray, CalRGB, Indexed, Lab, Separation, DeviceN, ou espace de périphérique remplacé par `Default*`) | Signalé (`ColorSpace`) | **Le tracé ou le texte peint dans cet espace n'est pas dessiné** ; premier manque du corpus après les polices remplacées | `survey` |
| Alpha constant (`/CA`, `/ca`) | Pris en charge | | `forms.pdf` |
| XObjects de formulaire, groupes de transparence | Pris en charge | Groupe à élimination (`/K true`) dessiné comme un groupe ordinaire, signalé (`BlendMode`) | `forms.pdf`, `pdftoppm` |
| Dégradés (`sh`, `Shading`) | Signalé (`Shading`) | | — |
| Motifs (`Pattern`) | Signalé (`Pattern`) | | — |
| Masques adoucis (`/SMask` d'un `ExtGState`) | Signalé (`SoftMask`) | Contenu peint sans masque : le rendu peut montrer trop | — |
| Modes de fusion autres que `Normal` | Signalé (`BlendMode`) | Peint en `Normal` | — |
| `/AIS true`, fonctions de transfert | Signalé (`AlphaIsShape`, `TransferFunction`) | | — |
| Contenu optionnel (`BDC /OC`) | Signalé (`OptionalContent`) | Tout est dessiné, y compris un contenu peut-être caché | — |
| Intention de rendu (`ri`), tolérance d'aplatissement (`i`), demi-teintes, génération du noir, retrait de sous-couleur, ajustement des contours | Absent | Lus puis ignorés : sans gestion de couleur ni sortie imprimée, ils n'ont pas d'effet ici | — |
| Surimpression (`OP`, `op`, `OPM`) et élimination du texte (`TK`) | Absent | Ignorées sans signalement ; la surimpression peut changer l'aspect d'un document prévu pour l'impression | — |
| XObject PostScript, formulaire abîmé ou trop imbriqué | Signalé (`XObject`) | Dessiné jusqu'à l'erreur, n'affecte que lui | Tests `forms.rs` et `images.rs` (PDF synthétiques) |

## Polices et texte dessiné

| Capacité | État | Détail | Preuve |
| --- | --- | --- | --- |
| TrueType incluses, simples et `CIDFontType2` | Pris en charge | `skrifa` ; encodages Standard, WinAnsi, MacRoman, MacExpert, `/Differences`, tables `cmap`, `/CIDToGIDMap` | `truetype.pdf`, `pdftoppm` |
| CFF incluses (`Type1C`, `CIDFontType0C`, OpenType) | Pris en charge | `read-fonts` | `cff.pdf`, `pdftoppm` |
| Type 1 incluses (`/FontFile`) | Pris en charge | `eexec`, PFB, `seac` ; programme illisible remplacé et signalé | `type1.pdf`, `pdftoppm` |
| Type 3 | Pris en charge | Procédures de glyphes exécutées comme des formulaires | `type3.pdf`, `pdftoppm` |
| Polices simples non incluses (14 standard, Arial, Times New Roman…) | Partiel (`FontSubstituted`) | Dessinées avec Arimo, Tinos ou Cousine (OFL) aux largeurs du PDF ; les glyphes ressemblent à l'original sans l'être | Tests de métriques AFM |
| Symbol et ZapfDingbats non incluses | Partiel (`FontSubstituted`) | Police « CastlePDF Symbols » (OFL) | Tests `pdf-render` (`fonts.rs`, `text.rs`) |
| Polices CJK non incluses | Signalé (`Text`) | Aucune police de remplacement CJK livrée ; le texte reste extrait | Tests `pdf-render` |
| Autre police symbolique non incluse, glyphe absent du programme | Signalé (`Text`) | | — |
| Encodages Type 0 | Pris en charge | `Identity-H`, `Identity-V`, CMap incluses (`usecmap` sur 4 niveaux), 12 CMap prédéfinies d'Adobe (6 encodages CJK et leurs formes verticales, fonction `cjk-cmaps` active par défaut) | `cmaps.pdf`, `cjk.pdf`, `pdftoppm` |
| Autre CMap prédéfinie | Signalé (`Text`) | Octets comptés (`unread_bytes`) : 0 et 10 premières pages | `survey` |
| Écriture verticale | Pris en charge | `/W2`, `/DW2` | `cmaps.pdf` |
| Modes de rendu 0 à 3 (remplissage, contour, les deux, invisible) | Pris en charge | | `truetype.pdf` |
| Modes de rendu 4 à 7 (ajout à la découpe) | Signalé (`TextClip`) | Le texte des modes 4 à 6 est peint, mais la découpe n'est pas appliquée : ce qui suit peut montrer trop | — |
| Hinting | Absent | Glyphes dessinés sans hinting | — |

## Images

| Capacité | État | Détail | Preuve |
| --- | --- | --- | --- |
| XObjects image et images en ligne | Pris en charge | Abréviations d'ISO 32000-2, 8.9.7 développées | `images.pdf`, `inline-images.pdf`, `pdftoppm` |
| Profondeurs 1, 2, 4, 8, 16 bits, `/Decode` | Pris en charge | | `images.pdf` |
| DeviceGray, RGB, CMYK, ICCBased (par `/N`), CalGray, CalRGB, Indexed | Partiel | Sans gestion de couleur : un profil ICC est remplacé par l'espace de base à `/N` composantes | `images.pdf`, `jpeg.pdf` |
| Lab, Separation, DeviceN | Signalé (`ColorSpace`) | | — |
| Masques de pochoir, `/SMask`, `/Mask` (pochoir ou clé de couleur), `/Matte` | Pris en charge | | `images.pdf`, `pdftoppm` |
| Image JPEG dont le PDF déclare un espace Indexed, Separation, DeviceN ou Lab | Absent | L'espace vient du nombre de composantes du JPEG (gris, RVB, CMJN) : l'image est dessinée dans cet espace, sans signalement | — |
| `DefaultGray`, `DefaultRGB`, `DefaultCMYK` sur une image | Absent | Non appliqués aux images en espace de périphérique (les tracés, eux, sont signalés) | — |
| Contenu optionnel (`/OC`) d'une image | Absent | Non lu : une image d'un calque masqué est dessinée sans signalement (le `/OC` d'un formulaire et `BDC /OC` sont signalés) | — |
| Image de plus de 32 Mi pixels (`max_image_pixels`) | Signalé (`Image`) | Taille vérifiée avant allocation | Tests `images.rs` |
| Page dont les données décodées (flux, polices, images et leur conversion en RGBA) dépassent 256 Mio | Refusé (la page) | `limit/DecodedBytes` : 2 et 2 premières pages (voir la feuille de route) | `survey` |

## Texte, recherche et sélection

| Capacité | État | Détail | Preuve |
| --- | --- | --- | --- |
| Unicode par `/ToUnicode`, noms de glyphes, encodage intégré, collections Adobe (Japan1, GB1, CNS1, Korea1) | Pris en charge | Un glyphe sans source est **inconnu et signalé**, jamais deviné | `text.pdf`, `pdftotext` |
| `/ActualText` | Pris en charge | | `layout.pdf` |
| Mots, lignes, ordre de lecture (colonnes) | Partiel | Découpes XY ; au-delà de 10 000 lignes, tri de haut en bas | Tests `layout.rs` (`layout.pdf`, `columns.pdf`) ; comparaison à `pdftotext` sur corpus local seulement (test ignoré `layout_corpus`) |
| Police CID `Identity` sans `/ToUnicode` d'une autre collection, TrueType symbolique sans `/Encoding`, ZapfDingbats inclus | Signalé | Texte inconnu (feuille de route, limites ouvertes de P3) | — |
| Recherche (CLI, worker, application) | Pris en charge | Casse et accents latins ignorés sauf demande, ligatures repliées, d'une ligne à l'autre ; 1 000 occurrences par page, 10 000 par recherche dans l'application | Tests `search`, fuzzing `search` |
| Sélection et copie dans l'application | Pris en charge | Couche de texte par ligne ; vérifiée à la main sous Windows | `docs/QUALITY.md` |
| Lecteur d'écran | Partiel | Narrateur de Windows vérifié à la main ; VoiceOver et Orca non vérifiés | `docs/QUALITY.md` |

## Interactivité et sécurité

| Capacité | État | Détail |
| --- | --- | --- |
| Annotations (liens, notes, tampons, surlignages…) | **Absent** | Ni dessinées ni signalées : leur apparence (`/AP`) n'est pas lue. Prévues en P5 ; jusque-là, une page annotée s'affiche sans ses annotations et sans le dire |
| Formulaires AcroForm et XFA | **Absent** | Comme les annotations, dont les champs AcroForm sont des widgets : valeurs non affichées. P5 |
| Liens, signets, navigation par destinations | Absent | P5 au plus tôt |
| JavaScript, actions `Launch`, fichiers joints | Jamais exécutés | Règle de sécurité du projet (`docs/SECURITY.md`) |
| Ressources réseau ou externes | Jamais chargées | |
| Signatures numériques | Absent | Ni lues ni vérifiées ; aucune signature n'est jamais présentée comme valide (P6) |

## Écriture

Aucune : le moteur ne modifie ni n'enregistre de PDF (P4). Le fichier source n'est jamais
ouvert en écriture.

## Application desktop

| Capacité | État | Détail |
| --- | --- | --- |
| Ouvrir un PDF | Pris en charge | Glisser-déposer, argument de ligne de commande, Finder (macOS, `.app` empaqueté seulement) |
| Défilement continu, ajusté à la largeur, navigation page par page | Pris en charge | |
| Zoom | Absent | Page toujours ajustée à la largeur |
| Impression | Absent | Prévue avec l'impression selon l'OS (P5) |
| Rendu partiel, polices remplacées, document réparé | Signalé | Notes à l'écran |
| Recherche, sélection, copie | Pris en charge | Voir plus haut |

## Plateformes

| Plateforme | État |
| --- | --- |
| Windows x64 | Tests et build natif en CI ; vérifié à la main (affichage, défilement, glisser-déposer, sélection, recherche, Narrateur) |
| Windows ARM64, Linux x64 et ARM64, macOS ARM64 et Intel | Tests et build natif en CI ; affichage vu sur captures CI (Linux, macOS) ; aucun geste vérifié à la main |
| Paquets, installateurs, signature, mises à jour | Absent (phase de diffusion) |

## Limites et ressources

Chaque document, police, image et contenu est une entrée non fiable. Limites par défaut
(`ResourceLimits`, `RenderLimits`) : 512 Mio de fichier, 256 Mio de données décodées par
tâche, un million d'objets visités, 32 Mi pixels par image, 16 384 pixels par côté de rendu, un million de glyphes de texte
par page. Une image trop grande est sautée et signalée (`Image`) ; tout autre dépassement
est une erreur explicite, jamais un résultat tronqué présenté comme complet. Détails : `docs/SECURITY.md` et
`docs/PERFORMANCE.md`.
