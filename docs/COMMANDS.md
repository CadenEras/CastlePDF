# Commandes et environnement

## Disponibles dès ce kit

Depuis sa racine, sous PowerShell, Bash ou terminal macOS/Linux :

```sh
node scripts/validate-agent-kit.mjs
node --test scripts/tests/hooks.test.mjs
```

Node 22+ suffit, sans installation npm. La fixture PDF synthétique se régénère avec
`node scripts/make-fixtures.mjs` et se vérifie avec `node --test scripts/tests/fixtures.test.mjs`.
`node --test scripts/tests/readme.test.mjs` vérifie que les badges de versions du README
correspondent aux versions fixées (toolchain Rust, Node de la CI, `package.json`, crate Tauri).
Le plancher mémoire de l'application release se mesure avec `node scripts/measure-memory.mjs`
(voir `docs/PERFORMANCE.md`). Le validateur contrôle JSON, frontmatter du
sous-ensemble utilisé, références Markdown locales et configuration des hooks. Il
n'est pas un validateur officiel du schéma Claude Code ni un interpréteur YAML général.

Pour Claude Code : `claude --version`, puis dans une session `/status`, `/permissions`,
`/hooks`, `/agents`. Les tests Node ne demandent pas de connexion Claude et ne consomment
pas l'abonnement.

## Produit (P0)

Disponibles depuis P0 et exécutées par la CI : workflow `rust` (fmt, Clippy, tests, sur
les trois OS) et workflow `desktop` (contrôles du frontend sur Ubuntu, build natif release
sans installateur sur les trois OS).
npm, un `package-lock.json` dans `apps/desktop`, sans workspace npm racine ; workspace
Cargo racine avec `apps/desktop/src-tauri` comme membre. Prérequis : rustup (la version
vient de `rust-toolchain.toml`), Node 24, dépendances système Tauri (WebKitGTK sous Linux).
`cargo test --workspace` construit aussi `pdf-worker`, utilisé par les tests du shell.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked   # inclut les doctests
npm --prefix apps/desktop ci
npm --prefix apps/desktop run lint
npm --prefix apps/desktop run typecheck
npm --prefix apps/desktop run test:unit
npm --prefix apps/desktop run build
npm --prefix apps/desktop run tauri -- build --no-bundle
cargo deny check   # licences, sources, interdictions, avis RustSec ; cargo install --locked cargo-deny
```

Scripts npm non interactifs : `lint`, `typecheck`, `test:unit`, `build`, `tauri` ;
`npm --prefix apps/desktop run tauri -- dev` lance l'application en développement. Documenter séparément les tests E2E réels et leur support par OS. Un navigateur
web de test ne remplace pas automatiquement la WebView Tauri.

Ouvrir un PDF dans l'application : le déposer sur la fenêtre (un seul fichier à la fois),
ou passer son chemin en argument de l'exécutable
(`target/debug/castle-pdf-desktop.exe <fichier.pdf>` après
`npm --prefix apps/desktop run tauri -- build --debug --no-bundle`). L'application affiche
la page ajustée à la largeur, en tuiles de 256 pixels, avec une navigation page par page.
Elle signale un rendu partiel quand la page contient du texte, des images ou d'autres
éléments encore non rendus. Le dépôt de fichier n'est vérifié qu'à la main ; l'ouverture
par argument a été vérifiée par capture d'écran sous Windows.
Mesures : `cargo test -p castle-pdf-desktop --release -- --ignored --nocapture`
(`docs/perf/2026-11-08-p2-first-page.md`).

Verrouiller une toolchain Rust exacte et les versions Node/npm utilisées en CI,
committer les lockfiles et installer les prérequis natifs Tauri pour chaque runner.
`cargo test --workspace` compile aussi le shell Tauri : une machine sans ses dépendances
peut tester le cœur avec les packages explicitement sélectionnés, sans déclarer le
workspace entier validé. Tester les features supportées dans une matrice dédiée.

## Contrôles périodiques à installer

`cargo audit` et `cargo deny check` après installation/versionnage et politique de
licences ; `npm --prefix apps/desktop audit` pour les dépendances JS. Inclure les
dépendances de build, codecs natifs et sidecars. Évaluer les alertes plutôt que lancer
un correctif automatique majeur. Une exception doit avoir motif, responsable et échéance.

Fuzzing (ADR-0002) : espace de travail séparé `fuzz/`, nightly daté dans
`fuzz/rust-toolchain.toml`, sous Linux ou macOS (libFuzzer). Depuis `fuzz/` :

```sh
rustup toolchain install                      # nightly de fuzz/rust-toolchain.toml
cargo install --locked cargo-fuzz@0.13.2
cargo fuzz run lexer -- -max_total_time=300 -rss_limit_mb=2048 -dict=dict/pdf.dict   # ou objects
```

Cibles : `lexer`, `objects`, `xref` (lecture stricte), `repair` (reconstruction, ADR-0004), `font` (programme TrueType arbitraire ; graines dans `fuzz/seeds/font`), `document`, `flate`, `render` (contenu de page arbitraire ; graines dans `fuzz/seeds/render`). Un crash est écrit dans `fuzz/artifacts/<cible>/` ; le minimiser (`cargo fuzz tmin`)
puis en faire un test de régression dans la crate concernée. La CI (`fuzz.yml`) lance
5 minutes par cible chaque semaine et sur les PR touchant le parseur.

Oracle qpdf (ADR-0002, jamais livré) : avec qpdf installé, sous Linux ou macOS,
`CASTLE_QPDF="$(command -v qpdf)" cargo test -p pdf-syntax -p pdf-document --locked --test qpdf_oracle -- --nocapture`
compare, pour chaque fixture de `tests/corpus/public` (toutes valides), les entrées xref à
`qpdf --show-xref`, chaque flux décodé à `qpdf --filtered-stream-data` et l'ordre des
pages à `qpdf --show-pages`. Sans `CASTLE_QPDF`, le test est sauté ; le workflow `oracle` l'impose.

Oracle de rendu `pdftoppm` (poppler, GPL, ADR-0003, jamais livré ni lié) : avec
poppler-utils installé, `CASTLE_PDFTOPPM="$(command -v pdftoppm)" cargo test -p pdf-render --locked --test pdftoppm_oracle -- --nocapture`
compare des pages de `shapes.pdf` à plusieurs échelles. Un pixel concorde s'il reste,
canal par canal, à 16 niveaux près de l'intervalle des pixels de l'autre image dans son
voisinage 3 × 3 (différences d'anticrénelage sur les bords), dans les deux sens ; au plus
0,01 % de pixels discordants (0 mesuré avec poppler 22.02). Des défauts contrôlés (tiret
effacé, décalage de 2 px, trou rempli) et la page DeviceCMYK, qui diffère par conception
(conversion ISO 32000-2 10.4.2.4 contre approximation d'un profil d'impression par
poppler), doivent être détectés : c'est le contrôle de sensibilité du critère. Le
workflow `oracle` l'impose.

## CLI de diagnostic `castle-pdf` (crate `pdf-cli`)

Interface effective, relevée sur `--help` :

```sh
cargo run -p pdf-cli -- inspect <fichier>   # castle-pdf inspect <fichier>
cargo run -p pdf-cli -- render <fichier> <page> <sortie.ppm> [--scale <facteur>]
cargo run -p pdf-cli -- survey <répertoire> [--timeout <secondes>] [--jobs <n>] [--paths]
cargo run -p pdf-cli -- probe <fichier>       # une étape de survey
cargo run -p pdf-cli -- --help                # ou -h
cargo run -p pdf-cli -- --version
```

`inspect` affiche la structure (taille, version, `startxref`, révisions et sections xref,
objets en usage/libres, clés standard du trailer (les autres sont seulement comptées),
racine, pages avec objet, MediaBox, CropBox et rotation, puis une ligne `repair:` : `none`,
ou le nombre d'objets indexés par balayage, les numéros trouvés plusieurs fois, l'origine
du trailer et l'erreur stricte qui a déclenché la réparation, ADR-0004), jamais le contenu du document ni
le chemin du fichier. Le rapport est écrit ligne par ligne, chaque ligne bornée (notation
scientifique au-delà de 1e15). Seul un fichier régulier est lu. La structure du fichier
est affichée même si l'arbre des pages est invalide. Codes de sortie : 0 succès,
1 document illisible (erreur sur la sortie d'erreur), 2 usage incorrect, 3 écriture du
rapport impossible (pipe fermé). Limites par défaut de `ResourceLimits`.

`render` rend une page (numérotée à partir de 1) sur fond blanc dans un nouveau fichier
PPM binaire (P6), le format de `pdftoppm` ; l'échelle 1 vaut 72 pixels par pouce. Le
fichier de sortie ne doit pas exister : l'image est écrite dans un fichier temporaire
voisin puis publiée par lien dur, ce qui n'écrase jamais un fichier et ne laisse pas
d'image partielle (le système de fichiers doit accepter les liens durs). Elle affiche la taille et les capacités
utilisées par la page mais pas encore rendues (texte, images, XObjects…). Code 1 aussi
quand le rendu échoue ou que l'image ne peut pas être créée. Limites par défaut de
`RenderLimits`.

`survey` parcourt récursivement les fichiers réguliers du répertoire (liens symboliques
non suivis) et lance pour chacun `castle-pdf probe` dans un processus enfant, tué après
`--timeout` (30 s par défaut, au plus 3600) ; `--jobs` (1 à 64, 4 par défaut) borne les
sondes simultanées, chacune pouvant tenir l'entrée entière en mémoire. Le rapport agrège :
taux d'ouverture et de rendu de la première page (échelle 1), catégories d'erreur sans
offset, capacités non rendues, crashs, timeouts, temps par fichier (création du processus
comprise). Il ne contient ni contenu ni nom de fichier ; `--paths` ajoute la liste des
fichiers non rendus, à garder local. Code 0 si aucun crash ni timeout, 1 sinon.
Un fichier ouvert par réparation (ADR-0004) est compté à part (`opened by repair`, avec
la catégorie de l'erreur stricte).
`probe` écrit une ligne `open=… pages=… render=… skipped=…`, suivie de `repaired=<catégorie>`
si la structure a été reconstruite, et `fonts=<types>` : les types des polices que déclare
la première page (`Type1`, `Type1C`, `TrueType`, `OpenType`, `CidType0C`, `CidTrueType`,
`Standard14`, `NotEmbedded`, `CidNotEmbedded`, `Type3`, `Invalid`), comptés par `survey`
(`repaired=on-lookup/…` si le fichier s'est ouvert
strictement et que seuls des objets hors de leur offset ont été retrouvés par balayage) ; code 0 même si le document est invalide. Mesure sur un corpus externe : voir `docs/QUALITY.md`.

Benchmarks, autres tests différentiels et vérifications de conformité seront ajoutés avec
leurs fixtures et outils. Ne pas inventer d'option à `castle-pdf` : relire son `--help`.
