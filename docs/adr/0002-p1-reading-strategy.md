# ADR-0002 — P1 : source d'entrée, construire ou réutiliser, oracles et fuzzing

Statut : accepté le 2026-11-06
Date : 2026-11-06
Responsable : Mélissa Gries (maintenance du dépôt)

## Contexte et contraintes

P1 livre tokenizer, objets, xref classique, pages et CLI de diagnostic
(`docs/ROADMAP.md`). Avant d'écrire le lexer, quatre choix conditionnent tout le reste :
comment lire le fichier, quelles couches écrire nous-mêmes, contre quoi comparer nos
résultats et comment chercher les défauts sur entrée arbitraire. Contraintes : `unsafe`
interdit dans le projet (ADR-0001), aucune dépendance dans `crates/*` sans ADR,
licence `Apache-2.0 OR MIT`, CI gratuite sur runners standards.

## Options

- **Source** : fichier entier en mémoire avec taille maximale (simple, copie unique,
  coût mémoire égal à la taille du fichier) ; `mmap` (pas de copie, mais `unsafe` et
  comportement indéfini si le fichier change sous nos pieds) ; lecture par plages à la
  demande (mémoire minimale, complexité et appels système à chaque accès).
- **Fichiers abîmés** : réparation dès P1 (reconstruction de la xref par balayage) ou
  refus explicite, réparation plus tard comme capacité séparée et testée.
- **Construire ou réutiliser** : tout écrire (contrôle total, années de travail sur des
  domaines non spécifiques au PDF) ou réutiliser des briques Rust pures par couche.
- **Fuzzing** : `cargo-fuzz` (libFuzzer, Rust nightly, référence de l'écosystème) ;
  `bolero` (stable possible, moins répandu) ; tests de propriétés seuls (`proptest`,
  stable, mais exploration guidée par la couverture absente).

## Décision

| Sujet | Décision |
| --- | --- |
| Source | Fichier entier lu en mémoire sous `max_input_bytes` (nouvelle limite, 512 Mio par défaut), vérifiée pendant la lecture et, si la taille annoncée est fournie, avant toute lecture. Le parseur de structure (xref, objets) ne voit qu'un trait `Source` (longueur `u64`, lecture d'une plage bornée), introduit avec l'accès aléatoire de la tranche xref. Le lexer travaille sur une tranche d'octets fournie par la source. Amendement du 2026-11-07 : en P1, `PdfFile` demande à la source la plage entière, servie sans copie par `MemorySource` ; une source par plages imposera des lectures fenêtrées dans le lecteur de structure, sans changer le trait ni l'API publique. Pas de `mmap` |
| Offsets | `u64` partout dans le format ; conversion vers `usize` vérifiée à la frontière de la source |
| Fichiers abîmés | P1 strict : en-tête, `startxref`, tables et trailer doivent être cohérents, sinon erreur `Malformed` localisée (offset, objet). Pas de reconstruction de xref en P1 ; capacité « réparation » à part, avec son propre corpus. Amendement du 2026-11-09 : ADR-0004 ajoute la reconstruction de la xref par balayage en P2, visible (`RepairReport`) et désactivable (option `strict`) |
| Capacités absentes | Erreur `Unsupported(Capability)` explicite : xref streams, object streams, chiffrement, filtres non implémentés. Jamais de résultat partiel silencieux |
| Construire | Lexer, objets, xref, révisions incrémentales (`/Prev`), modèle de document, interpréteur de contenu, écriture : c'est la sémantique PDF, cœur du projet |
| Réutiliser (ADR dédiée par brique, à son tour) | Polices : `skrifa`/`read-fonts` (fontations, Google). Rasterisation : `tiny-skia` ou `vello_cpu`, à comparer en P2. Codecs : `zlib-rs` ou `miniz_oxide` (Flate, P3), `zune-jpeg` (DCT). Chaque brique : licence compatible, pas de code natif, limites mémoire testables, maintenance active |
| Oracles (jamais livrés) | `qpdf --check` et `qpdf --json` (Apache-2.0) en CI Linux pour la structure dès la tranche xref ; `hayro` (MIT/Apache, Rust pur) et `pdf.js` pour le rendu à partir de P2 ; `mutool` (AGPL) seulement en local. Un désaccord est analysé, pas tranché en faveur de l'oracle |
| Fuzzing | `cargo-fuzz` dans `fuzz/`, hors workspace, avec un nightly daté dans `fuzz/rust-toolchain.toml`. Une cible par couche (lexer, objets, xref, document). Job CI borné en temps (5 min par cible), sur planning hebdomadaire et sur PR touchant `crates/pdf-syntax` ou `crates/pdf-document`, non requis au départ. Graines : fixtures générées. Tout crash minimisé devient un test de régression |
| Propriétés | `proptest` (MIT/Apache, dépendance de test uniquement) : aucun panic, aucune lecture hors borne, limites respectées, aller-retour lexer → texte → lexer stable |
| Modèle de référence | ISO 32000-2:2020 pour la sémantique ; Arlington PDF Model (PDF Association) envisagé en P3 pour générer des validations de dictionnaires, pas en P1 |

## Conséquences et validation

- `ResourceLimits` gagne `max_input_bytes` ; test limite exacte acceptée, limite + 1
  refusée, capacité du tampon jamais au-delà de la limite. Sans taille annoncée, la
  croissance par doublement peut tenir brièvement l'ancien et le nouveau tampon
  (environ 1,5 × la limite) ; avec la taille du fichier, une seule allocation.
- Chaque tranche P1 livre ses cas invalides (troncature, débordement d'entier, cycle
  `/Prev`, profondeur) en fixtures générées par `scripts/make-fixtures.mjs`, documentées
  dans le manifeste ; aucun PDF privé ni corpus téléchargé pendant les tests.
- `cargo-deny` vérifie toute nouvelle dépendance, y compris de test.
- Risque : un fichier de 512 Mio occupe 512 Mio dans le worker. Accepté pour P1 ;
  la source par plages est la sortie prévue si les mesures l'exigent.
- Retour arrière : la source est derrière un trait ; l'outil de fuzzing est isolé dans
  `fuzz/` et peut être remplacé sans toucher au code du moteur.
