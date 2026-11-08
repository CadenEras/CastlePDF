# ADR-0004 — Reconstruction de la table des références croisées

Statut : proposé
Date : 2026-11-08
Responsable : Mélissa Gries (maintenance du dépôt)

## Contexte et contraintes

ADR-0002 a choisi une lecture stricte en P1 et renvoyé la réparation à « une capacité à
part, avec son propre corpus ». La première mesure sur corpus réel (`castle-pdf survey`,
`docs/QUALITY.md`, 2026-11-08) en donne la taille :

| Cause du refus à l'ouverture | SafeDocs (2 000) | UNSAFE (2 636) |
| --- | --- | --- |
| `startxref` absent (surtout des fichiers tronqués) | 9 | 319 |
| Offset xref faux : `MissingXref`, `InvalidXrefEntry`, `InvalidXrefSubsection`, `ObjectIdMismatch` | 56 | 144 |
| `%%EOF` absent après `startxref` | 13 | 13 |
| Chiffrement (hors périmètre de cet ADR) | 42 | 44 |
| Autres | 18 | 57 |

Les causes structurelles représentent environ 4 % des fichiers web ordinaires et
18 % de l'échantillon UNSAFE. Pour la plupart, les objets sont intacts dans le fichier.
Ce qui manque ou est faux, c'est l'index qui permet de les trouver. Tous les lecteurs
courants (pdf.js, poppler, MuPDF, qpdf, Acrobat) reconstruisent cet index par
balayage. Un lecteur qui refuse 4 % des fichiers ordinaires ne sera pas utilisé.

Contraintes :
- les budgets cumulés restent appliqués (`max_input_bytes`, objets visités, octets
  décodés) ;
- une réparation n'est jamais silencieuse (ADR-0002 : « jamais de résultat partiel
  silencieux ») ;
- on n'écrit jamais par-dessus le fichier source ;
- aucune dépendance nouvelle ;
- le chiffrement reste `Unsupported`.

## Options

1. **Rester strict.** C'est simple et prévisible, mais environ 4 % des fichiers web
   restent illisibles, et ce taux croît avec les fichiers anciens ou tronqués.
2. **Tolérances ponctuelles**, comme accepter un offset décalé de quelques octets ou un
   `%%EOF` manquant. C'est peu coûteux, mais chaque tolérance est un cas particulier et
   cela ne couvre pas les fichiers tronqués.
3. **Reconstruire l'index par balayage** quand la lecture stricte échoue. On cherche
   les en-têtes `n g obj` dans tout le fichier, on relit les flux d'objets trouvés, puis
   on choisit un trailer : le dernier `trailer` lisible, sinon un objet `/Type /Catalog`.
   C'est la méthode des autres lecteurs. Elle a un coût linéaire en taille de fichier et
   pose des ambiguïtés : un objet présent plusieurs fois, ou des octets qui ressemblent à
   un en-tête à l'intérieur d'un flux.
4. **Option 3, déclenchée aussi à la résolution.** Un objet dont l'offset ne pointe pas
   vers le bon en-tête (`ObjectIdMismatch` en lecture paresseuse) bascule vers l'index
   reconstruit, construit une seule fois.

## Décision (proposée)

**Option 4**, sous la forme d'une capacité distincte, visible et testée :

| Sujet | Proposition |
| --- | --- |
| Déclenchement | `PdfFile::open` tente d'abord la lecture stricte. Si elle échoue sur une erreur structurelle (`MissingStartxref`, `MissingEof`, `MissingXref`, `InvalidXrefEntry`, `InvalidXrefSubsection`, `InvalidTrailer`, `XrefPrevCycle`, `Truncated` dans une section xref), on reconstruit l'index. Une résolution qui rencontre `ObjectIdMismatch` utilise aussi l'index reconstruit, construit au plus une fois par fichier. Le chiffrement, une version non prise en charge et les limites de ressources ne déclenchent jamais de réparation |
| Balayage | Un seul passage linéaire sur la source, sous `max_input_bytes`. Chaque en-tête `n g obj` accepté est facturé à `max_objects_visited`. Un flux est sauté grâce à son `/Length` s'il est cohérent, sinon jusqu'au prochain `endstream`, ce qui évite d'indexer des octets compressés. Les flux d'objets trouvés sont relus avec le décodeur existant, sous `max_decoded_bytes` |
| Ambiguïtés | Pour un même numéro d'objet, la dernière occurrence dans le fichier l'emporte : c'est l'ordre des mises à jour incrémentales. Un objet compressé ne remplace un objet direct que s'il apparaît plus loin dans le fichier |
| Trailer | Le dernier dictionnaire `trailer` dont la `/Root` existe dans l'index. À défaut, un `/Type /Catalog` unique, ou bien le dernier si plusieurs existent. `/Encrypt` présent : `Unsupported(Encryption)` |
| Visibilité | `PdfFile::repair()` renvoie `None` ou un rapport (`RepairReport` : cause de la réparation, nombre d'objets indexés, nombre d'occurrences en conflit). `castle-pdf inspect` l'affiche, `survey` compte les fichiers réparés à part, et l'UI affichera « document réparé ». Une option `strict` de `PdfFile::open` (activée par les oracles et les tests de structure) conserve le comportement actuel |
| Sauvegarde | Un document réparé n'est jamais réécrit sur sa source. P4 (écriture) imposera une sauvegarde sous un nouveau nom et un avertissement |
| Hors périmètre | Arbre des pages abîmé (`/Kids`, `/Count` incohérents) et `/Length` faux en dehors du balayage. Ces cas feront l'objet de décisions séparées, mesurées avec `survey` |

## Conséquences et validation

- **Fixtures synthétiques** générées par `scripts/make-fixtures.mjs` :
  - offsets xref décalés ;
  - `startxref` absent ;
  - fichier tronqué au milieu d'un objet ;
  - objet dupliqué par une mise à jour incrémentale ;
  - texte `1 0 obj` à l'intérieur d'un flux ;
  - trailer absent mais catalogue présent ;
  - flux d'objets non indexé.

  Pour chacune, le résultat attendu est décrit dans `manifest.json`.
- **Limites :** taille limite et limite + 1 pour objets visités et octets décodés pendant
  le balayage. Un fichier de 512 Mio rempli d'en-têtes `n g obj` est arrêté par
  `max_objects_visited`. Le coût reste linéaire, sans retour en arrière.
- **Oracle :** comparer la liste des objets et la racine à `qpdf --show-xref` et
  `qpdf --check`, qui reconstruisent aussi. Un désaccord est analysé, pas tranché en
  faveur de qpdf.
- **Fuzzing :** une cible `repair`, avec des octets arbitraires et la réparation activée.
  Propriétés : aucun panic, travail borné ; un fichier valide n'est jamais réparé.
- **Mesure :** relancer `survey` sur le même échantillon avant et après. Le gain attendu
  porte sur les 554 refus structurels mesurés (78 + 476). Le taux de fichiers réparés est publié à
  part.
- **Risque :** un fichier réparé peut s'afficher différemment des autres lecteurs, par
  exemple si un objet en conflit est choisi autrement. Le rapport de réparation le
  signale ; les écarts observés deviennent des fixtures.
- **Retour arrière :** la réparation est derrière l'option `strict` et un module séparé
  de `pdf-syntax` (`repair.rs`). Désactiver le déclenchement rétablit la lecture
  d'ADR-0002 sans toucher au reste.
- **Une fois accepté :** ADR-0002 est amendé sur la ligne « Fichiers abîmés », et
  `docs/ROADMAP.md` place la réparation juste après la tranche 4 de P2.
