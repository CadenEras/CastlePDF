# Images de test

JPEG synthétiques utilisés par `tests/corpus/public/jpeg.pdf` et comme graines de la
cible de fuzzing `image`. Leur contenu (aplats et dégradés) est produit par le projet
(données synthétiques redistribuables) et encodé par Pillow 11.3.0, avec la
libjpeg-turbo incluse dans cette version, via `scripts/make-test-jpegs.sh`. La CI ne lance
pas ce script ; les fichiers commités sont contrôlés à travers le manifeste du corpus.

| Fichier | Contenu | sha256 |
| --- | --- | --- |
| `rgb.jpg` | 32 × 16, YCbCr : bande verte en haut, rouge à gauche, bleu à droite | `c4e264fa717c92e3607a11579ffc95dcefe44bedfd3772f4039b339f5fabe1a2` |
| `gray.jpg` | 32 × 16, niveaux de gris : dégradé du noir au blanc | `d70ab7f6987ea34c98922b8a9fb82859d9097b5574efeb306802cd08175c6c20` |
| `cmyk.jpg` | 32 × 16, CMYK Adobe (échantillons inversés, segment APP14) : cyan à gauche, jaune à droite | `0ed5cfde63a739f3bbe10f6ecf9654d3a71b4c55cc489b10f924e186b190c762` |
