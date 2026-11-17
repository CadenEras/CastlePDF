# ADR-0009 — Dialogue natif « Enregistrer sous » (tranche 4.5c)

Statut : accepté le 2026-11-17
Date : 2026-11-17
Responsable : Mélissa Gries (maintenance du dépôt)

## Contexte et contraintes

ADR-0008 (point 4) prévoit un « Enregistrer sous » vers un chemin choisi par l'utilisateur dans
le shell, différent de la source. Le moteur (`pdf_write::save_as`, tranche 4.5a) et le worker
(`SaveDocument`, protocole v8, tranche 4.5b) existent ; il manque le choix du chemin.

L'application n'a aujourd'hui aucun dialogue de fichiers : un document s'ouvre par
glisser-déposer ou par le système (`opening.rs`), et la capability de la fenêtre principale ne
donne à l'UI aucune permission de fichier, de dialogue, de shell ou de réseau (« l'UI ne nomme
jamais un chemin », `docs/SECURITY.md`). Toute extension des permissions Tauri ou dépendance
native demande un ADR (`.claude/rules/architecture.md`).

## Options

1. **`tauri-plugin-dialog`, appelé seulement depuis Rust.** Greffon officiel de Tauri 2
   (MIT ou Apache-2.0), qui enveloppe `rfd` et gère son fil d'exécution avec la boucle
   d'événements de Tauri (macOS exige le fil principal). Enregistré dans le shell sans aucune
   permission `dialog:*` donnée à la fenêtre : le JavaScript ne peut pas l'appeler.
2. **`rfd` directement.** Une couche de moins, mais la synchronisation avec la boucle
   d'événements (fil principal sous macOS, portail XDG sous Linux) serait à notre charge.
3. **Dialogue dessiné dans l'UI.** L'UI devrait parcourir le système de fichiers : contraire au
   principe « l'UI ne nomme jamais un chemin ».

## Décision

Décision (choisie par la responsable le 2026-11-17) : **option 1**, `tauri-plugin-dialog` 2.8.4,
appelé seulement depuis le shell Rust.

- Une commande applicative `save_as` (permission `allow-save-as`, seule ajoutée à la
  capability) reçoit de l'UI le plan (identifiants opaques de documents, pages, rotations),
  jamais un chemin. Le shell ouvre le dialogue natif (filtre PDF, nom proposé d'après le
  document de la première page, dossier de celui-ci), puis valide le chemin rendu : absolu,
  extension `.pdf` ajoutée si absente, différent (liens résolus) de chaque document ouvert. Il
  envoie `SaveDocument` au worker et rend à l'UI le résultat (pages, taille, ce qui n'est pas
  gardé, pages aux ressources gardées entières) ou l'annulation du dialogue.
- Aucune permission `dialog:*`, `fs:*` ni `shell:*` n'est donnée à la fenêtre ; la capability
  et sa description le disent.
- Remplacer un fichier existant passe par la confirmation du dialogue natif ; le worker refuse
  de toute façon le fichier d'un document ouvert (`SameAsSource`).

## Conséquences et validation

- Surface native ajoutée : `rfd` 0.17.2 et, sous Linux, GTK (déjà présent par WebKitGTK) ;
  sous Windows, les API de dialogue COM ; sous macOS, `NSSavePanel`. Le greffon tire aussi la
  crate `tauri-plugin-fs` 2.7.1, qui n’est pas enregistrée : aucune de ses commandes n’existe
  dans l’application. `cargo deny check` passe (avis, licences, sources) avec ces ajouts.
- Tests : validation du chemin rendu (extension, chemin relatif refusé, document ouvert
  refusé) en tests unitaires du shell ; parcours complet (dialogue, enregistrement,
  réouverture) vérifié à la main sous Windows, comme les tranches d'interface précédentes, le
  dialogue natif n'étant pas automatisable en CI.
- L'interface (tranche 4.6) appelle `save_as` ; avant elle, la commande n'est reliée à rien,
  et le parcours à la main se fait avec elle.
- Une seule sauvegarde à la fois : une seconde demande pendant un dialogue ouvert est refusée
  (`busy`).
