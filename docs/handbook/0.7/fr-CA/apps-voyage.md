# Voyage
Naviguez sur le Web avec WebKit, le moteur de Safari : JavaScript au complet et CSS moderne.

## Utiliser l’application
1. Ouvrez Voyage depuis le dock, le lanceur ou Seek. Les liens Web de Learn et d’About et les fichiers `.html` s’y ouvrent aussi.
2. Tapez une adresse ou une recherche dans le champ du haut et appuyez sur Entrée (Ctrl+L ou F6 vous y amène). Les recherches vont à DuckDuckGo.
3. Ctrl+T ouvre un onglet et Ctrl+W le ferme; Ctrl+Tab et Ctrl+Maj+Tab passent d’un onglet à l’autre, et on peut faire glisser les onglets pour les réordonner. Les liens qui ouvrent une nouvelle fenêtre ouvrent un onglet.
4. Utilisez les flèches, Alt+Gauche/Droite ou les boutons latéraux d’une souris pour reculer et avancer, et Ctrl+R ou F5 pour actualiser. Ctrl+F cherche du texte dans la page (Entrée pour le suivant, Ctrl+Maj+G pour le précédent, Échap pour fermer), et Ctrl+plus, Ctrl+moins et Ctrl+0 changent le zoom.
5. Les téléchargements vont dans votre dossier Downloads, que Files affiche. Les témoins et les données des sites sont conservés d’une visite à l’autre.

## Résultat attendu et dépannage
Voyage est le navigateur de WaveOS autour de WebKitGTK, la version de WebKit de Debian, qui fonctionne sur la couche Linux de WaveOS : les sites voient un navigateur comme Safari, avec JavaScript (JavaScriptCore), la mise en page CSS, les polices Web, la vidéo et l’audio. Il suit l’apparence claire ou sombre de WaveOS à son démarrage, et les pages qui offrent un style sombre l’obtiennent. Il met environ quinze secondes à afficher une page dans une machine virtuelle qui émule le processeur (comme sur les Mac à puce Apple), la moitié du temps de Firefox, et bien moins là où la machine virtuelle utilise la virtualisation du processeur lui-même. Ses onglets partagent un seul processus Web : les nouveaux onglets s’ouvrent vite.

Voyage vient avec les paquets Linux du disque. S’ils n’y sont pas, [Firefox](apps-firefox.md) s’ouvre à sa place quand il est sur le disque, sinon [NetSurf](apps-netsurf.md). Le bac à sable de WebKit est désactivé sur WaveOS, qui n’a pas encore les fonctions Linux qu’il demande : naviguez sur des sites de confiance. Les pages sont dessinées par le logiciel.

Consultez [Bureau et fenêtres](desktop-windows.md) pour les raccourcis communs et [Récupération](reference-recovery.md) si l’application ne s’ouvre pas.
