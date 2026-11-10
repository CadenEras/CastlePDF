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

Le fuzzing de la cible `repair` (600 s, environ 2,5 millions d'exécutions, WSL) n'a trouvé
aucun crash. Sur les fixtures abîmées, qpdf 10.6 reconstruit la même liste d'objets quand
il trouve un trailer. Sans trailer, il refuse le fichier là où CastlePDF prend le dernier
catalogue. Il indexe aussi un en-tête `10 0 obj` placé dans les données d'un flux à
`/Length` indirecte, que CastlePDF saute volontairement (`header-in-stream.pdf`).

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
