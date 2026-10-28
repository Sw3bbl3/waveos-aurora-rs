# WaveOS Aurora 0.9.0

Pas encore publiée.

WaveOS dessine plus vite, affiche votre batterie, exécute son premier pilote graphique venu de Linux, offre le Wi-Fi dans Réglages, fonctionne avec tous les types de contrôleurs USB comprend les pavés tactiles et les manettes de jeu, et toutes ses applications sont reconstruites avec WaveUI, le nouveau cadriciel d’interface du WaveSDK. Cette mise à jour comprend aussi tous les changements annoncés pour la version 0.8.0.

## Toutes les applications, reconstruites
- Les quinze applications fournies avec WaveOS sont réécrites avec **WaveUI**, un nouveau cadriciel d’interface déclaratif. Elles partagent un même style et les mêmes commandes : les mêmes boutons, interrupteurs, listes, barres latérales, barres d’outils, menus, alertes et feuilles partout, en apparence claire et sombre et dans votre couleur d’accent.
- Toutes les commandes fonctionnent au clavier : Tab passe de l’une à l’autre, Espace et Retour les activent, et la commande ciblée défile pour devenir visible.
- Le défilement est fluide partout, les menus et les feuilles apparaissent en douceur (ou non, avec Réduire les animations), et les boutons des barres d’outils s’expliquent au survol.
- Le guide de Learn a une nouvelle allure : chaque article repose sur une page arrondie, avec un titre plus clair, des exemples de code arrondis et des notes en évidence, en apparence claire et sombre.

## Des écrans plus grands et plus nets
- Avec un affichage virtio, WaveOS propose tous les modes que l’écran annonce, jusqu’à 5120 × 2160, et **Réglages → Affichage** passe de l’un à l’autre aussitôt. Quand vous redimensionnez la fenêtre de la machine virtuelle, le bureau suit.
- Sur les écrans de 2560 × 1440 et plus, tout est dessiné avec deux fois plus de détail (HiDPI) : le texte et les commandes gardent leur taille et deviennent plus nets. **Réglages → Affichage → Échelle** propose Automatique, 1× ou 2×, appliqués immédiatement.
- `cargo xtask run --resolution 2560x1440` démarre la machine virtuelle à la taille voulue.

## Les applications, au nouveau design
- Toutes les applications suivent maintenant le design Eau vive : une seule échelle de texte, des boutons en pilule, des cartes arrondies, une sélection teintée, et Manrope Light pour les grands chiffres.
- **Notes** devient une bibliothèque : vos notes, les plus récentes d’abord, dans une barre latérale où chercher. Les notes s’enregistrent pendant la frappe, prennent leur nom de fichier de leur titre, et les listes de tâches se cochent d’un clic. Ce sont des fichiers texte dans Documents → Notes.
- **Terminal** a des onglets (Ctrl+T) et une invite en couleurs qui suit le mode clair ou sombre.
- **Calculatrice** affiche les séparateurs de milliers, entoure l’opération qui attend son second nombre et garde un ruban des résultats (Ctrl+T).
- **Horloge** a un anneau de minuterie qui brille pendant le décompte, et l’heure du monde à côté de chaque onglet.
- **Nebula** a une barre d’adresse centrée qui affiche le site et la page, des onglets colorés et une page de démarrage aux couleurs du bureau.
- **Pulse** s’ouvre sur une vue d’ensemble : quatre jauges en direct (processeur, mémoire, disque, réseau) au-dessus des processus, que l’on peut chercher.
- **Paint** range ses outils sur le côté, avec un panneau de couleurs au sélecteur toujours ouvert. **Lens** montre les images bord à bord, avec une barre d’outils flottante et une bande des autres images du dossier.
- **Learn** s’ouvre sur six courtes leçons qui retiennent où vous en êtes. **Studio** a une arborescence de fichiers, des onglets pour les fichiers ouverts, des numéros de ligne, et les problèmes de compilation à part de la sortie.
- **Bienvenue** permet de choisir un fond d’écran et le mode clair ou sombre, et peut revenir à la prochaine ouverture de session. **À propos** et **Installateur** suivent les nouvelles mises en page.

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

## L’USB sur les ordinateurs plus anciens aussi
- WaveOS pilote maintenant tous les types de contrôleurs USB : en plus de l’USB 3 (xHCI), les contrôleurs USB 2 (EHCI) des ordinateurs d’avant 2012 environ, et les contrôleurs USB 1.1 (OHCI et UHCI) qui les accompagnent. Les claviers, les souris, les concentrateurs et les clés USB fonctionnent sur chacun d’eux, branchés à tout moment.
- Cela comprend le contrôleur USB 2.0 de VirtualBox (EHCI avec OHCI).
- Les clés USB sont prêtes plus tôt : en monter une prend maintenant environ une seconde, sur n’importe quel contrôleur.

## Claviers, souris, pavés tactiles et manettes de jeu
- **Claviers :** chaque touche compte, peu importe combien vous en tenez enfoncées; les touches de volume, de lecture, de luminosité et de navigation fonctionnent; et le voyant du verrouillage des majuscules suit la touche sur tous vos claviers branchés.
- **Souris :** les boutons latéraux reculent et avancent, et les souris à molette inclinable ou à deuxième molette défilent sur le côté.
- **Pavés tactiles** (USB) : touchez pour cliquer, touchez avec deux doigts pour un clic secondaire, touchez puis touchez de nouveau pour glisser, appuyez avec deux doigts pour un clic secondaire, et défilez avec deux doigts — en douceur, le contenu continuant de glisser après un geste rapide. Le défilement suit vos doigts, comme sur un écran tactile.
- **Manettes de jeu :** les manettes Xbox 360, Xbox One et Series, PlayStation (DualShock 4 et DualSense), Switch Pro et les autres manettes USB fonctionnent, toutes avec la même disposition de boutons pour les jeux, et vibrent sur les manettes qui le peuvent.
- Les souris PS/2 à boutons latéraux fonctionnent aussi.
- **Réglages** offre de nouveaux panneaux **Trackpad**, **Mouse** et **Game Controllers** : toucher pour cliquer, défilement naturel, vitesse du suivi et du pointeur, et chaque manette branchée avec un aperçu en direct de ses boutons et un test de vibration.

## Pour les développeurs
- **Le WaveSDK** est le nouveau nom du SDK Constellation. La commande `wavesdk` remplace `constellation`, qui fonctionne toujours; les paquets créés pour les SDK 0.6 à 0.8 s’installent encore, et les nouveaux indiquent le SDK 0.9.
- **WaveUI**, dans le WaveSDK, est maintenant la façon d’écrire les applications WaveOS : décrivez l’interface à partir de l’état de votre application, et WaveUI s’occupe de la mise en page, du dessin, des entrées, du focus, du défilement, des menus, des feuilles et des animations. Les nouveaux projets de Studio et de `wavesdk new` sont des applications WaveUI, et l’exemple WaveUI Gallery présente toutes les vues.
- Learn propose trois nouveaux guides pour les développeurs : [WaveUI](../../handbook/0.7/fr-CA/developer-waveui.md), [WaveSDK](../../handbook/0.7/fr-CA/developer-wavesdk.md) et les [Directives de design](../../handbook/0.7/fr-CA/developer-design.md).
- `cargo xtask run --gpu virtio` démarre avec un écran virtio piloté par le pilote de Linux; `cargo xtask test --gpu virtio` vérifie aussi que l’écran s’est allumé.
- `cargo xtask run --wifi-sim` ajoute les réseaux Wi-Fi simulés.
- `cargo xtask run --usb ehci` (ou `ehci-ohci`, `ohci`, `uhci`) choisit le contrôleur USB de la machine virtuelle.
- Les applications peuvent lire les manettes de jeu avec `corekit::gamepad` : les manettes branchées, leurs boutons, leurs manches et leurs gâchettes dans la disposition standard, et la vibration. Les évènements de défilement donnent aussi la valeur exacte, pour un défilement en douceur.
- La compilation des pilotes graphiques demande clang. Sans lui, le noyau se compile sans ces pilotes.

## Limites connues
Les pavés tactiles intégrés des portables se branchent surtout en I²C, que WaveOS ne pilote pas encore; plusieurs fonctionnent aussi comme simple pointeur PS/2, sans gestes. Les claviers, souris et manettes Bluetooth ne sont pas pris en charge. Les nouveaux contrôleurs USB (EHCI, OHCI, UHCI) n’ont été essayés que dans des machines virtuelles. Aucune vraie carte Wi-Fi ne fonctionne encore : le pilote Intel s’arrête avant de démarrer la radio, et rien de ce pilote n’a encore tourné sur du vrai matériel. Le pilote graphique de Linux fonctionne dans les machines virtuelles (virtio); les pilotes pour les cartes graphiques AMD, Intel et NVIDIA, ainsi que l’accélération 3D, ne sont pas encore là. Dans la machine d’essai de cette mise à jour, le micrologiciel ne sortait pas de la suspension; le test de suspension a donc été sauté, comme il l’a été sur le code de la version 0.8.0.

## Résultats de validation
`cargo xtask lint`, les tests hôte décrits dans [Compiler et tester](../../handbook/0.7/fr-CA/developer-build.md) (qui comprennent maintenant les tests de la bibliothèque Wi-Fi, vérifiés avec les vecteurs d’essai IEEE 802.11 et une implémentation indépendante) et les autotests du noyau sur deux démarrages (38 tests, dont l’autotest de la couche de compatibilité Linux et une connexion WPA2 avec DHCP sur le réseau simulé) ont été exécutés sur ce changement, de même que `cargo xtask test --gpu virtio`. Les tests de l’USB et des entrées ont été exécutés sur chaque contrôleur USB (`--usb xhci`, `ehci`, `ehci-ohci`, `ohci`, `uhci`), avec des appareils virtuels branchés par redirection USB : un clavier à touches multimédias, un pavé tactile de précision, une DualShock 4 et une manette Xbox 360; la bibliothèque d’entrées a ses propres tests sur les descripteurs de ces appareils. Les captures d’écran de `docs/screenshots/0.9` proviennent de QEMU à 1280 × 800.
