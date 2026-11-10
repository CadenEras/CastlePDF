# Polices de test

Polices tierces utilisées par les fixtures et les tests. Ce ne sont pas des polices
livrées avec l'application : celles-là (ADR-0005, étape 5.3) auront leur propre
inventaire.

| Fichier | Origine | Licence | sha256 |
| --- | --- | --- | --- |
| `arimo-subset.ttf` | Sous-ensemble d'Arimo Regular, [googlefonts/arimo](https://github.com/googlefonts/arimo) commit `4a6255f2`, `fonts/ttf/Arimo-Regular.ttf` (sha256 `41b22bc8…24f9f8c`) | SIL Open Font License 1.1 (`OFL-Arimo.txt`) | `a626576bb08523e23c61f08c91387c81a450f0cae459688c0c7cac50fb7b69da` |
| `arimo-subset.json` | Identifiant de glyphe et chasse de chaque caractère conservé, extraits du fichier ci-dessus | Données dérivées, même licence | voir le script |

Les deux fichiers sont produits par `scripts/subset-test-font.sh`, avec fonttools 4.60.1 :
- caractères conservés : espace, `!,.`, chiffres, lettres latines non accentuées ;
- sans hinting ni tables de mise en page.

Le script vérifie le sha256 de la police source avant de l'utiliser. La CI ne le lance pas.
Les fichiers commités sont contrôlés indirectement : `node scripts/make-fixtures.mjs --check`
compare `tests/corpus/public/truetype.pdf`, qui inclut la police, au manifeste.

L'OFL autorise la redistribution et la modification, y compris en sous-ensemble,
avec la licence jointe, mais interdit de vendre la police seule. Le nom « Arimo » est
un nom réservé. Les fixtures l'emploient seulement comme `/BaseFont` interne au PDF,
précédé d'un préfixe de sous-ensemble.
