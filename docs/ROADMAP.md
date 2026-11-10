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

## P2 en cours

Ordre (ADR-0003, amendé par ADR-0004) : 1. Flate ; 2. flux xref et flux d'objets ;
3. interpréteur minimal ; 4. tuiles, cache et IPC jusqu'à la première page (fait) ;
**4 bis. reconstruction de la xref (ADR-0004)** (fait) ; 5. polices, texte et images
(ADR-0005), par étapes : 5.1 décompte des types de police dans `survey` (fait) ; 5.2 polices
incluses TrueType et CFF ; 5.3 polices non incluses (jeu OFL) ; 5.4 images Flate et DCT ;
5.5 polices Type 3.

### Limites ouvertes à lever avant la fin de P2

Constatées à la tranche 4 (`docs/perf/2026-11-08-p2-first-page.md`). Chacune est soit
levée par une tranche, soit reportée explicitement avec sa raison ; aucune ne disparaît
de cette liste sans preuve.

| Limite | Pourquoi elle compte | Sortie attendue |
| --- | --- | --- |
| Temps côté WebView non mesuré (`ipc::Response` → `ArrayBuffer` → `putImageData`) | Le transport d'ADR-0003 n'est ni confirmé ni infirmé | Mesure dans l'application réelle, médiane et dispersion, sur les trois OS |
| Mémoire application + worker + WebView non mesurée pendant la navigation | Critère de sortie de P2 (« mémoire/worker contrôlés ») | `scripts/measure-memory.mjs` étendu à l'ouverture et au parcours de pages |
| Démarrage du worker mesuré une seule fois (491 ms) | Une mesure n'est pas une médiane | Démarrage à froid, plusieurs exécutions |
| Worker planté non relancé | L'utilisateur doit redémarrer l'application | Relance bornée par le shell, document rouvert, erreur explicite au-delà |
| Glisser-déposer non testé automatiquement | Seul chemin d'ouverture depuis l'UI | Test natif du shell ou procédure manuelle consignée par OS |
| Affichage non vérifié sur macOS et Linux | La CI compile et teste, elle ne regarde pas l'écran | Capture vérifiée sur chaque OS |
| Ouverture depuis le Finder (macOS) non prise en charge | Usage courant sur macOS | `RunEvent::Opened` géré, ou limite publiée |
| Navigation page par page, pas de défilement continu | « UI réactive » et virtualisation des pages | Défilement virtualisé, tuiles invisibles annulées |
| Chaque tuile réinterprète tout le contenu de la page | Coût multiplié par le nombre de tuiles sur une page lourde | Mesure sur pages lourdes, puis cache de la liste d'affichage si nécessaire |
| Mesures sur page simple et Windows seulement | Pas représentatives | Pages du corpus avec texte et images, trois OS |
| Pas de jobs CI ARM64 ni de build macOS x86_64 | Annoncés par ADR-0003 | Jobs ajoutés ou report motivé |

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
