# Polices de remplacement livrées

Inventaire des polices tierces incluses dans `pdf-render` (fonction `standard-fonts`,
active par défaut) par `include_bytes!`. Elles remplacent une police qu'un PDF nomme sans
l'inclure, ou dont le programme n'est pas lu (Type 1) ou est endommagé (ADR-0005, étape
5.3). `cargo-deny` ne voit pas ces fichiers de données : cet inventaire en tient lieu.

Toutes sont sous **SIL Open Font License 1.1** (fichiers `OFL-*.txt` ci-contre). Elles
peuvent être redistribuées et modifiées, y compris réduites, avec leur licence, mais pas
vendues seules. Les enregistrements `name` (droits d'auteur, licence) sont conservés.
Les licences de ces trois familles, aux commits retenus, ne déclarent aucun nom réservé
(« Reserved Font Name ») dans leur en-tête : les fichiers réduits peuvent garder leurs
noms. À vérifier à chaque mise à jour des sources, car un nom réservé imposerait de
renommer.

| Fichiers | Remplace | Source (github.com/googlefonts, commit) | Licence |
| --- | --- | --- | --- |
| `Arimo-{Regular,Bold,Italic,BoldItalic}.ttf` | Helvetica, Arial et polices sans empattement | `arimo` `4a6255f2`, `fonts/ttf/` | `OFL-Arimo.txt` |
| `Tinos-{Regular,Bold,Italic,BoldItalic}.ttf` | Times et polices à empattement | `tinos` `3b4482a9`, `fonts/ttf/` | `OFL-Tinos.txt` |
| `Cousine-{Regular,Bold,Italic,BoldItalic}.ttf` | Courier et polices à chasse fixe | `cousine` `0c999bee`, `fonts/ttf/` | `OFL-Cousine.txt` |

Les fichiers sont produits par `scripts/build-standard-fonts.sh`, avec fonttools 4.60.1 :
- le script vérifie le sha256 de chacune des 12 polices sources ;
- il les réduit aux 245 caractères des encodages Standard, WinAnsi et MacRoman ;
- il retire hinting et tables de mise en page.

Total : environ 280 Kio. Les largeurs de ces polices sont celles d'Arial, de Times New Roman
et de Courier New, donc celles des polices standard qu'elles remplacent. Un test de
`pdf-render` les compare aux métriques AFM d'Adobe (`replacements_keep_the_standard_widths`).

sha256 des fichiers livrés :

```
e0502857323fddb1a2ee07586c80bc728445f33b2562e6a482ee3481add1f2e5  Arimo-Regular.ttf
b3bead2a626fd6ed27e478e1dc78882ae9548ca824799cadeb0350c6ff32ce39  Arimo-Bold.ttf
bbea0cfcb7635f02975042c2a18c51c6f9d525674f188c68cde329756100ee63  Arimo-Italic.ttf
7e36f16ed06d2c3f44ee031c9bde6ba985d8d32fa79dfbdf5c9aacabdee8020f  Arimo-BoldItalic.ttf
13d7ff7a0d226642c7b0ced8dabf0eeb1f4766ab4c7ce3c3f9be3b0a7600c1ce  Tinos-Regular.ttf
84333a6b2341482db5808c35e94ef57f0208b5873daaecd089fec8e9068d8290  Tinos-Bold.ttf
803ba199b196a482c96f8503c02be3fef9dff524d00558ec0b13d993100454da  Tinos-Italic.ttf
35cbb71b1603487153a7bedc1a21206fa06c26143c1bb37e8ccf0e793f12ae50  Tinos-BoldItalic.ttf
dc3dbb7fea83c4cd6d6badc9af4bebd1a5ef819fd741e92133da116a88e9964d  Cousine-Regular.ttf
5313e36686c2cb6cc75a69bc0eadd4ad0b77ee2098118ed5d752deb12a5d0427  Cousine-Bold.ttf
157802fd77c4820b37d4cd67a5c8cb66e6c0d4a8bc0fe5ec524fccfa19ff2bd1  Cousine-Italic.ttf
ac6b876618ccd11443ed3679d9a7f3f2f6cd321c4ad2d4bfc94c8ab2ec24d201  Cousine-BoldItalic.ttf
```

Symbol et ZapfDingbats n'ont pas encore de remplacement (étape 5.3b) : leur texte reste
signalé `Text`.
