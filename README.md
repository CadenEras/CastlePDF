# CastlePDF

[![version](https://img.shields.io/github/v/tag/CadenEras/CastlePDF?include_prereleases&sort=semver&label=version)](https://github.com/CadenEras/CastlePDF/tags)
[![rust](https://img.shields.io/github/actions/workflow/status/CadenEras/CastlePDF/rust.yml?branch=main&label=rust)](https://github.com/CadenEras/CastlePDF/actions/workflows/rust.yml)
[![desktop](https://img.shields.io/github/actions/workflow/status/CadenEras/CastlePDF/desktop.yml?branch=main&label=desktop)](https://github.com/CadenEras/CastlePDF/actions/workflows/desktop.yml)
[![agent-config](https://img.shields.io/github/actions/workflow/status/CadenEras/CastlePDF/agent-config.yml?branch=main&label=agent-config)](https://github.com/CadenEras/CastlePDF/actions/workflows/agent-config.yml)

[![Rust](https://img.shields.io/badge/Rust-1.99.1-B7410E?logo=rust&logoColor=white)](rust-toolchain.toml)
[![Node.js](https://img.shields.io/badge/Node.js-24.16.0-5FA04E?logo=nodedotjs&logoColor=white)](.github/workflows/desktop.yml)
[![Tauri](https://img.shields.io/badge/Tauri-2.12-24C8D8?logo=tauri&logoColor=white)](apps/desktop/src-tauri/Cargo.toml)
[![React](https://img.shields.io/badge/React-19.3.1-087EA4?logo=react&logoColor=white)](apps/desktop/package.json)
[![TypeScript](https://img.shields.io/badge/TypeScript-6.0.3-3178C6?logo=typescript&logoColor=white)](apps/desktop/package.json)
[![Vite](https://img.shields.io/badge/Vite-8.3.6-646CFF?logo=vite&logoColor=white)](apps/desktop/package.json)
[![Plateformes](https://img.shields.io/badge/plateformes-Windows%20%7C%20macOS%20%7C%20Linux-informational)](docs/adr/0001-p0-foundations.md)
[![Licence](https://img.shields.io/badge/licence-Apache--2.0%20OR%20MIT-blue)](#licence)

Moteur PDF écrit en Rust et application desktop hors ligne (Tauri 2, React, TypeScript)
pour Windows, macOS et Linux. Le moteur est développé dans ce dépôt et reste utilisable
sans interface. Priorités : exactitude, sécurité face aux documents non fiables,
mémoire bornée, réactivité et maintenabilité.

> [!WARNING]
> Projet en phase de fondations. **CastlePDF ne lit pas encore de PDF** : la phase
> P0 livre l'architecture, le worker, les budgets de ressources et la CI ; la lecture
> structurée arrive en P1. Aucune version n'est destinée à un usage réel.

## État

| Phase | Contenu | État |
| --- | --- | --- |
| P0 — fondations | Workspace, UI vide, protocole worker, budgets, corpus, CI trois OS | Terminée (`v0.1.0-alpha.1`) |
| P1 — lecture structurée | Tokenizer, objets, xref classique, pages, CLI de diagnostic | Terminée (`v0.1.0-alpha.2`) : xref classique, pages, CLI `inspect` |
| P2 — lecteur utile | Flate, flux xref, interpréteur, rendu tuilé, polices, images | En cours ([ADR-0003](docs/adr/0003-p2-render-pipeline.md)) : Flate, flux xref et d'objets, interpréteur graphique, première page affichée en tuiles faits ; polices et images à venir |
| P3 et suivantes | Texte, écriture, annotations, fonctions avancées | Voir la [feuille de route](docs/ROADMAP.md) |

Le support PDF sera publié sous forme de matrice de capacités. Toute capacité absente
renvoie une erreur explicite plutôt qu'un résultat approximatif.

## Architecture

```text
crates/
  pdf-syntax/      budgets, annulation, source d'entrée, tokenizer, objets, xref (tables, flux, hybrides), Flate
  pdf-document/    catalogue, arbre des pages, attributs hérités
  pdf-cli/         binaire castle-pdf : diagnostic `inspect` (structure, pages)
  pdf-worker/      processus isolé : trames bornées, protocole versionné, annulation
apps/desktop/
  src/             interface React / TypeScript, sans privilège
  src-tauri/       shell Tauri : lance le worker, commandes IPC validées côté Rust
```

Le cœur Rust ne dépend ni de Tauri, ni de React, ni de Node.js. Le traitement des
documents se fait dans un sous-processus que l'application peut annuler ou arrêter.
Détails : [architecture](docs/ARCHITECTURE.md) et
[ADR-0001](docs/adr/0001-p0-foundations.md).

## Principes de sécurité

- Chaque document, police ou image est une entrée non fiable : limites cumulatives
  vérifiées avant allocation, annulation et erreurs explicites.
- Aucun JavaScript PDF, aucun lancement de pièce jointe, aucun accès réseau implicite.
- `unsafe` interdit dans le code du projet ; permissions Tauri minimales et CSP stricte.
- Le document source n'est jamais écrasé pendant une lecture ou une sauvegarde échouée.

Voir la [politique de sécurité](docs/SECURITY.md).

## Démarrer

Prérequis : [rustup](https://rustup.rs/) (la version exacte vient de
`rust-toolchain.toml`), Node.js 24 et, sous Linux, les
[dépendances système de Tauri](https://v2.tauri.app/start/prerequisites/) (WebKitGTK 4.1).

```sh
# Contrôles Rust
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked

# Application desktop
npm --prefix apps/desktop ci
npm --prefix apps/desktop run tauri dev
```

La liste complète des commandes, dont le build release et la mesure mémoire, est dans
[docs/COMMANDS.md](docs/COMMANDS.md).

## Documentation

| Document | Contenu |
| --- | --- |
| [Feuille de route](docs/ROADMAP.md) | Phases, livrables et critères de sortie |
| [Architecture](docs/ARCHITECTURE.md) | Frontières Rust / UI / worker et contrats |
| [Décisions (ADR)](docs/adr/) | Choix structurants acceptés |
| [Commandes](docs/COMMANDS.md) | Contrôles locaux et CI |
| [Qualité](docs/QUALITY.md) | Niveaux de tests et contrôles |
| [Sécurité](docs/SECURITY.md) | Menaces PDF, Tauri, sauvegarde, dépendances |
| [Performance](docs/PERFORMANCE.md) | Budgets, mesures mémoire et relevés |
| [Git](docs/GIT.md) | Branches, commits, PR, versions et tags |
| [Journal des versions](CHANGELOG.md) | Changements par version |

## Contribuer

Les branches sont courtes et chaque changement passe par une pull request. Les titres
suivent [Conventional Commits](https://www.conventionalcommits.org/) et les PR sont
fusionnées en squash une fois la CI verte sur les trois OS. Les versions suivent
[SemVer](https://semver.org/) avec des tags annotés `vX.Y.Z` ; une préversion
`alpha` marque la fin de chaque phase. Détails dans [docs/GIT.md](docs/GIT.md).

Le dépôt inclut une configuration pour Claude Code (règles, agents, skills et hooks) ;
voir [CLAUDE.md](CLAUDE.md) et [docs/AGENT-SETUP.md](docs/AGENT-SETUP.md).

## Licence

CastlePDF est distribué sous double licence, au choix :

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT ([LICENSE-MIT](LICENSE-MIT))

Sauf mention contraire explicite, toute contribution soumise pour inclusion dans le
projet, au sens de la licence Apache-2.0, est placée sous cette même double licence,
sans condition supplémentaire.
