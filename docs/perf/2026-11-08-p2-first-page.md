# Première page en tuiles — Windows 11 x64 (2026-11-08)

Premières mesures demandées par ADR-0003 pour la tranche 4 de P2 : coût d'une tuile à
travers l'IPC worker, et temps jusqu'à la première page à travers le shell. Ce sont des
points de départ sur une page simple, pas des budgets.

## Protocole

- Build release, Rust 1.99.1, avec le code de la branche `feat/desktop-first-page` (PR de la
  tranche 4, partie 3). Même machine que `2026-11-05-p0-windows.md` : Windows 11
  Professionnel 10.0.26300 x64, Intel Core i7-12700KF, 64 GiB.
- Page 1 de `tests/corpus/public/shapes.pdf` : 200 × 200 points, quelques chemins, deux
  flux de contenu, sans texte ni image.
- Tuile à travers le tube du worker :
  `cargo test -p pdf-worker --release --test documents -- --ignored --nocapture`.
  Tuile pleine de 256 × 256 à l'échelle 4. On mesure 20 allers-retours (rendu, encodage,
  trame, décodage) et on retient la médiane.
- Première page à travers le shell :
  `cargo test -p castle-pdf-desktop --release -- --ignored --nocapture`, avec le worker
  déjà démarré. Le temps couvre `open` puis `page_size`, puis les 16 tuiles d'une page de
  1000 × 1000 pixels (échelle 5) passées par la file du shell, le worker et le cache. La
  même page est ensuite redemandée, servie depuis le cache. 3 exécutions.

## Résultats

| Mesure | Valeur |
| --- | --- |
| Tuile 256 × 256 à travers le tube du worker | médiane 0,54 ms (min 0,48, max 0,98) |
| Première page, de l'ouverture à la dernière tuile (16 tuiles, 1000 × 1000) | 13,9 / 15,1 / 15,8 ms |
| Même page depuis le cache du shell | 12 à 32 µs |
| Démarrage du worker inclus (première exécution, non isolée) | 491 ms |

## Limites

- **WebView non mesurée.** Le passage par `tauri::ipc::Response` vers un `ArrayBuffer`,
  puis `putImageData`, n'est pas mesuré. L'affichage a été vérifié visuellement dans
  l'application réelle (build debug), sans chronométrage. Le transport retenu par ADR-0003
  n'est donc ni confirmé ni infirmé par ces chiffres.
- **Page simple.** Une page avec du texte, des images ou beaucoup de chemins coûtera bien
  plus cher. Chaque tuile réinterprète tout le contenu de la page.
- **Démarrage du worker.** Les 491 ms comprennent le lancement du processus et la poignée de
  main, sur une seule mesure. À reprendre avec `scripts/measure-memory.mjs` et un démarrage
  à froid.
- **Mémoire non mesurée** pendant le défilement : il n'y a pas encore de défilement continu
  (navigation page par page).
- **Windows seulement.** Mesures macOS et Linux à faire.
