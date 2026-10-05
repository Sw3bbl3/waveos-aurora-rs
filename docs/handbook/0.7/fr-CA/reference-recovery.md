# Dépannage et récupération
Partez du symptôme et conservez les éléments de diagnostic avant de modifier un disque.

## Échec de démarrage ou d’affichage
1. Confirmez que la machine virtuelle utilise UEFI x86-64 et l’image attendue.
2. Conservez le journal série produit par `cargo xtask run`.
3. Si un nouveau mode d’affichage est illisible, laissez expirer la confirmation ou choisissez Revert.
4. Redémarrez avec une copie fonctionnelle au besoin. Conservez l’image défectueuse pour le diagnostic.

## Problèmes de saisie sous macOS
Activez QEMU dans Réglages Système → Confidentialité et sécurité → Accessibilité, redémarrez QEMU et cliquez dans la machine virtuelle. Control+Option+G libère la capture. L’interception de Commande doit être vérifiée avec un clavier physique; l’absence d’avertissement ne suffit pas.

## Problèmes d’application
Le plantage d’un processus isolé devrait laisser le bureau utilisable. Rouvrez l’application, consultez le rapport et `dmesg`, puis reproduisez avec un petit document. Si Learn signale du contenu manquant, reconstruisez l’archive avec `cargo xtask docs` et `cargo xtask build`.

## Fichiers ou réglages manquants
Utilisez `df` pour confirmer la présence d’un volume personnel AuroraFS accessible en écriture. Une session ISO seule peut utiliser du stockage temporaire. Arrêtez proprement le système et vérifiez que l’hôte rouvre la même image persistante. Restaurez les sauvegardes lorsque la machine virtuelle est arrêtée; ne recréez jamais un disque avant d’avoir récupéré les fichiers.

## Signaler un problème
Indiquez la version, l’architecture de l’hôte, la version QEMU, les options, les étapes de reproduction, le résultat attendu et obtenu, le journal série et une capture. Retirez les données privées des rapports publics.
