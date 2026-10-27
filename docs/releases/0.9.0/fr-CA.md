# WaveOS Aurora 0.9.0

Pas encore publiée.

WaveOS dessine plus vite, affiche votre batterie, exécute son premier pilote graphique venu de Linux et offre le Wi-Fi dans Réglages. Cette mise à jour comprend aussi tous les changements annoncés pour la version 0.8.0.

## Plus rapide
- Le dessin du bureau prend moins de la moitié du temps qu’avant : les ombres des fenêtres, les remplissages translucides, les bordures et le verre sont tous plus rapides, et le flou d’arrière-plan de l’écran de connexion est environ six fois plus rapide (mesuré sur le processeur hôte; les données se trouvent dans `docs/measurements/drawing-2026-10-27` du code source).
- Le serveur de fenêtres dessine avec SSE2 dans sa propre tâche, n’envoie que les parties de l’écran qui ont changé quand vous déplacez une fenêtre, et accède à l’écran en mode « write-combining », pour que les images arrivent à l’affichage par rafales.
- Le curseur clignotant de l’écran de connexion ne redessine plus tout l’écran.
- Les images JPEG se décodent plus vite, avec un résultat identique.

## Batteries
- Les batteries et les adaptateurs secteur des portables se lisent correctement sur plus de machines : deux bogues de l’interpréteur ACPI, qui faisaient afficher 0 % à certaines batteries ou arrêtaient le code de la batterie, sont corrigés.
- Dans une machine virtuelle, `cargo xtask run --battery=host` affiche dans WaveOS la batterie de votre propre ordinateur, en direct, sur un hôte Linux, macOS ou Windows.

## Graphisme : des pilotes venus de Linux
- WaveOS peut maintenant exécuter des pilotes graphiques écrits pour Linux. Le premier est le pilote **virtio-gpu** de Linux, avec le cœur d’affichage DRM, sans modification, sur une couche de compatibilité écrite pour WaveOS. Dans une machine virtuelle dotée d’un écran virtio (`cargo xtask run --gpu virtio`), il règle le mode d’affichage et montre tout le bureau.
- Seul le code de Linux dont la licence le permet est inclus (MIT et licences semblables). Là où l’implémentation de Linux est réservée à la GPL, WaveOS a la sienne.

![L’écran de connexion, affiché par le pilote virtio-gpu de Linux](../../screenshots/0.9/virtio-gpu-login.png)

## Wi-Fi
- **Réglages → Réseau** comporte une section Wi-Fi : les réseaux autour de vous, un champ de mot de passe, **Rejoindre** et **Déconnecter**. WaveOS rejoint les réseaux ouverts et WPA2 Personnel; les réseaux WEP, WPA3 seulement et d’entreprise sont affichés, mais ne peuvent pas encore être rejoints.
- Quand vous êtes en Wi-Fi, l’anneau d’état affiche le symbole Wi-Fi et Current affiche le nom du réseau.
- Les cartes Wi-Fi Intel (du Wireless 7265 au Wi-Fi 6E AX211) sont reconnues, mais WaveOS ne peut pas encore démarrer leur radio; Réglages l’indique. Le Wi-Fi fonctionne aujourd’hui sur les réseaux simulés pour les essais (`cargo xtask run --wifi-sim`, réseau « Harbor Cafe », mot de passe « tide pools at dawn »).

![Rejoindre un réseau](../../screenshots/0.9/wifi-password.png)

## Pour les développeurs
- `cargo xtask run --gpu virtio` démarre avec un écran virtio piloté par le pilote de Linux; `cargo xtask test --gpu virtio` vérifie aussi que l’écran s’est allumé.
- `cargo xtask run --wifi-sim` ajoute les réseaux Wi-Fi simulés.
- La compilation des pilotes graphiques demande clang. Sans lui, le noyau se compile sans ces pilotes.

## Limites connues
Aucune vraie carte Wi-Fi ne fonctionne encore : le pilote Intel s’arrête avant de démarrer la radio, et rien de ce pilote n’a encore tourné sur du vrai matériel. Le pilote graphique de Linux fonctionne dans les machines virtuelles (virtio); les pilotes pour les cartes graphiques AMD, Intel et NVIDIA, ainsi que l’accélération 3D, ne sont pas encore là. Dans la machine d’essai de cette mise à jour, le micrologiciel ne sortait pas de la suspension; le test de suspension a donc été sauté, comme il l’a été sur le code de la version 0.8.0.

## Résultats de validation
`cargo xtask lint`, les tests hôte décrits dans [Compiler et tester](../../handbook/0.7/fr-CA/developer-build.md) (qui comprennent maintenant les tests de la bibliothèque Wi-Fi, vérifiés avec les vecteurs d’essai IEEE 802.11 et une implémentation indépendante) et les autotests du noyau sur deux démarrages (37 tests, dont l’autotest de la couche de compatibilité Linux et une connexion WPA2 avec DHCP sur le réseau simulé) ont été exécutés sur ce changement, de même que `cargo xtask test --gpu virtio`. Les captures d’écran de `docs/screenshots/0.9` proviennent de QEMU à 1280 × 800.
