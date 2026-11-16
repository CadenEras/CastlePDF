# ADR-0008 — Écriture et opérations sur les pages (P4)

Statut : accepté le 2026-11-16
Date : 2026-11-16
Responsable : Mélissa Gries (maintenance du dépôt)

## Contexte et contraintes

P3 s'est terminée avec `v0.1.0-alpha.4`, complétée par `v0.1.0-alpha.5`. CastlePDF lit et
affiche, mais n'écrit rien : aucune crate `pdf-write`, aucune commande de sauvegarde. Le critère
de sortie de P4 (`docs/ROADMAP.md`) est : « réouverture indépendante, erreurs d'écriture sans
perte, propriétés de round-trip et undo/redo », pour les opérations rotation, réordonnancement,
suppression, fusion et découpe de pages.

Contraintes du dépôt :
- **ne jamais écraser le document source pendant une lecture ou une sauvegarde échouée**
  (`CLAUDE.md`) ; `docs/SECURITY.md` prévoit qu'en P4 la sauvegarde se fasse sous un nouveau
  nom, de façon transactionnelle (fichier temporaire sur le même volume, puis remplacement), sans
  suivre un lien symbolique inattendu ;
- l'écriture PDF est développée dans le projet (`docs/ARCHITECTURE.md`) ; `pdf-write` dépend du
  modèle (`pdf-document`, `pdf-syntax`), jamais de Tauri ni du rendu ;
- toute entrée reste non fiable : un document lu, puis réécrit, garde ses budgets ; la sortie
  est bornée en taille et la tâche annulable ;
- aucune capacité de sécurité (caviardage, signature) n'est annoncée : une suppression de page
  ne garantit pas que son contenu a disparu du fichier tant que ce n'est pas vérifié.

Ce qui existe : lecture stricte et réparée (xref classiques et flux, flux d'objets, mises à jour
incrémentales), résolution d'objets bornée, modèle de pages avec attributs hérités
(`MediaBox`, `CropBox`, `Rotate`, `Resources`), worker et protocole versionné, rendu par page.
Le chiffrement est refusé à l'ouverture, donc aucun document chiffré n'est à réécrire.

## Options

### Forme du fichier écrit

1. **Mise à jour incrémentale** : ajouter à la fin du fichier un nouvel arbre des pages et les
   objets changés. Rapide et fidèle aux octets d'origine (une signature existante peut rester
   vérifiable), mais une page supprimée reste dans le fichier et récupérable, une fusion doit de
   toute façon copier les objets d'un autre fichier, et un fichier réparé ne peut pas recevoir de
   mise à jour sûre (sa table d'origine est fausse).
2. **Réécriture complète** des objets atteignables depuis le nouveau catalogue, renumérotés,
   avec une table xref classique. Le contenu supprimé n'est plus écrit, les fichiers réparés
   ressortent sains, fusion et découpe suivent le même chemin. Plus de code (sérialisation de
   tous les types d'objets) et une signature existante est invalidée, ce qui doit être dit.
3. **Les deux dès P4** : double surface de tests pour un besoin (garder une signature) qui
   appartient à P5–P6.

### Ce que l'écriture conserve

Les données de niveau document qui désignent des pages (signets `/Outlines`, destinations
nommées, liens d'annotation, formulaire `/AcroForm`, arbre de structure `/StructTreeRoot`,
`/OpenAction`) deviennent fausses quand des pages disparaissent ou changent d'ordre. Les
réécrire correctement est un chantier à part entière ; les copier telles quelles produit des
références vers des pages absentes.

### Édition et annulation

Modifier le modèle en place à chaque opération, ou décrire le document voulu comme un **plan
de pages** (liste ordonnée de « page n du document d, rotation r ») appliqué seulement à la
sauvegarde. Le plan est une petite valeur : annuler et rétablir reviennent à garder la liste
des plans précédents.

## Décision

Décision (acceptée le 2026-11-16) : **option 2, réécriture complète**, dans une nouvelle crate `pdf-write`, à partir
d'un **plan de pages**.

1. **Sérialisation.** Tous les objets PDF (nombres, chaînes en forme littérale ou hexadécimale,
   noms échappés, tableaux, dictionnaires, références, flux) écrits par le projet, avec un en-tête
   `%PDF-x.y` (la version la plus haute des sources, au moins 1.4), une table xref classique et
   un trailer avec `/ID`. Les données de flux sont copiées **sans les décoder ni les
   réencoder** (filtres et `/Length` conservés) ; les flux d'objets et flux xref des sources sont
   lus mais ne sont pas réécrits comme tels (P4 n'écrit ni flux d'objets ni flux xref : fichiers
   un peu plus grands, format le plus largement lu).
2. **Copie des objets.** Depuis le nouveau catalogue, chaque objet atteint est copié une fois par
   document source, renuméroté, sans boucle (références circulaires suivies une seule fois),
   sous le budget de la tâche. Chaque page est réécrite avec ses attributs hérités rendus
   explicites, sa rotation appliquée à `/Rotate`, et sans `/Parent` d'origine ; l'arbre des pages
   écrit est plat.
3. **Ce qui n'est pas conservé en P4, et le dit.** Signets, destinations nommées, `/AcroForm`,
   arbre de structure, `/OpenAction`, pièces jointes et `/Names` ne sont pas copiés ; les
   annotations de lien vers une page absente perdent leur destination. Avant la sauvegarde, la
   commande renvoie la liste de ce qui sera perdu, affichée dans l'application. Une signature
   numérique présente est signalée comme invalidée.
4. **Sauvegarde.** Uniquement « Enregistrer sous » vers un chemin **différent de la source**
   (vérifié après résolution des liens), choisi par l'utilisateur dans le shell. Le fichier est
   écrit dans un temporaire du même dossier, vidé sur disque, **relu par notre propre lecteur
   strict** (nombre de pages, chaque page ouvrable) avant d'être renommé à sa place ; tout échec
   laisse la source et la destination intactes et supprime le temporaire. Remplacer le fichier
   source n'est pas proposé en P4.
5. **Plan, annulation, affichage.** Les opérations (rotation, suppression, déplacement, insertion
   des pages d'un autre document ouvert, extraction d'une sélection) produisent un nouveau plan ;
   l'interface garde l'historique pour annuler et rétablir. Le visualiseur affiche le plan (pages
   dans l'ordre, rotation appliquée) avant toute écriture.

Ordre des tranches proposé :
- 4.1 sérialiseur et propriété aller-retour sur les objets (lecture ∘ écriture = identité) ;
- 4.2 réécriture d'un document inchangé : `castle-pdf rewrite`, contrôle `qpdf --check`, mêmes
  pages et même rendu au pixel près sur le corpus ;
- 4.3 plan de pages : rotation, suppression, réordonnancement (`castle-pdf pages`) ;
- 4.4 fusion et découpe (plusieurs sources) ;
- 4.5 sauvegarde transactionnelle, protocole du worker et commande Tauri « Enregistrer sous » ;
- 4.6 interface : vignettes, opérations, annuler et rétablir, liste de ce qui sera perdu ;
- 4.7 signets et liens recalculés pour les pages conservées, si la mesure le justifie.

## Conséquences et validation

- **Oracles.** `qpdf --check` sur chaque fichier écrit ; `pdftoppm` et notre rendu identiques
  avant et après réécriture pour chaque page conservée ; `pdfinfo` pour le nombre de pages et les
  boîtes ; réouverture par notre lecteur strict (sans réparation).
- **Propriétés.** Pour les objets : écrire puis relire rend l'objet (`proptest`). Pour les
  documents : réécrire un fichier réécrit donne les mêmes octets (déterminisme) ; un plan
  identité garde pages, boîtes, rotations et rendu.
- **Fuzzing.** Nouvelle cible `write` : document arbitraire réécrit, la sortie doit se rouvrir en
  lecture stricte.
- **Mesure.** Sur le corpus : part des fichiers réécrits avec succès, rouverts strictement et
  rendus à l'identique ; taille de sortie rapportée à l'entrée ; temps.
- **Sauvegarde.** Tests d'échec : disque plein simulé, permission refusée, destination
  verrouillée, interruption, destination égale à la source (refusée), lien symbolique ; la source
  reste identique octet pour octet.
- **Limites.** Taille de sortie bornée (par défaut celle des entrées plus une marge), budgets et
  annulation pendant l'écriture ; aucun `unsafe`.
- **Retour arrière.** `pdf-write` est une crate séparée ; sans elle, l'application reste un
  lecteur. Une mise à jour incrémentale, l'écriture de flux d'objets ou la conservation des
  signets pourront s'ajouter par un ADR ultérieur sans changer ce contrat.
