# Architecture et développement d’applications
FirstLight charge le noyau Aster et l’archive système par UEFI. Aster gère la mémoire, l’ordonnancement, les pilotes, les systèmes de fichiers, le réseau, les appels système et le serveur de fenêtres Lumen. Lumen demeure dans le noyau; les applications s’exécutent en anneau 3.

## Comprendre les couches
1. Consultez libs/abi pour les numéros d’appels système, les événements et les structures partagées validées.
2. Utilisez CoreKit pour les fichiers, les processus, les fils d’exécution, les préférences, le réseau et les communications interprocessus.
3. Décrivez votre interface avec WaveUI, le cadriciel déclaratif qu’utilisent toutes les applications intégrées. Consultez [WaveUI](developer-waveui.md).
4. WaveUI s’appuie sur AuroraKit, qui dessine dans des surfaces Lumen partagées et transmet les événements. N’utilisez AuroraKit directement que pour le travail de fenêtre de bas niveau.

WaveUI offre le texte, les images et symboles, les boutons, interrupteurs, curseurs, compteurs, sélecteurs, champs et éditeurs de texte, piles, grilles, listes, défilements, vues partagées, sections, feuilles, alertes, menus et canevas. Les matériaux transparents sont facultatifs; les applications opaques demeurent compatibles.

## Réduire les réveils inutiles
Les applications WaveUI utilisent `poll_interval` et `poll` pour le travail en arrière-plan, et `Context::after` pour les minuteries. Une application AuroraKit de bas niveau peut implémenter `next_wakeup(&Env) -> Option<u64>` pour retourner une échéance absolue depuis le démarrage. AuroraKit la recalcule après les entrées, le dessin et les appels à `tick`, afin de traiter rapidement le nouveau travail. Retournez `None` pour conserver le comportement de `tick_interval`. Gardez les échéances stables lors des déplacements du pointeur sans rapport avec le travail; `tick` doit retourner vrai seulement si la vue a changé. Files et Settings utilisent des échéances espacées au repos; le suivi des transferts est limité à dix mises à jour par seconde.

Pour les mesures sur une VM jetable, créez `/Settings/runtime-metrics` avant de lancer une application. AuroraKit journalise alors les réveils, les événements, les compteurs de processeur et de mémoire de la VM, ainsi que les compteurs de processeur, de mémoire et de fils du processus, toutes les dix secondes. Retirez ce fichier pour désactiver la collecte aux lancements suivants. `tools/benchmark_everyday.py` et `tools/summarize_everyday.py` comparent des essais répétés au repos et lors du changement de fenêtre; le temps de composition ne représente pas la latence complète des entrées.

## Développer une application
Créez une application compilable sur l’hôte dans userland, implémentez le trait `App` de WaveUI (titre, `update` et `view`), puis compilez avec xtask. Testez le redimensionnement, le focus, le thème, le travail non enregistré et l’isolation. Consultez [Paquets](developer-packages.md) pour la distribution.

## Limites et débogage
Utilisez `cargo xtask run --gdb` et le débogueur de l’hôte pour examiner le noyau. Le débogueur source natif de Studio n’est pas disponible. Le SDK ne contient pas encore de sysroot Rust hors ligne complet ni de chaîne native.
