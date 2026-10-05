# Architecture et développement d’applications
FirstLight charge le noyau Aster et l’archive système par UEFI. Aster gère la mémoire, l’ordonnancement, les pilotes, les systèmes de fichiers, le réseau, les appels système et le serveur de fenêtres Lumen. Lumen demeure dans le noyau; les applications s’exécutent en anneau 3.

## Comprendre les couches
1. Consultez libs/abi pour les numéros d’appels système, les événements et les structures partagées validées.
2. Utilisez CoreKit pour les fichiers, les processus, les fils d’exécution, les préférences, le réseau et les communications interprocessus.
3. Implémentez le trait App d’AuroraKit pour les fenêtres natives. L’environnement dessine dans des surfaces partagées et transmet les événements.
4. Utilisez les primitives et les commandes déclaratives de Lumen. Des identifiants stables conservent le focus.

AuroraKit offre des libellés, titres, boutons, interrupteurs, curseurs, rangées, colonnes, grilles, défilements, cartes et panneaux de verre. Les éditeurs de texte existants restent distincts. Les matériaux transparents sont facultatifs; les applications opaques demeurent compatibles.

## Développer une application
Créez une application compilable sur l’hôte dans userland, implémentez title/size/draw et les gestionnaires requis, puis compilez avec xtask. Testez le redimensionnement, le focus, le thème, le travail non enregistré et l’isolation. Consultez [Paquets](developer-packages.md) pour la distribution.

## Limites et débogage
Utilisez `cargo xtask run --gdb` et le débogueur de l’hôte pour examiner le noyau. Le débogueur source natif de Studio n’est pas disponible. Le SDK ne contient pas encore de sysroot Rust hors ligne complet ni de chaîne native.
