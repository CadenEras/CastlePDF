# Polices de test

Polices tierces utilisées par les fixtures et les tests. Ce ne sont pas des polices
livrées avec l'application : celles-là (ADR-0005, étape 5.3) auront leur propre
inventaire.

| Fichier | Origine | Licence | sha256 |
| --- | --- | --- | --- |
| `arimo-subset.ttf` | Sous-ensemble d'Arimo Regular, [googlefonts/arimo](https://github.com/googlefonts/arimo) commit `4a6255f2`, `fonts/ttf/Arimo-Regular.ttf` (sha256 `41b22bc8…24f9f8c`) | SIL Open Font License 1.1 (`OFL-Arimo.txt`) | `a626576bb08523e23c61f08c91387c81a450f0cae459688c0c7cac50fb7b69da` |
| `arimo-subset.json` | Identifiant de glyphe et chasse de chaque caractère conservé, extraits du fichier ci-dessus | Données dérivées, même licence | voir le script |
| `source-sans-subset.otf` | Sous-ensemble de Source Sans 3 Regular (OpenType, contours CFF), [adobe-fonts/source-sans](https://github.com/adobe-fonts/source-sans) commit `7c691c27`, `OTF/SourceSans3-Regular.otf` (sha256 `08df2664…f13ab6`) | SIL OFL 1.1 (`OFL-SourceSans.txt`) | `0867dd8d628001e79899811e0d0c280853cde01bf53e5e29b8f77a1883efebe1` |
| `source-sans-subset.cff` | Sa table `CFF ` nue (CFF à noms de glyphes) | Même licence | `9822e2e6d4aa79f50d3599d73b544804ab1f82c1a89603928c93e3c008daea7b` |
| `noto-cjk-subset.cff` | Table `CFF ` nue (CFF à CID) d'un sous-ensemble de Noto Sans CJK SC Regular, [notofonts/noto-cjk](https://github.com/notofonts/noto-cjk) commit `165c01b4`, `Sans/OTF/SimplifiedChinese/NotoSansCJKsc-Regular.otf` (sha256 `2c76254f…5b74b`) | SIL OFL 1.1 (`OFL-NotoSansCJK.txt`) | `463a0b5ce2ed3a9734fb756259cd2a8bd5e1f48f11c859326932706a643d1f5d` |
| `source-sans-subset.json`, `noto-cjk-subset.json` | Identifiant de glyphe, nom ou CID et chasse de chaque caractère conservé | Données dérivées, même licence | voir le script |

Les fichiers d'Arimo sont produits par `scripts/subset-test-font.sh` et les fichiers CFF par
`scripts/subset-cff-test-fonts.sh`, avec fonttools 4.60.1 :
- caractères conservés : espace, `!,.`, chiffres, lettres latines non accentuées, et pour
  Noto Sans CJK quelques lettres plus « 字体中文 » ;
- sans hinting ni tables de mise en page.

Chaque script vérifie le sha256 de la police source avant de l'utiliser. La CI ne les lance
pas. Les fichiers commités sont contrôlés indirectement : `node scripts/make-fixtures.mjs
--check` compare au manifeste `tests/corpus/public/truetype.pdf` et `cff.pdf`, qui incluent
ces polices.

L'OFL autorise la redistribution et la modification, y compris en sous-ensemble,
avec la licence jointe, mais interdit de vendre la police seule. « Arimo » et « Source »
sont des noms réservés. Les fixtures les emploient seulement comme `/BaseFont` interne au
PDF, précédé d'un préfixe de sous-ensemble.
