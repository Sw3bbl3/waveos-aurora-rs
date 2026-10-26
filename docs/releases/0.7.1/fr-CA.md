# WaveOS Aurora 0.7.1

Pas encore publiée.

Cette mise à jour rend WaveOS plus solide face aux données qu’il n’a pas créées. Les images, polices, témoins, clés USB et disques endommagés sont désormais refusés proprement au lieu de fermer une app ou d’arrêter le système, et l’ensemble du code passe une révision automatisée plus stricte.

## Aperçu, Fichiers et Nebula
- Les images JPEG endommagées ne ferment plus Aperçu, Fichiers ou Nebula.
- Nebula ne se ferme plus lorsqu’un site envoie un témoin dont la date contient des lettres accentuées ou non anglaises.
- Les pages aux tailles CSS démesurées, comme `margin: 1e40px`, se mettent en page au lieu de produire des positions aberrantes.
- Une police dont un glyphe a un contour mal formé ne fait plus planter l’affichage du texte.

## Stockage
- Une clé USB FAT32 ou un volume AuroraFS endommagé est signalé comme endommagé au lieu d’arrêter le système, et la vérification d’un volume AuroraFS endommagé ne bloque plus.
- Une demande d’agrandir un fichier au-delà de l’espace libre de son disque est refusée aussitôt, au lieu d’épuiser la mémoire du système.

## Son
- Les enregistrements forts ne craquent plus lorsqu’ils sont lus à une autre fréquence d’échantillonnage.

## Notes
- La loupe du bouton Rechercher n’affiche plus de disque foncé lorsque le pointeur la survole.

## Pour les développeurs
- `cargo xtask lint` exécute Clippy sur les bibliothèques hôtes, le noyau, le chargeur et l’espace utilisateur, en traitant les avertissements comme des erreurs, et l’intégration continue l’exécute à chaque changement.
- Chaque analyseur de données externes a un test de robustesse qui lui soumet des milliers d’entrées corrompues. Consultez [Compilation et tests](../../handbook/0.7/fr-CA/developer-build.md).
- Le noyau utilise désormais toujours le code SHA-256 portable, comme il se doit : il ne sauvegarde jamais les registres SIMD.

## Limites connues
Les tests de robustesse modifient au hasard des entrées valides; ce n’est pas du fuzzing guidé par la couverture, si bien qu’ils rendent d’autres plantages moins probables sans pouvoir les exclure. Consultez le [guide complet](../../handbook/0.7/fr-CA/start-welcome.md).

## Résultats de validation
`cargo xtask lint`, la suite de tests hôtes de [Compilation et tests](../../handbook/0.7/fr-CA/developer-build.md) et les autotests du noyau (`cargo xtask test`) ont été exécutés sur ce changement. Chaque plantage corrigé a un test de régression qui échoue sans sa correction.
