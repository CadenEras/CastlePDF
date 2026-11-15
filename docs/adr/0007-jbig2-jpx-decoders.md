# ADR-0007 — Décodeurs JBIG2 et JPX

Statut : accepté le 2026-11-15
Date : 2026-11-15
Responsable : Mélissa Gries (maintenance du dépôt)

## Contexte et contraintes

ADR-0006 renvoie JBIG2 (`JBIG2Decode`, ITU-T T.88) et JPEG 2000 (`JPXDecode`, ISO/IEC
15444-1) à un ADR dédié, au moment de leur tranche (3.10). Les deux formats sont complexes et
historiquement riches en vulnérabilités (jbig2dec, OpenJPEG). Aujourd'hui une telle image est
signalée (`Image`), jamais dessinée.

Mesure du 2026-11-15 (`survey`, premières pages, après le décodeur CCITTFax de la tranche 3.9) :

| | SafeDocs (1 940 ouverts) | UNSAFE (2 282 ouverts) |
| --- | --- | --- |
| Premières pages qui gardent une image non dessinée | 48 | 63 |
| … avec une image JPX | 23 | 20 |
| … avec une image JBIG2 | 17 | 33 |
| … avec une image au-delà de `max_image_pixels` | 9 | 5 |

Contraintes (CLAUDE.md, `docs/SECURITY.md`) : pas de code natif ni d'`unsafe` dans le cœur, ni
dans une dépendance retenue sans revue ; licences compatibles Apache-2.0 OU MIT ; limites
cumulatives avant allocation, annulation, erreurs explicites ; chaque décodeur avec son oracle
et son fuzzing ; une image mal décodée n'est jamais présentée comme exacte.

## Options

### 1. Décodeurs écrits pour le projet

Le décodeur CCITTFax (tranche 3.9) fait environ 600 lignes. Un décodeur JBIG2 complet
(régions génériques, de raffinement, de symboles, de texte, demi-teintes, décodeur arithmétique
MQ, tables de Huffman) en compte de l'ordre de 10 000 ; un décodeur JPEG 2000 (ondelettes 5/3
et 9/7, EBCOT, ordres de progression, tuiles, boîtes JP2, espaces de couleur) autant ou plus.
Coût : des mois, puis une longue période de durcissement face à des données hostiles, pour une
fraction de pages. Écarté.

### 2. Bibliothèques C par FFI (jbig2dec, OpenJPEG)

Les plus éprouvées, mais du code natif, avec un passif de vulnérabilités. Exclu par ADR-0006 et
par la règle « pas de code natif dans le cœur ». Les portages traduits en Rust (`openjp2`,
c2rust) gardent l'essentiel en `unsafe`. Écarté.

### 3. `hayro-jbig2` et `hayro-jpeg2000` (projet hayro)

Décodeurs en Rust du projet hayro (lecteur PDF de Laurenz Stampfl), publiés séparément :

| | `hayro-jbig2` 0.4.0 | `hayro-jpeg2000` 0.5.0 |
| --- | --- | --- |
| Licence | Apache-2.0 OR MIT | Apache-2.0 OR MIT |
| `unsafe` | `#![forbid(unsafe_code)]` | `#![forbid(unsafe_code)]` |
| Taille | environ 9 500 lignes | environ 9 900 lignes |
| Dépendances requises | `hayro-ccitt` (`forbid(unsafe_code)`, `no_std`) | aucune |
| Options à désactiver | `simd` (`fearless_simd`, `unsafe`), `image` | `simd`, `image` (`image`, `moxcms`) |
| Téléchargements | environ 6,5 millions | environ 6,7 millions |
| Utilisateurs | 27 crates (pdf_oxide, kreuzberg…) | 34 crates |

Le dépôt (2026-11-15) est actif (commits quotidiens), a ses propres cibles de fuzzing
(`hayro-fuzz` : `fuzz_jbig2`, `fuzz_jpeg2000`, `fuzz_ccitt`) et une CI. `hayro-jpeg2000` est
testé sur plus de 20 000 images extraites de PDF et une grande partie de la suite d'OpenJPEG.

Défauts ouverts qui nous concernent (lus le 2026-11-15) :
- **#1549** (`hayro-jpeg2000`) : aucun budget de mémoire décodée. La taille vient du marqueur SIZ
  du flux ; un flux de 97 octets annonçant 100 000 × 100 000 demande environ 95 Go.
- **#1649** (`hayro-jpeg2000`) : panique dans `Image::decode` sur une boîte `pclr` à `cmap` vide
  ou une colonne de plus de 31 bits.
- **#1052** : pas d'annulation coopérative.
- **#1588** concerne `hayro-interpret`, que nous n'utilisons pas.

Ces défauts se contiennent à notre frontière : les dimensions et le nombre de composantes sont
connus après `Image::new` / `Page::new_embedded`, avant tout décodage ; une panique se capture
comme celle de `zune-jpeg` (`contained`, `docs/SECURITY.md`).

## Décision

Décision (acceptée le 2026-11-15) : **option 3**, `hayro-jbig2` et `hayro-jpeg2000` en versions exactes
(`=0.4.0`, `=0.5.0`), `default-features = false` avec seulement `std` (ni `simd`, ni `image`),
utilisés dans `pdf-render` seulement (comme `zune-jpeg`), derrière cette frontière :

1. **Taille avant décodage.** Largeur, hauteur et nombre de composantes lus par le décodeur
   (SIZ pour JPX, informations de page pour JBIG2), et non seulement ceux du dictionnaire de
   l'image : refus au-delà de `max_image_pixels`, puis facturation à `max_decoded_bytes` de la
   mémoire que le décodeur allouera (pixels × composantes × 4 octets pour JPX, qui travaille en
   flottants ; un octet par pixel pour la page JBIG2), avant l'appel. C'est la parade à #1549.
2. **Panique contenue.** Chaque appel sous `catch_unwind` : image signalée non dessinée.
3. **Temps.** Sans annulation dans le décodeur, une image bornée par `max_image_pixels`
   (32 Mi pixels) se décode d'un seul tenant ; l'annulation est vérifiée avant et après. Un
   décodage trop long reste couvert par le délai du shell et l'arrêt du worker (ADR-0001).
4. **`/JBIG2Globals`** décodés avec la page (`Globals`), sous le même budget.
5. **Couleur JPX.** Espaces gris, RVB et CMJN du flux ou du dictionnaire ; ICC et espaces
   d'extension non gérés restent signalés (`ColorSpace`), comme ailleurs.

Les deux crates sont nouvelles pour le projet : ajout à `deny.toml` si nécessaire, inventaire
dans `docs/SECURITY.md`, et mise à jour seulement par PR relue, avec le fuzzing relancé.

## Conséquences et validation

- **Oracles.** `pdfimages` (Poppler : jbig2 interne, OpenJPEG) : JBIG2 identique bit à bit ;
  JPX sans perte identique, JPX avec perte à une tolérance motivée, mesurée et documentée. Le
  rendu est comparé à `pdftoppm` là où Poppler dessine exactement (voir l'écart des images 1 bit
  à l'échelle 1 noté en 3.9).
- **Fixtures.** Aucun encodeur JPX ni JBIG2 en Rust sûr n'est assez mûr pour les produire dans
  `scripts/make-fixtures.mjs`. Elles viennent d'encodeurs de référence lancés une fois par un
  script versionné (`opj_compress` d'OpenJPEG, BSD-2-Clause ; `jbig2enc`, Apache-2.0), hors CI,
  sur des images synthétiques du projet ; fichiers commités avec provenance, versions et sha256,
  comme les polices de remplacement. Couverture : JBIG2 générique (MMR et arithmétique), symboles
  et texte, `/JBIG2Globals`, masque ; JPX sans perte et avec perte, gris, RVB, CMJN, plusieurs
  tuiles et niveaux, boîte JP2 et flux nu, `/SMaskInData`.
- **Fuzzing.** Nouvelles cibles `jbig2` et `jpx` dans notre `fuzz/`, avec le budget de la
  frontière ; graines tirées des fixtures et des cas de #1549 et #1649.
- **Mesure.** `survey` avant et après : premières pages qui gardent une image non dessinée
  (48 et 63), temps par fichier, mémoire résidente maximale des sondes.
- **Taille du binaire** mesurée et publiée dans la PR.
- **Retour arrière.** Chaque décodeur est une tranche (3.10a JBIG2, 3.10b JPX) ; retirer une
  dépendance ramène l'image à `Image`, sans autre effet.
- **Défauts amont.** Suivre #1549, #1649 et #1052 ; une version qui les corrige ne retire nos
  parades qu'avec leurs tests verts sans elles. Tout signalement en amont attend l'accord de la
  maintenance du dépôt.
