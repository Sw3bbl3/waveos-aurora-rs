# Wave Play
Jouez avec une manette : vos jeux et vos applications en grandes rangées, en plein écran.

## Lancer Wave Play
1. Branchez une manette de jeu USB : Xbox, PlayStation, Switch Pro ou toute manette HID.
2. Un avis confirme la connexion, puis le bureau se transforme en Wave Play : les fenêtres reculent, la barre et le dock glissent hors de l'écran, et des rangées de jeux et d'applications apparaissent.
3. Sans manette, choisissez Wave Play dans le menu WaveOS, ou cherchez Wave Play avec Super+Espace.

Pour que Wave Play ne s'ouvre plus de lui-même, désactivez **Ouvrir au branchement d'une manette** dans Réglages → Wave Play. Une manette affiche alors un avis; appuyez sur A pour entrer.

## Vous déplacer
| Bouton | Action |
| --- | --- |
| Croix directionnelle ou stick gauche | Passer d'une tuile à l'autre; maintenir pour aller plus vite |
| A | Ouvrir la tuile (ou revenir à ce qui est en cours) |
| Y | Chercher avec une bande de lettres à l'écran |
| LB et RB | Sauter à la rangée du dessus ou du dessous |
| Menu | Ouvrir le menu rapide |
| Affichage | Revenir au bureau |
| Accueil | Le menu rapide, de partout |

Sur les manettes PlayStation, A est la croix; sur les manettes Nintendo, A est le bouton de droite, comme sur la console. Les indications au bas de l'écran utilisent les symboles de votre manette.

Les rangées sont **Continuer** (ce que vous avez lancé récemment et ce qui tourne encore), **Jeux**, **Applications** et **Réglages**. Chaque rangée se souvient de votre position. Maintenir une direction au bout d'une rangée s'y arrête; appuyer de nouveau revient à l'autre bout.

Le clavier fonctionne aussi : les flèches déplacent, Entrée ouvre, Échap revient, Tab cherche et Super+Échap ouvre le menu rapide.

## Jouer
Ouvrir une tuile l'agrandit jusqu'à remplir l'écran pendant que le programme démarre. Les jeux et applications lancés depuis Wave Play s'affichent en plein écran. Doom lit la manette directement : le stick gauche et la croix déplacent, la gâchette droite tire, A ouvre les portes, les boutons supérieurs font des pas de côté, Affichage montre la carte et Menu ouvre le menu de Doom. Les programmes qui ne lisent pas les manettes reçoivent la croix comme flèches, A comme Entrée et B comme Échap.

## Le menu rapide
Appuyez sur le bouton Accueil (ou Super+Échap) par-dessus un jeu, Wave Play ou le bureau. Ce qui est dessous s'assombrit et devient flou, et un panneau glisse à l'écran :

- **Reprendre** ferme le menu.
- **Accueil de Wave Play** revient à vos rangées; le jeu continue et attend dans Continuer.
- **Fermer** quitte le jeu ou l'application.
- **Volume** se règle avec gauche et droite.
- **Aller au bureau** quitte Wave Play.
- **Alimentation** propose la suspension, le redémarrage et l'arrêt.

Le panneau indique aussi l'heure, la pile, votre manette et la vitesse d'affichage (images par seconde et millisecondes par image).

## Revenir au bureau
Appuyez sur Affichage, choisissez la tuile Bureau ou débranchez la manette. Tout revient exactement à sa place : vos fenêtres, leur ordre et celle qui était devant. Appuyer de nouveau sur Affichage pendant le retour au bureau ramène à Wave Play à partir du mouvement en cours.

## Réglages
Réglages → Wave Play propose **Ouvrir au branchement d'une manette**, **Sons et vibrations** (de légers clics pendant les déplacements et une petite vibration sur les manettes qui en ont) et **Réduire les animations**, qui change d'écran sans la transformation animée. Réduire les animations est partagé avec Accessibilité.

## Dépannage
Si une manette n'est pas reconnue, vérifiez-la dans Réglages → Manettes de jeu. Les programmes installés avec les paquets Linux apparaissent dans Jeux quand leur entrée de menu les décrit comme des jeux, et Steam apparaît quand il est installé. Les développeurs peuvent ajouter des tuiles en plaçant un fichier `.play` dans `/Settings/Play/Library` (voir les notes sur Wave Play dans le guide WaveSDK).
