# Phasage par capacité vérifiable

Pas d'échéances artificielles : chaque phase se termine par des preuves. Les numéros
de phase ne sont pas les versions SemVer ; le support PDF est une matrice de capacités,
pas un booléen « compatible PDF 2.0 ».

| Phase | Livrable | Critère de sortie |
| --- | --- | --- |
| P0 — fondations | Workspace, UI vide, protocole worker, budgets, corpus, CI OS | Build natif des trois OS, contrôle annulation/worker, toolchains verrouillées et baseline mémoire |
| P1 — lecture structurée | Tokenizer, objets, xref classique, sources, pages ; CLI diagnostic | Cas valides et invalides, limites et références cycliques testés ; refus explicite des capacités absentes |
| P2 — lecteur utile | Filtre Flate, xref/object streams, interpréteur graphique minimal, rendu tuilé, polices/images ciblées, navigation (ordre : ADR-0003) | Première page rendue, UI réactive, corpus/goldens et comparaisons, mémoire/worker contrôlés |
| P3 — compatibilité et texte | Filtres secondaires, texte/ToUnicode, recherche/sélection | Matrice de support publiée, tests sur corpus hétérogène, lecture et copie Unicode vérifiées |
| P4 — écriture et pages | Writer, rotation/réordre/suppression, fusion/découpe, sauvegarde | Réouverture indépendante, erreurs d'écriture sans perte, propriétés de round-trip et undo/redo |
| P5 — annotations/formulaires | Annotations, apparences, AcroForm simple, impression selon OS | Réouverture dans moteurs indépendants, valeurs/apparences conservées, accessibilité UI testée |
| P6 — fonctions avancées séparées | OCR, édition de texte existant, chiffrement, signatures, caviardage | Étude et tests propres à chaque fonction ; aucune promesse de sécurité avant validation dédiée |
| Stabilisation — diffusion | Packaging, signatures OS, mises à jour, guide utilisateur | Release depuis tag testé, corpus accepté, mesures et limites publiques, récupération après erreur |

## P3 terminée (`v0.1.0-alpha.4`)

Ordre ([ADR-0006](adr/0006-p3-text-and-compatibility.md), accepté le 2026-11-12) : 3.1 mesure du texte sur le corpus (fait) ; 3.2 lecteur de CMap (fait) ; 3.3 Unicode des
glyphes et `castle-pdf text` (fait) ; 3.4 encodages composites (3.4a `Identity-V` et CMap
incluses, fait ; 3.4b police « texte seulement », fait) ; 3.5 mise en page (3.5a mots, lignes et `/ActualText`, fait ; 3.5b ordre de lecture, fait) ; 3.6 couche de
texte, sélection et copie (fait ; vérifiée à la main sous Windows le 2026-11-14) ; 3.7 recherche (3.7a moteur, CLI, worker et shell, fait ; interface, fait, vérifiée à la main sous Windows le 2026-11-14) ; 3.7b CMap prédéfinies d'Adobe (CJK, fait) ; 3.8 Type 1 inclus (fait, ADR-0005 amendé) ; 3.9 CCITTFax (fait) ; 3.10 JBIG2 et
JPX ([ADR-0007](adr/0007-jbig2-jpx-decoders.md), accepté le 2026-11-15 : 3.10a JBIG2, 3.10b JPX) ; 3.11 limites reportées de P2 (fait : remesurées, aucune optimisation justifiée, voir plus bas) ; 3.12 matrice de support ([`docs/SUPPORT.md`](SUPPORT.md), fait) ; 3.13 signaler ce qui manquait en silence (annotations, champs de formulaire, espace et calque des images ; fait) ; 3.14 espaces de couleur des tracés et du texte (ICCBased, CalGray, CalRGB, Indexed ; fait) ; `v0.1.0-alpha.4` le 2026-11-16.

3.10 (JBIG2 et JPX) reste ouverte après la fin de P3 : `hayro-jbig2` 0.4.0 laisse 140 octets
coûter 7 s et 423 Mo hors du budget (trouvé par fuzzing, défaut amont #1701 corrigé sur la
branche principale de hayro mais pas encore publié). Le travail est gardé sur une branche
locale et reprendra avec la version qui borne la mémoire du décodeur ; ces images restent
signalées (`Image`). Les autres limites ouvertes ci-dessous sont publiées comme limites
connues de `v0.1.0-alpha.4`.

### Limites ouvertes de P3 (texte)

Relevées à la tranche 3.3. Chacune est levée par une tranche ou reportée avec sa raison.

| Limite | Pourquoi elle compte | Sortie attendue |
| --- | --- | --- |
| Depuis 3.7b, les 6 encodages prédéfinis rencontrés sur le corpus et leurs formes verticales (12 CMap, `crates/pdf-render/cmaps`) sont lus. Une autre CMap prédéfinie (`UniJIS-UCS2-HW-H`, `GBK-EUC-H`…) reste non lue : texte perdu, octets comptés (`unread_bytes`) | 0 première page SafeDocs, 10 UNSAFE (2026-11-14), dont des dictionnaires abîmés | Ajouter une CMap d'Adobe à `scripts/make-cmaps.mjs` quand la mesure en trouve |
| Une police CJK non incluse n'est pas dessinée : aucune police de remplacement CJK n'est livrée. Son texte est extrait (3.4b et 3.7b) | 54 et 39 premières pages où du texte n'est pas dessiné (`Text`), inchangées par 3.7b | Police de remplacement CJK (taille et licence à étudier, plusieurs Mo), si la mesure le justifie |
| Les notices des données tierces incluses dans le binaire (polices OFL, CMap d'Adobe en BSD-3-Clause) ne sont livrées qu'avec les sources (`crates/pdf-render/fonts`, `crates/pdf-render/cmaps`) : une distribution binaire doit les reproduire dans sa documentation | Condition des licences pour tout artefact publié | Fichier de notices tierces joint aux binaires et à l'application, avant le premier artefact qui en contient (au plus tard en stabilisation) |
| Une CMap incluse qui déclare `/WMode 0` mais fait `usecmap` d'une CMap verticale (`Identity-V`, une `-V` prédéfinie) devient verticale : le lecteur ne distingue pas `/WMode 0` d'un `/WMode` absent | Cas improbable, non vu sur le corpus | Garder le `/WMode` déclaré, si un document en montre le besoin |
| Police CID `Identity` sans `/ToUnicode` d'une collection autre qu'Adobe-Japan1, -GB1, -CNS1 ou -Korea1 (celles-ci passent par leur CMap UCS2 depuis 3.7b), TrueType symbolique sans `/Encoding`, ZapfDingbats inclus (noms `a1`…) : texte inconnu | Signalé, jamais deviné, mais absent | Repli signalé sur la table `cmap` ou `post` de la police incluse et sur la liste ZapfDingbats, si la mesure le justifie |
| `usecmap` d'une `/ToUnicode` non fusionné (celui des CMap d'encodage l'est depuis 3.4a, sur quatre niveaux) | Parent ignoré | Même résolution bornée que les encodages, si la mesure en trouve |
| Une `/ToUnicode` endommagée est rejetée entière (Poppler garde le début) | Texte venu des noms de glyphes ou inconnu | Mesurer la fréquence sur le corpus avant de choisir une tolérance |
| Relevée en 3.12, levée pour les tracés et le texte en 3.14 (ICCBased, CalGray, CalRGB, Indexed) et 3.15 (Lab, Separation, DeviceN et leurs fonctions de teinte), sans gestion de couleur. Restent signalés (`ColorSpace`) : les images en Lab, Separation ou DeviceN, et un Indexed sur l'un d'eux | `ColorSpace` 392 → 91 → 16 et 417 → 75 → 12 premières pages (2026-11-16) | Images en Separation et DeviceN (évaluation par pixel, avec une table pour une composante de 8 bits), si la mesure le justifie ; gestion de couleur (profils ICC), à décider |
| Relevée en 3.12, signalée depuis 3.13 : les annotations et les champs de formulaire ne sont pas dessinés (`Annotation`) | 42 et 40 premières pages (2026-11-15) | Dessiner leurs apparences en P5 |
| Relevée en 3.12, signalée depuis 3.13 : une image JPEG en Indexed, Separation, DeviceN ou Lab n'est pas dessinée (`ColorSpace`) ; `Default*` n'est pas appliqué aux images (dessinées, `ColorSpace`) ; le `/OC` d'une image n'est pas suivi (dessinée, `OptionalContent`) | Avec eux, `ColorSpace` passe de 380 à 392 et de 409 à 417 premières pages, `OptionalContent` de 54 à 56 sur SafeDocs | Les prendre en charge avec les espaces de couleur (3.14 et suivantes) et le contenu optionnel |

## P2 terminée (`v0.1.0-alpha.3`)

Ordre (ADR-0003, amendé par ADR-0004) : 1. Flate ; 2. flux xref et flux d'objets ;
3. interpréteur minimal ; 4. tuiles, cache et IPC jusqu'à la première page (fait) ;
**4 bis. reconstruction de la xref (ADR-0004)** (fait) ; 5. polices, texte et images
(ADR-0005), par étapes : 5.1 décompte des types de police dans `survey` (fait) ; 5.2 polices
incluses TrueType (5.2a, fait) et CFF (5.2b, fait) ; 5.3 polices non incluses (jeu OFL : 5.3a
Helvetica, Times, Courier et polices nommées, fait ; 5.3b Symbol et ZapfDingbats, fait) ; 5.4 images Flate et DCT (5.4a
XObjects image, fait ; 5.4b images en ligne, fait ; 5.4c XObjects de formulaire et groupes
de transparence, fait) ;
5.5 polices Type 3, fait.

### Limites ouvertes à la fin de P2

Constatées à la tranche 4 (`docs/perf/2026-11-08-p2-first-page.md`). Chacune est soit
levée par une tranche, soit reportée explicitement avec sa raison ; aucune ne disparaît
de cette liste sans preuve. Celles qui restent demandent une machine macOS ou Linux : elles
sont publiées comme limites connues de `v0.1.0-alpha.3` et restent à lever.

| Limite | Pourquoi elle compte | Sortie attendue |
| --- | --- | --- |
| Temps côté WebView mesuré sous Windows seulement (panneau « Mesures », 2026-11-11, PDF de 11 pages avec texte, images et formulaires) : dessin d'une tuile 0,1 ms en médiane, réception 5 ms en médiane et 9,9 ms au p95 en défilement | Le transport d'ADR-0003 est confirmé sous Windows, pas encore sur les autres OS | Même relevé sous macOS et Linux |
| Mémoire en défilement mesurée sous Windows seulement, à la main (environ 173 Mo, 2026-11-11). Au repos et avec un document ouvert, le workflow `memory` mesure les trois OS (voir `docs/QUALITY.md`) | Critère de sortie de P2 (« mémoire/worker contrôlés ») | Même relevé en défilement sous macOS et Linux |
| Glisser-déposer vérifié sous Windows seulement (procédure de `docs/QUALITY.md`, 2026-11-11) ; le choix du fichier est testé en Rust | Seul chemin d'ouverture depuis l'UI | Même procédure sous macOS et Linux |

Levées le 2026-11-11, preuves dans `docs/QUALITY.md` : jobs CI ARM64 et macOS Intel (#52) ;
démarrage du worker en médiane de 20 exécutions (#53) ; affichage sous macOS et Linux, vu
sur les captures du workflow `screenshot` (#54) ; mesures « page simple et Windows
seulement », remplacées par le relevé sur corpus (`tile_cost`), le panneau « Mesures » et le
workflow `memory` sur les trois OS ; ouverture depuis le Finder (#55), vue sur la capture
macOS après `open -a`, le geste à la main restant au point 6 de la procédure.

### Limites reportées en P3 (décision du 2026-11-11, remesurées en 3.11 le 2026-11-15)

Elles ne bloquent pas un lecteur utile : chacune donne un résultat explicite (image signalée
`Image`, page refusée par `limit/DecodedBytes`) ou un coût mesuré acceptable, jamais un rendu
faux présenté comme exact. Elles restent suivies ici jusqu'à leur tranche.

| Limite | Pourquoi elle compte | Sortie attendue |
| --- | --- | --- |
| Chaque tuile réinterprète tout le contenu de la page (le cache par document évite seulement de relire et décoder) | Avec le cache, une première page en tuiles de 256 px à l'échelle 2 prend 89 ms en médiane, 362 ms au p95 et 4,4 s au plus, soit 3,4 fois la page entière (23 ms en médiane). Une passe d'interprétation, que chaque tuile paie, coûte au plus 13,6 ms sur les pages les plus lentes (SafeDocs, 2026-11-15, test `tile_cost`) | Liste d'affichage par page si une page montre une passe qui retarde ses tuiles visibles, mesurée de même |
| Une image en ligne est décodée à chaque dessin, même dans un rendu (pas d'identifiant d'objet pour la garder) | Coût sur les pages qui répètent une image en ligne ; compris dans la passe d'interprétation ci-dessus, il ne ressort sur aucune page mesurée (2026-11-15) | Images en ligne clées par contenu si la mesure le justifie |
| Une image CCITTFax dont une rangée est abîmée n'est pas dessinée du tout : `/DamagedRowsBeforeError` n'est pas suivi (Poppler et pdf.js dessinent l'image partielle) | Image numérisée abîmée absente au lieu de partielle ; non mesuré sur le corpus | Suivre `/DamagedRowsBeforeError` en peignant les rangées sûres, si la mesure en trouve |
| Images en JBIG2 ou JPX signalées (`Image`), pas dessinées (CCITTFax décodé depuis la tranche 3.9) | 48 et 63 premières pages du corpus gardent une image non dessinée, pour un de ces filtres ou une taille au-delà de `max_image_pixels` (2026-11-14 ; 113 et 140 avant CCITTFax) | Filtres de P3 (ADR-0003, amendé), avec leurs propres oracles et fuzzing |
| Une page dont les images dépassent ensemble le budget de données décodées (256 Mio) n'est pas rendue | 2 premières pages SafeDocs et 2 UNSAFE (`limit/DecodedBytes`, 2026-11-15), chacune avec 2 à 7 images de 8,7 à 32,9 Mpx | Décodage réduit à la résolution affichée, si la mesure en trouve davantage ; pas de budget propre aux images (voir les raisons) |

Raisons du report, dans l'ordre du tableau :
- réinterprétation par tuile : une optimisation, pas un manque ; 89 ms en médiane pour une
  première page avec le cache, et une réception de 5 ms en médiane en défilement sous Windows.
  En 3.11 : 2,6 ms de plus par tuile en moyenne, une passe de 13,6 ms au plus, donc une tuile
  visible sous 20 ms ; une liste d'affichage (état graphique, glyphes et images à garder par
  page, bornés en mémoire) ne se justifie pas encore ;
- images en ligne redécodées : aucun coût mesuré ne le justifie encore (3.11 : compris dans
  la passe d'interprétation, sans page qui ressorte) ;
- JBIG2 et JPX : filtres de P3 par ADR-0003 ; chacun demande son oracle et son fuzzing
  (CCITTFax fait en 3.9) ;
- pages au-delà de 256 Mio d'images décodées : 4 premières pages du corpus, refusées
  explicitement ; un décodage à la résolution affichée est une tranche à part entière. En
  3.11 : le rasteriseur garde chaque image dessinée jusqu'à la fin de la page, donc le budget
  cumulatif approche le pic réel ; un budget par image relâcherait la borne de la mémoire
  réellement tenue par une page. Ne facturer que le RGBA gardé rendrait 3 de ces 4 pages, mais
  demande de restituer un budget aujourd'hui monotone : pas pour 0,07 % des premières pages.

## Première tranche concrète pour Claude

Lire ce kit puis proposer l'ADR P0 avec les versions vérifiées à ce moment-là.
Créer le workspace, un shell Tauri minimal et un worker qui traite une requête bornée
et annulable sans lire de PDF réel. Ajouter CI Rust/UI/native, fixture synthétique et
mesure du plancher mémoire. Ne pas commencer par une barre d'outils Adobe complète.

## Décisions à enregistrer en P0

OS/architectures minimums, versions Rust/Node/Tauri/React, politique de codecs et licences,
protocole et isolation du worker par OS, profil PDF initial et rendu des polices,
machines de mesure et budgets. Utiliser `docs/templates/ADR.md` et marquer les choix
proposés comme tels jusqu'à validation dans le dépôt.

L'application vise les fonctions basiques d'un éditeur PDF progressivement. La fidélité
des polices, transparences, couleurs, impressions et éditions de texte est un chantier
à part entière ; aucune phase courte ne garantit une parité générale avec Acrobat.
