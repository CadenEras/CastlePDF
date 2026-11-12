# ADR-0006 — Texte et compatibilité (P3)

Statut : accepté le 2026-11-12
Date : 2026-11-12
Responsable : Mélissa Gries (maintenance du dépôt)

## Contexte et contraintes

P2 s'est terminée avec `v0.1.0-alpha.3`. L'application affiche des PDF, mais leur texte n'est
qu'une image : on ne peut ni le sélectionner, ni le copier, ni le chercher, et un lecteur
d'écran n'y a pas accès. Le critère de sortie de P3 (`docs/ROADMAP.md`) est : « matrice de
support publiée, tests sur corpus hétérogène, lecture et copie Unicode vérifiées ».

Ce qui existe déjà et sert au texte :
- l'interpréteur de `pdf-render` connaît chaque glyphe dessiné : code, police, matrice de
  texte, largeur ;
- les encodages simples (Standard, WinAnsi, MacRoman, MacExpert, Symbol, ZapfDingbats) et
  l'Adobe Glyph List sont générés dans `text/encodings.rs` ;
- les polices composites ne sont lues qu'en `Identity-H` ; les marques de contenu (`BDC`,
  `BMC`) sont reconnues, mais leurs propriétés (`/ActualText`) ne sont pas lues.

Ce qui reste reporté de P2 (feuille de route) : filtres CCITTFax, JBIG2 et JPX ; pages dont
les images dépassent le budget de 256 Mio ; réinterprétation de la page par tuile ; images en
ligne redécodées. Le Type 1 inclus (`FontFile`, environ 1,5 % des premières pages) est encore
remplacé par une police libre.

Contraintes :
- le texte extrait est une donnée non fiable : budgets (caractères par page, taille des
  CMap, profondeur de `usecmap`), annulation, aucun panic, rendu dans l'UI comme texte
  seulement (règle TypeScript : aucun HTML, script ou lien actif venant du PDF) ;
- pas d'`unsafe` ni de code natif dans le cœur ; licences compatibles Apache-2.0 OU MIT ;
- un caractère dont l'Unicode est inconnu est signalé, jamais inventé ;
- une copie ou une recherche n'est annoncée « vérifiée » que contre un oracle indépendant.

## Options

### Où extraire le texte

1. **Une crate `pdf-text`.** Elle dupliquerait l'interpréteur ou devrait en extraire une
   interface commune : beaucoup de déplacement de code pour une frontière qui n'existe pas.
2. **Un second consommateur de l'interpréteur de `pdf-render`.** L'interpréteur émet déjà
   les glyphes ; un module `text` reçoit, au lieu de les dessiner, leur Unicode, leur boîte
   et leur style. Rendu et texte partagent ainsi polices, encodages et budgets.

### Correspondance code → Unicode

Ordre de la norme (ISO 32000-2, 9.10.2) : `/ToUnicode`, puis encodage simple et noms de
glyphes (AGL, `uniXXXX`, `uXXXX[XX]`), puis CMap prédéfinie de la collection CID
(`Adobe-Japan1`, `-GB1`, `-CNS1`, `-Korea1`) vers UCS-2. Une table `cmap` de la police
incluse est un dernier recours, signalé comme tel.

Les CMap prédéfinies demandent les données d'Adobe `cmap-resources` (BSD-3-Clause, plusieurs
Mo). Les embarquer n'a de sens que si des pages en ont besoin sans `/ToUnicode`.

### Sélection dans l'UI

- **(a) Couche de texte DOM** transparente sur le canevas, comme pdf.js : sélection, copie,
  recherche du navigateur et lecteurs d'écran natifs. Coût : des nœuds DOM par page visible,
  bornés par la virtualisation existante.
- **(b) Sélection dessinée sur le canevas.** Tout est à réécrire (gestes, presse-papiers,
  accessibilité), sans gain de sécurité : le texte serait de toute façon du texte.

### JBIG2 et JPX

Formats complexes, historiquement riches en vulnérabilités. Écrire nos décodeurs est long ;
OpenJPEG ou jbig2dec par FFI sont exclus (code natif). Des décodeurs en Rust sûr existent
peut-être à cette date : à évaluer dans un ADR dédié au moment de leur tranche, avec licence,
maintenance, fuzzing et oracle. CCITTFax (G3/G4) est plus petit et bien spécifié.

## Décision

- **Extraction dans `pdf-render`**, option 2, module `text` consommateur de l'interpréteur.
- **Unicode** dans l'ordre de la norme. **CMap prédéfinies** (décidé le 2026-11-12 d'après la
  mesure de 3.1, `docs/QUALITY.md`) : intégrées en tranche 3.7b, après la sélection, la copie
  et la recherche. Seulement les quatre CMap vers UCS-2 des collections Adobe CJK et les
  encodages nommés rencontrés sur le corpus, générés en tables compactes depuis une version
  figée d'Adobe `cmap-resources`, avec sa notice BSD-3-Clause ; taille ajoutée mesurée et
  publiée dans la PR. Raison : elles servent l'Unicode d'au plus 1,7 % des premières pages
  ouvertes, mais aussi le rendu de 22 et 6 pages dont le texte n'est pas dessiné du tout.
- **Couche de texte DOM**, option (a).
- **Texte d'abord, filtres ensuite** : le texte est ce qui manque au lecteur ; les filtres
  touchent 113 premières pages SafeDocs et 140 UNSAFE, déjà signalées.

Ordre en tranches vérifiables :

| Tranche | Contenu | Preuve attendue |
| --- | --- | --- |
| 3.1 | Mesure : `survey` compte, par page, les polices avec et sans `/ToUnicode`, les encodages (`Identity-H/V`, CMap incluses, CMap prédéfinies) ; le marquage `/ActualText`, qui demande d'interpréter le contenu, est mesuré en 3.5 | Tableau dans `docs/QUALITY.md` ; décision sur les CMap prédéfinies |
| 3.2 | Lecteur de CMap partagé (`codespacerange`, `bfchar`, `bfrange`, `cidchar`, `cidrange`, `usecmap` borné) pour `/ToUnicode` et les encodages | Vecteurs de la norme, cas invalides et limites, cible de fuzzing `cmap` |
| 3.3 | Unicode de chaque glyphe et `castle-pdf text <fichier> [--page n]` dans l'ordre du contenu ; caractères inconnus signalés | Fixtures comparées à `pdftotext` ; part des glyphes résolus sur le corpus |
| 3.4 | Encodages composites au-delà d'`Identity-H` : `Identity-V` et CMap incluses, pour le rendu comme pour le texte (CMap prédéfinies en 3.7b) | Fixtures rendues identiques à `pdftoppm`, texte comparé à `pdftotext` |
| 3.5 | Mise en page : mots, lignes, blocs, ordre de lecture, espaces déduits des écarts, `/ActualText`, texte tourné | Mesure mot à mot contre `pdftotext` sur fixtures et échantillon du corpus, tolérance documentée ; part des pages marquées `/ActualText` |
| 3.6 | Protocole worker : texte d'une page (lignes, caractères, boîtes) borné ; couche de texte DOM ; sélection, copie, lecteurs d'écran | Tests du worker et de l'UI ; copie vérifiée à la main sur chaque OS disponible |
| 3.7 | Recherche dans le document, côté worker : résultats progressifs, annulation, surlignage, précédent et suivant au clavier | Tests de bout en bout sur fixtures ; temps mesuré sur un long document |
| 3.7b | CMap prédéfinies d'Adobe : UCS-2 des quatre collections CJK et encodages nommés rencontrés, tables générées, notice BSD-3-Clause | Fixtures CJK rendues identiques à `pdftoppm` et texte comparé à `pdftotext` ; taille ajoutée publiée ; pages du corpus 3.1 de nouveau mesurées |
| 3.8 | Type 1 inclus (`FontFile`) : lecture du programme (`eexec`, charstrings) ou dépendance, selon un amendement d'ADR-0005 | Fixture identique à `pdftoppm` ; fuzzing |
| 3.9 | CCITTFaxDecode (G3 1D/2D, G4), décodeur du projet | Vecteurs, oracle `pdftoppm`, fuzzing |
| 3.10 | JBIG2 et JPX selon un ADR dédié | Selon l'ADR |
| 3.11 | Limites reportées de P2, si la mesure le justifie : images au-delà du budget (décodage réduit), liste d'affichage par page, images en ligne gardées | Mesure avant et après, protocole de `docs/PERFORMANCE.md` |
| 3.12 | Matrice de support publiée (`docs/SUPPORT.md`), puis `v0.1.0-alpha.4` | Critère de sortie de P3 |

## Conséquences et validation

- Le protocole du worker change en 3.6 (nouvelle requête et nouvelle réponse, version
  incrémentée) ; les autres tranches ne touchent pas l'IPC.
- Nouveaux budgets dans `ResourceLimits` : caractères par page, octets de CMap, profondeur
  de `usecmap`. Chacun testé à la limite et à la limite + 1.
- Oracles : `pdftotext` (Poppler) pour le texte, `pdftoppm` pour le rendu ; un second oracle
  (MuPDF `mutool draw -F txt`, AGPL, utilisé seulement comme outil de test hors du dépôt)
  pour départager un écart. Un oracle peut se tromper : chaque écart accepté est documenté.
- Retour arrière : chaque tranche est une PR autonome ; une couche de texte défaillante peut
  être désactivée sans toucher au rendu.
- Accepté le 2026-11-12 : texte avant filtres, couche de texte DOM, CMap prédéfinies
  décidées après la mesure de la tranche 3.1 (même jour : intégrées en 3.7b).
