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
