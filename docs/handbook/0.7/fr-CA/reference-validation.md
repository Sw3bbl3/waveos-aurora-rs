# Validation et mesures de performance
La validation distingue les tests automatisés, les interactions observées, les mesures et les configurations non testées.

## Lire les résultats
1. Ouvrez le rapport de validation dans les fichiers de la version GitHub.
2. Faites correspondre la révision, la somme de contrôle, l’hôte, l’émulateur, la résolution et la tâche à votre configuration.
3. Comparez des mesures équivalentes : le temps de composition et de transfert n’est pas la latence complète, et le nombre de pixels redessinés n’est pas un temps écoulé.

Les mesures du bureau utilisent une répétition de préparation et cinq répétitions mesurées. Learn apparaît dans la version 0.7; ses résultats de démarrage, de recherche et de défilement constituent donc une référence initiale. Les gains chiffrés ne sont publiés que lorsqu’ils sont confirmés par des mesures répétables.

## Reproduire
Utilisez `tools/benchmark_desktop.py LOG OUTPUT.json` avec la configuration documentée. Conservez les échantillons bruts et calculez la médiane et le 95e percentile par tâche. Une comparaison avec une référence de développement doit être présentée comme telle, et non comme une comparaison avec une version publique précédente.

## Dépannage
Si aucun test ne couvre votre matériel, considérez sa prise en charge comme non vérifiée. N’appliquez pas un pourcentage obtenu dans le meilleur cas à tout le système.

## Résultats du bureau — 5 octobre 2026
La comparaison utilise la base de développement `1011e1a`, et non une version publique précédente, ainsi que le code du bureau à `368d421`. Les retouches ultérieures du guide et de Learn ne changent pas l’algorithme du bureau mesuré. Hôte : Apple M2 (8 cœurs), 16 GB de mémoire, macOS 27.0.1 (26A434), QEMU 11.1.1, émulation TCG, q35, x86-64 `-cpu max`, 4 processeurs invités, 512 MiB de mémoire, AHCI, 1280×800, affichage sans fenêtre et réseau désactivé. Rust nightly-2026-09-28 utilise le commit d080e7dff1b0fc54541545252818f8cccf995d05 et LLVM 23.1.1.

Chaque version a exécuté une répétition de réchauffement, puis cinq répétitions mesurées, avec Notes et Welcome aux mêmes positions, l’apparence claire par défaut et aucune compilation en parallèle. `AURORA_QMP_KEY_DELAY=0.1` règle les touches injectées. Le script remplace une ligne dans Notes, déplace la fenêtre puis la ramène, change de fenêtre puis revient, et ouvre, recherche et ferme Settings. Des captures après chaque étape confirment le placement. Le changement de fenêtre inclut le nouveau sélecteur; son travail visuel diffère donc volontairement.

Toutes les paires ci-dessous indiquent base / candidat. Le temps mesure la composition invitée et la copie vers le tampon d’affichage par lot d’images, en millisecondes. Il exclut le dessin de l’application, la livraison des événements et la latence perçue. La résolution du chronomètre est de 1 ms. Le p95 utilise le rang supérieur le plus proche; les médianes regroupent tous les lots mesurés. Les pixels peints comptent les zones soumises, y compris le curseur. Les lots `partial-or-cursor` de la base sont des mises à jour du curseur; le candidat distingue full, partial et cursor.

| Charge de travail | Échantillons | Médiane ms | p95 ms | Pixels peints moyens |
|---|---:|---:|---:|---:|
| typing | 180 / 182 | 47 / 10 | 49 / 12 | 1.024e+06 / 71785.9 |
| dragging | 61 / 61 | 45 / 36 | 62 / 69 | 940126 / 621186 |
| switching | 57 / 50 | 28 / 23 | 61 / 69 | 934240 / 519785 |
| settings | 168 / 170 | 50 / 49 | 76 / 73 | 1.024e+06 / 933650 |

La saisie dans Notes réduit de 78.7% le temps médian de composition/copie (47 à 10 ms) et de 93.0% les pixels peints par lot (1,024,000 à 71,785.9). La médiane baisse à chaque répétition : 46–47 ms pour la base, 9–11 ms pour le candidat. Cela appuie seulement l’amélioration des mises à jour localisées.

Les régressions du p95 pendant le déplacement et le changement de fenêtre sont significatives : 62 à 69 ms et 61 à 69 ms. Les populations d’images différentes et le nouveau sélecteur ne justifient pas une affirmation générale sur la réactivité. L’écart médian de Settings, de 50 à 49 ms, est trop petit pour annoncer une amélioration reproductible. Aucune réduction de mémoire ni accélération globale n’est annoncée.

## Mesures initiales de Learn
Learn est nouveau. Avec le réseau désactivé, du texte anglais de 16 px par défaut et une zone de contenu de 1020×620, une répétition de réchauffement et cinq répétitions mesurées ont donné :

| Opération | Échantillons | Médiane ms | p95 ms |
|---|---:|---:|---:|
| Démarrage jusqu’au premier dessin de l’application | 5 | 81 | 87 |
| Recherche pendant la saisie de building | 40 | 3 | 4 |
| Dessin après PageDown/PageUp | 10 | 15.5 | 22 |

Le démarrage commence dans Learn et exclut la création du processus et la présentation à l’écran. La recherche mesure seulement le filtrage et le classement. Le défilement mesure le dessin après le changement de position, sans composition ni livraison des entrées. Ce sont des mesures initiales absolues, sans comparaison avec une ancienne version de Learn.

## Données brutes et reproduction
La [base du bureau](https://github.com/Sw3bbl3/waveos-aurora-rs/blob/v0.7.0/docs/releases/0.7.0/evidence/desktop-baseline.json), le [candidat du bureau](https://github.com/Sw3bbl3/waveos-aurora-rs/blob/v0.7.0/docs/releases/0.7.0/evidence/desktop-candidate.json) et les [échantillons Learn](https://github.com/Sw3bbl3/waveos-aurora-rs/blob/v0.7.0/docs/releases/0.7.0/evidence/learn-baseline.json) conservent chaque échantillon et répétition. Ces liens nécessitent le réseau; les fichiers de validation de la version contiennent des copies hors ligne.

1. Compilez séparément la base et le candidat; conservez leurs partitions système et utilisez des disques d’exemple distincts.
2. Exécutez `cargo xtask run --no-build --no-refresh --headless --disk-image PATH --net none`, en redirigeant la sortie vers un journal. Ne remplacez pas le système de la base par celui du candidat.
3. Exécutez `AURORA_QMP_KEY_DELAY=0.1 python3 tools/benchmark_desktop.py LOG OUTPUT.json` avec la même configuration pour les deux versions.
4. Utilisez `python3 tools/summarize_performance.py BASELINE.json CANDIDATE.json` pour les statistiques.
5. Pour Learn, fermez ses fenêtres et choisissez l’anglais; exécutez `tools/benchmark_learn.py LOG OUTPUT.json` avec le même délai de touches.

L’ordonnancement de l’hôte, l’émulation TCG, les chronomètres entiers et les nombres d’images différents limitent l’interprétation. Le matériel physique et VirtualBox n’ont pas été mesurés.
