# CMap prédéfinies d'Adobe livrées

Inventaire des CMap d'Adobe incluses dans `pdf-render` par `include_bytes!`, derrière la
fonction `cjk-cmaps` (active par défaut) : ADR-0006, tranche 3.7b. `cargo-deny` ne voit pas
ces fichiers de données ; cet inventaire en tient lieu.

Chaque fichier `*.z` est le fichier d'Adobe inchangé, compressé en zlib (Deflate) : le moteur
le décompresse puis le lit avec son lecteur de CMap (`text/cmap.rs`), le même que pour les
CMap des documents. Aucun format propre au projet.

| Fichiers | Rôle | Source |
| --- | --- | --- |
| `UniJIS-UCS2-{H,V}`, `UniJIS-UTF16-{H,V}`, `90ms-RKSJ-{H,V}` | Encodages vers les CID d'Adobe-Japan1 | `cmap-resources`, `Adobe-Japan1-7/CMap/` |
| `UniGB-UCS2-{H,V}` | Encodage vers Adobe-GB1 | `cmap-resources`, `Adobe-GB1-6/CMap/` |
| `UniCNS-UCS2-{H,V}` | Encodage vers Adobe-CNS1 | `cmap-resources`, `Adobe-CNS1-7/CMap/` |
| `UniKS-UCS2-{H,V}` | Encodage vers Adobe-Korea1 | `cmap-resources`, `Adobe-Korea1-2/CMap/` |
| `Adobe-{Japan1,GB1,CNS1,Korea1}-UCS2` | CID de chaque collection vers Unicode (texte, ISO 32000-2, 9.10.2) | `mapping-resources-pdf`, `pdf2unicode/` |

Choix : les encodages nommés rencontrés sur les premières pages du corpus de mesure (relevé du
2026-11-14 : `UniJIS-UCS2-H`, `UniCNS-UCS2-H`, `UniKS-UCS2-H`, `UniGB-UCS2-H`, `90ms-RKSJ-H`,
`UniJIS-UTF16-H`), avec leur forme verticale `-V`, qui ne fait que quelques kilo-octets.
Un autre encodage prédéfini reste non lu et signalé (`Text`).

Sources, aux versions figées (archives de github.com, sha256 de l'archive) :
- `adobe-type-tools/cmap-resources`, étiquette `20231115`, commit
  `f5cf3bca7fdfeaceb77aa82847e974f2306c20b4`, sha256
  `664dd496e7eea4f41d401c2fc296b639ace93ef14484f53f62abbaaf3c2b39cf` ;
- `adobe-type-tools/mapping-resources-pdf`, commit
  `2dd5e53fb74a01718b9dfd448a0d1cce6fff2aa5` (2023-01-17), sha256
  `bde04d598ec4f8c77c905b2a395aac5016a169758aeef2146413a430fca98e4a`.

Licence : BSD-3-Clause d'Adobe (`LICENSE-cmap-resources.txt`,
`LICENSE-mapping-resources-pdf.txt`). Chaque fichier garde aussi son en-tête de droits
d'auteur. Une distribution binaire doit reproduire cette notice dans sa documentation.

Produits par `node scripts/make-cmaps.mjs <cmap-resources> <mapping-resources-pdf>` ;
`--check` vérifie que chaque fichier décompressé est identique à sa source. Total : 735 886
octets compressés (2 084 051 octets de CMap).
