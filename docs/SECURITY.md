# Modèle de menace et exigences

Un attaquant fournit un PDF, une police, une image, un profil couleur, une pièce jointe
ou un chemin. Il peut chercher un crash, une consommation sans borne, une exécution,
une lecture de fichier, une sortie réseau ou une sauvegarde destructive. Un rendu
plausible peut aussi masquer une perte de contenu importante.

## Cœur et worker

Chaque budget est configurable, cumulatif et vérifié avant le travail coûteux :
octets lus/décodés, nombre d'objets/références, profondeur, pixels, allocations et tâches.
Éviter l'expansion répétée des mêmes flux via des références ; limiter les tentatives
de réparation. La lecture d'un gros fichier reste possible avec un index et des fenêtres
bornés ; la limite de mémoire ne se résume pas à une taille de fichier maximale.

Récursion, boucles de références, offsets incohérents, tailles négatives, débordements,
décompression et codecs doivent échouer proprement. Les annulations interrompent les
étapes longues ; si un codec ne peut pas être interrompu, le worker doit pouvoir être
tué sans bloquer l'application. Définir droits et limites de processus pour chaque OS.

État du worker (protocole v3) :
- le chemin d'un document est choisi par le shell, jamais transmis par l'UI ; le worker
  le lit sous `max_input_bytes`, refuse ce qui n'est pas un fichier régulier et ne
  reçoit pas plus de 4 096 octets de chemin en UTF-8 ;
- il garde au plus 4 documents ouverts, chacun entier en mémoire (ADR-0002) ;
- chaque tâche a son propre budget et son jeton d'annulation ;
- un document réparé (ADR-0004) est signalé à l'ouverture et dans chaque tuile, et l'UI
  le dit à l'utilisateur ;
- une tuile fait au plus 256 × 256 pixels, et une réponse est décodée seulement si sa
  taille correspond exactement à ses dimensions ;
- le worker n'a encore aucune restriction de droits propre à l'OS.

Polices (ADR-0005) : le programme d'une police incluse est lu par `skrifa` (TrueType,
OpenType) ou `read-fonts` (`ps::cff`, CFF nu), en Rust sans `unsafe`. La lecture du jeu de
caractères CFF est facturée au budget d'objets, glyphe par glyphe. Il est décodé sous `max_decoded_bytes`. Chaque contour de glyphe est
limité à `max_glyph_segments` segments et facturé au budget d'objets. Chacun de ses segments
compte comme une opération (`max_operations`, comme un segment tracé par un opérateur) et
est vérifié contre `max_path_work` avant d'être ajouté au chemin : une chaîne courte ne peut
pas faire allouer un chemin démesuré. L'annulation est vérifiée toutes les 4 096 lettres. Le
cache des contours d'un rendu est borné, et les échecs y sont gardés. Un nom de glyphe
absent du programme est signalé (`Text`), jamais remplacé par un autre glyphe. Une police illisible saute son
texte (signalé `Text`). Une dépendance peut toutefois paniquer : la cible de fuzzing
`font` en a trouvé une dans `read-fonts` 0.41.0, corrigée en 0.42.0. Le worker isolé reste
la protection contre ce qui n'a pas encore été trouvé.

Polices de remplacement (ADR-0005) : 13 polices OFL incluses dans le binaire. Elles ne
viennent ni du système ni du réseau, ce qui garde un rendu déterministe. Leur inventaire
(sources épinglées, sha256, licences) est dans `crates/pdf-render/fonts/README.md`, car
`cargo-deny` ne voit pas ces fichiers. Un texte dessiné avec elles est signalé, jamais
présenté comme exact.

Images (ADR-0005) :
- **taille** : largeur × hauteur est vérifiée contre `max_image_pixels` avant tout
  décodage, ce qui refuse une image déclarée géante avec peu de données sans rien allouer ;
- **budget** : les données décodées et chaque tampon de pixels sont facturés à
  `max_decoded_bytes` ;
- **JPEG** : décodé par `zune-jpeg`, sans `unsafe` (fonctions SIMD désactivées), avec des
  dimensions maximales. Seules des paires d'espaces de couleur que la bibliothèque convertit
  lui sont demandées, car une autre paire atteint un `unreachable!()` ;
- **cache** : les images décodées d'un rendu sont gardées dans la limite de 128 Mio.

Réparation de la structure (ADR-0004) : un seul balayage linéaire, jamais de retour en
arrière ; chaque en-tête d'objet est facturé à `max_objects_visited` ; les dictionnaires
lus pendant le balayage sont bornés aux octets qui précèdent le mot-clé suivant, et seuls
les 16 derniers `trailer` sont gardés. Un fichier est refusé, jamais réparé, si un trailer
examiné contient `/Encrypt` ou si un objet direct est un dictionnaire de chiffrement
(`/Filter` avec `/O` et `/U`, ou `/Recipients`) : un fichier coupé avant son trailer
perd la clé `/Encrypt`, pas ce dictionnaire. Les pertes possibles (flux d'objets illisible,
flux sans `endstream`) sont comptées dans le rapport de réparation, jamais tues. Une
relocalisation à la demande est facturée au budget de la tâche qui la déclenche ; un
balayage arrêté par une limite n'est pas gardé. Un fichier réparé n'est jamais réécrit sur sa
source ; l'écriture (P4) imposera une sauvegarde sous un nouveau nom.

Aucun JavaScript PDF, action Launch, programme joint ou accès distant implicite.
Les URL externes passent par un geste explicite, une validation de schéma et la politique
du shell. Les mots de passe ne sont ni journalisés ni persistés par défaut ; ne pas
réinventer les primitives cryptographiques.

## Tauri et UI

Appliquer le moindre privilège selon les [capabilities Tauri](https://v2.tauri.app/security/capabilities/)
et son [modèle de sécurité](https://v2.tauri.app/security/). Définir une CSP adaptée
au build réel, sans script distant, évaluation dynamique ou permissions globales de
fichiers/shell. Ne pas charger de contenu distant dans une WebView privilégiée.

Définir explicitement les permissions des commandes propres à l'application et les
inclure dans les capabilities des fenêtres concernées ; ne pas supposer que les
permissions des plugins protègent automatiquement les commandes applicatives.
Valider types, tailles, chemins et handles côté Rust. Une requête IPC bien typée dans
TypeScript peut toujours être falsifiée. Éviter que des chemins libres ou identifiants
de documents permettent de traverser un répertoire ou d'accéder à un document fermé.

État du shell (P2, tranche 4) :
- l'UI ne nomme jamais un chemin. Le shell ouvre un fichier déposé sur la fenêtre, passé
  en argument ou ouvert depuis le Finder (macOS), et n'annonce à l'UI que son nom ;
- un seul fichier à la fois : un dépôt ou une ouverture qui en nomme plusieurs n'ouvre rien.
  Depuis le Finder, seule une URL `file:` locale est acceptée ; tout autre schéma est
  ignoré, jamais chargé ;
- les commandes `page_size` et `render_tile` n'acceptent que le document courant, une page
  existante, une échelle finie dans ]0, 64] et une tuile d'au plus 256 × 256 ;
- `page_text` n'accepte que le document courant et une page existante ; la couche de texte
  rendue est bornée (`MAX_TEXT_LAYER_BYTES`, 960 Kio, puis `truncated`) et le décodage de sa
  réponse vérifie chaque nombre de lignes et de mots avant d'allouer (cible de fuzzing
  `protocol`). Le texte reste une donnée : l'UI ne l'insère que comme texte ;
- `current_document` ne rend que le dernier événement de document ;
- `begin_view` numérote les vues côté shell : une tuile ne peut citer qu'une génération
  déjà attribuée ;
- une tâche de diagnostic ne dépasse pas 60 s (unités × délai), pour ne pas occuper le
  worker ;
- aucune permission de fichier, de dialogue, de shell ou de réseau n'est accordée à la
  fenêtre ;
- la file du shell est bornée (256 tâches), et le cache de tuiles est limité à 128 Mio ;
- le canvas de l'UI est limité à 16 Mpx : une page haute et étroite est rendue à une
  échelle plus faible.

Un worker perdu (crash, protocole violé) est relancé par le shell, au plus 3 fois par
minute. La tâche qu'il exécutait échoue sans être renvoyée, puisqu'elle a pu provoquer la
perte. Le document courant est rouvert seulement si son fichier n'a pas changé (taille et
date de modification). Au-delà de la limite, toute requête échoue explicitement.

L'ouverture depuis le Finder n'existe que dans l'application empaquetée (`.app`), qui
déclare le type PDF en lecteur de rang `Alternate` : elle s'offre dans « Ouvrir avec »
sans prendre la place du lecteur par défaut.

Le texte extrait reste du texte. Nettoyer les noms d'export et interdire traversée,
écriture arbitraire et ouverture d'attachement automatique. Tester le nettoyage de
ressources à la fermeture d'un document et de la fenêtre.

## Sauvegarde et fonctions sensibles

Sauvegarde transactionnelle, même volume et politique de remplacement par OS ; pas
de promesse d'atomicité universelle sur tous les systèmes de fichiers. Une modification
simultanée du source doit être détectée ou interdite. Éviter d'écraser un fichier visé
par un lien symbolique inattendu ; tester les courses pertinentes.

Un caviardage réel exige suppression des contenus concernés, ressources et métadonnées,
réécriture complète sans anciennes révisions récupérables et tests d'extraction forensic.
Un rectangle noir ou une mise à jour incrémentale ne suffit pas.
Un dessin de signature n'est pas une signature cryptographique. Validation de certificats,
horodatage, révocation et politique de confiance forment une capacité séparée.
Ces fonctions ne sont pas commercialisées comme sûres avant leurs jalons dédiés.

## Chaîne de dépendances et distribution

Verrouiller dépendances et toolchains ; suivre RustSec/npm, licences, transitive/native,
sources de polices et codecs. Ne pas adopter automatiquement une bibliothèque à licence
incompatible. Les exceptions ont un responsable, une justification et une échéance.

En place : `deny.toml` (cargo-deny) refuse toute licence hors liste, toute source hors
crates.io et toute dépendance joker ; contrôle requis `deny` sur chaque PR. Les avis
RustSec tournent dans le workflow `advisories` (hebdomadaire, sur `main` et quand
`Cargo.lock` change), non bloquant pour les PR sans rapport. Exceptions actuelles,
toutes atteintes par le shell Tauri et jamais par `crates/*` : MPL-2.0 pour `cssparser`,
`cssparser-macros`, `dtoa-short`, `selectors` et `option-ext` ; RUSTSEC-2024-0370
(`proc-macro-error`, macro de compilation du backend GTK Linux), à revoir avec Tauri 3.
Codec dans le cœur : `miniz_oxide` 0.9.1 (Flate, `#![forbid(unsafe_code)]`, version
exacte épinglée) et sa dépendance `adler2`, sans code natif (ADR-0003) ; sortie facturée
au budget fenêtre par fenêtre. Rasterisation dans `pdf-render` : `vello_cpu` 0.3.0
(version exacte, sans `png`, `text` ni multithreading), seul module à l'appeler
`raster.rs`. Inventaire de son graphe d'exécution (17 crates, toutes sous licence de la
liste, aucun code natif ni accès réseau ; `build.rs` seulement dans `num-traits` et
`thiserror`, sondes de version du compilateur) : `vello_cpu`, `vello_common` et
`kurbo` sans `unsafe` ; `unsafe` présent dans `fearless_simd` (intrinsèques SIMD
détectées à l'exécution), `bytemuck`, `hashbrown`, `smallvec`, `arrayvec`, `euclid`,
`peniko`, `color`, `foldhash`, `log`, `num-traits`. Les entrées viennent du document :
coordonnées appareil bornées (10^12), tirets découpés en espace utilisateur et comptés
avant le tracé (une boucle sans fin de `kurbo` sur des tirets minuscules est ainsi
évitée), travail estimé du rasteriseur (segments, tuiles traversées, aire couverte,
contours, tirets) et découpes actives bornés par `RenderLimits`, côté du canevas limité à
16 384 px (`vello_cpu` panique sur un débordement `u16` près de 65 535), aucune couche
(`vello_cpu` fait échouer une assertion sur une couche non refermée), cible de fuzzing
`render`. La rasterisation finale n'est pas annulable ; son coût est borné par ce budget. Dependabot propose chaque semaine les mises à jour Cargo, npm et actions ; le cache
Rust de la CI n'est écrit que depuis `main`, jamais depuis une PR.

CI de PR sans secrets, jeton en lecture seule, actions épinglées à un commit revu,
pas de `pull_request_target` exécutant du code d'une contribution non fiable.
Les runners de signature sont séparés ; artefacts et mises à jour sont vérifiés avant
distribution. Ne pas utiliser l'agent pour afficher ou récupérer des clés de signature.

Pour un incident : conserver un reproducer autorisé hors canaux publics si sensible,
désactiver la capacité touchée si nécessaire, corriger et publier un avis proportionné.
Créer une politique publique de signalement avec un contact réel avant la diffusion ;
ce kit n'invente pas d'adresse de sécurité.
