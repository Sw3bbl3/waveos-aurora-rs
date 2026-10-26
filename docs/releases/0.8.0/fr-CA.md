# WaveOS Aurora 0.8.0

Pas encore publiée.

WaveOS adopte une nouvelle apparence, **l’eau vive** : des bulles de verre en haut de l’écran, Crest au centre, Harbor qui flotte en bas et des fenêtres sans barre de titre. Le démarrage affiche le logo WayV, puis demande votre mot de passe, et chaque surface et chaque app ont été redessinées en mode clair et en mode sombre. Cette mise à jour comprend aussi tous les changements annoncés pour la version 0.7.1.

## Démarrage
- Un écran de démarrage affiche le logo WayV, la progression de chaque étape et la version, dans l’apparence claire ou sombre que vous avez choisie.
- Au premier démarrage, **Shore** crée votre compte : votre nom, un mot de passe et un indice, puis l’apparence et le fond d’écran. Le mot de passe est salé et haché (PBKDF2-HMAC-SHA256); il n’est jamais conservé tel quel.
- Ensuite, Shore demande le mot de passe au démarrage, après **Verrouiller l’écran** (Super L ou le menu WayV) et après la suspension. **Réglages → Connexion** permet d’ouvrir la session automatiquement au démarrage.

## La barre du haut
- La bulle de gauche contient le menu WayV et l’app utilisée; celle de droite contient **Seek** et l’anneau d’état.
- **Crest**, au centre, affiche l’heure. Pointez-le pour voir la date; il s’assombrit pour montrer le volume, les changements d’alimentation et les nouvelles notifications. Cliquez dessus pour ouvrir **Drift**.
- L’anneau d’état ouvre **Current** : le réseau, Calme, l’apparence, le verrouillage, le son et la batterie.
- **Réglages → Barre et Crest** permet de masquer Crest (l’heure et la date passent dans la bulle de droite) et d’afficher le niveau de la batterie à côté de l’anneau.

## Harbor
- Harbor remplace le Dock. Trois baies contiennent Shoal et vos apps épinglées, les autres apps ouvertes, ainsi que les piles Téléchargements et Documents avec la Corbeille.
- Une ligne d’eau lumineuse s’affiche sous les apps ouvertes, plus large sous celle que vous utilisez. Pointer une icône la soulève et affiche son nom; s’attarder sur une app ouverte montre ses fenêtres, et un clic en ramène une au premier plan.
- Harbor s’enfonce quand la fenêtre au premier plan l’atteint, et remonte quand le pointeur touche le bas de l’écran.

## Fenêtres
- Les fenêtres n’ont plus de barre de titre. Approchez le pointeur du bord supérieur d’une fenêtre : sa poignée devient une pastille. Faites-la glisser pour déplacer la fenêtre, double-cliquez pour l’agrandir, ou utilisez ses boutons pour la réduire, l’agrandir ou la fermer.
- Maintenez Super et faites glisser n’importe où dans une fenêtre pour la déplacer.
- Faites glisser une fenêtre en haut de l’écran et **Moorings** propose six dispositions : moitiés, tiers et quarts. La fenêtre active brille d’une ligne d’eau.

## Shoal, Seek, Drift et Glide
- **Shoal** affiche toutes les apps en plein écran, avec une recherche et des groupes (Travail, Création, Système, Installées). Appuyez sur Super et relâchez, ou cliquez sur la première icône de Harbor.
- **Seek** (Super+Espace) trouve des apps, des fichiers, des réglages et fait des calculs.
- **Drift** descend de Crest avec la date, un calendrier, Calme et vos notifications.
- **Glide** (Alt+Tab) montre chaque fenêtre en image.

## Apps
- **À propos de cet ordinateur** affiche le symbole WVOS, la version et votre machine, conçus par WayV. Réglages → À propos comporte une carte WVOS.
- Des apps changent de nom : **Lens** (anciennement Aperçu), **Pulse** (anciennement Moniteur d’activité), **Installateur** (anciennement GINA Apps) et **Studio** (anciennement Constellation Studio). Ne pas déranger devient **Calme**.
- Manrope est la police de l’interface. Chaque app a une nouvelle icône, et trois nouveaux fonds d’écran — Tide, Aurora et Dunes — existent en version jour et nuit. La couleur d’accentuation peut suivre le fond d’écran.

## Limites connues
WaveOS n’a pas de pilote Wi-Fi ni Bluetooth : Current n’affiche que le réseau filaire. Il y a un seul compte par ordinateur. Les durées d’image mesurées pendant le développement proviennent d’une émulation logicielle; cette mise à jour n’annonce donc aucun gain de performance. Consultez le [guide complet](../../handbook/0.7/fr-CA/start-welcome.md); il montre encore le bureau de la version 0.7.

## Résultats de validation
`cargo xtask lint`, la suite de tests hôte décrite dans [Compiler et tester](../../handbook/0.7/fr-CA/developer-build.md) et les autotests du noyau (`cargo xtask test`, deux démarrages, y compris le nouveau test de hachage du mot de passe) ont été exécutés sur ce changement. Chaque élément du nouveau bureau a été vérifié dans QEMU à 1280 × 800; les captures d’écran de `docs/screenshots/0.8` proviennent de ces essais.
