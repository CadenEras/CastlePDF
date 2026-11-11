# Mémoire et performances

Les chiffres ci-dessous sont des **objectifs de départ proposés**, à calibrer en P0,
pas des résultats mesurés ni une promesse liée à Tauri. Distinguer temps de démarrage,
temps jusqu'à la première page utile, fluidité et empreinte totale.

## Protocole P0

Choisir une machine de référence par OS ; noter CPU, RAM, GPU, OS, version WebView,
toolchain, build release et profil d'alimentation. Créer un corpus versionné : texte
10 pages, scan 100 pages, document complexe avec polices/transparence, gros document
1 000 pages, cas hostile borné. Publier les caractéristiques réelles et hashes des
fichiers autorisés, sans forcer ces tailles artificiellement sur tous les cas.

Mesurer une série froide et une série chaude séparément, avec au moins 10 répétitions
pour la tendance et davantage pour un percentile fiable. Consigner médiane, dispersion,
p95 si l'échantillon le permet, maximum et pic mémoire. Comparer sur le même matériel.
Les benchmarks CPU partagés de CI ne doivent pas servir de seul arbitre de latence.

## Objectifs initiaux

| Indicateur | Cible exploratoire | Décision |
| --- | --- | --- |
| Première page texte 10 pages | médiane < 1 s, build release chaud | Calibrer P0/P2 selon machine |
| Réponse UI à annulation | < 100 ms avant retour visuel | L'arrêt réel du worker est mesuré séparément |
| Cache de tuiles décodées | 128 MiB maximum initial | Budget explicite, inclure buffers en vol hors cache |
| Cache de rendu par document (worker) | 64 MiB par document (`RenderCache::DEFAULT_BYTES`), 4 documents au plus | Polices, contours, images décodées, formulaires, contenu des pages ; vidé au début d'un rendu s'il est plein aux trois quarts |
| Document très long | pas de rendu anticipé de toutes les pages | Tester résidence par fenêtre visible |
| Régression temps/mémoire | > 10 % déclenche investigation | Confirmer bruit et même environnement |
| Fermeture après 20 cycles | pas de croissance persistante expliquée seulement par les cycles | Distinguer cache borné et fuite |

Fixer un budget total RSS/privé seulement après mesure du plancher WebView sur les trois
OS. Le budget de cache n'est pas le budget total de l'application. Pour chaque tâche,
compter images décodées, surfaces temporaires, polices, IPC et textures GPU ; un rendu
peut dépasser son cache même si le cache lui-même respecte 128 MiB.

## Mesures et décisions

Observer shell, worker et processus WebView. Documenter RSS, mémoire privée/engagée et
PSS lorsqu'il existe ; ne pas sommer des valeurs partageant les mêmes pages sans
expliquer la méthode. Les outils et métriques diffèrent selon OS. Mesurer aussi le
heap Rust, allocations temporaires, handles et mémoire GPU quand accessible.

Lire les objets et flux à la demande ; cache LRU en octets avec éviction et annulation
des rendus invisibles. Borner la file et la concurrence ; libérer les textures et handles
après fermeture. Une empreinte dépendant du nombre de pages visitées doit rester bornée
par les caches, pas croître jusqu'à la fermeture du document.

Rapport par changement critique : corpus, commande/harness effectif, matériel, baseline,
mesures avant/après, dispersion, limites de sécurité inchangées et éventuel compromis de
qualité. Ne jamais compenser une régression en supprimant les limites ou en dégradant
silencieusement le rendu.

## Relevés

Deux catégories, toujours indiquées avec le relevé :

- **Référence** : machine physique décrite, OS natif, protocole ci-dessus. Seule base pour
  comparer des tranches entre elles ou fixer un budget.
- **Indicatif** : runner CI partagé (workflow `memory`, déclenché à la main ou par une
  modification du script) ou Linux sous WSL2. Utile pour l'ordre de grandeur et pour
  repérer un écart grossier ; affichage, GPU et matériel sont virtualisés ou partagés.

| Date | Catégorie | Portée | Relevé |
| --- | --- | --- | --- |
| 2026-11-05 | Référence | Plancher au repos, Windows 11 x64 | [P0 Windows](perf/2026-11-05-p0-windows.md) |
| 2026-11-05 | Indicatif | Plancher au repos, Ubuntu 22.04 sous WSL2 | [P0 WSL2](perf/2026-11-05-p0-wsl2.md) |

macOS et Linux n'ont pas encore de relevé de référence : il faut une machine physique
pour chacun. Les relevés indicatifs du workflow `memory` sont dans le résumé de chaque
exécution GitHub Actions.

### Linux sous WSL2 (indicatif)

Depuis la distribution Ubuntu de WSL2 avec WSLg, en clonant le dépôt dans le système de
fichiers Linux (`~/`), pas sous `/mnt/` :

```sh
sudo apt-get update
sudo apt-get install -y build-essential pkg-config curl libwebkit2gtk-4.1-dev libxdo-dev \
  libayatana-appindicator3-dev librsvg2-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none
. "$HOME/.cargo/env"
# Node 24 : via nvm ou le paquet officiel ; vérifier `node --version`.
git clone https://github.com/CadenEras/CastlePDF.git && cd CastlePDF
rustup toolchain install   # version de rust-toolchain.toml
npm --prefix apps/desktop ci
npm --prefix apps/desktop run tauri -- build --no-bundle -- --locked
node scripts/measure-memory.mjs 10 5
```

Consigner la version de WSL (`wsl --version` côté Windows), la distribution et la mémoire
allouée à la VM avec le résultat.
