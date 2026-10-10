# Nebula
Consultez des sites simples et des documents HTML locaux.

## Utiliser l’application
1. Ouvrez Nebula et appuyez sur Ctrl+L pour saisir une adresse ou une recherche.
2. Alt+Gauche/Droite parcourt l’historique et Ctrl+R recharge la page.
3. Ctrl+F recherche du texte; Ctrl+plus/moins/0 règle le zoom.
4. Ouvrez les téléchargements dans Files; vérifiez la destination affichée par le navigateur.
5. Ctrl+T ou le bouton + ouvre un onglet (maximum de huit). Ctrl+Tab / Ctrl+Maj+Tab change d’onglet; Ctrl+1–8 sélectionne un onglet et Ctrl+9 sélectionne le dernier.
6. Ctrl+W ferme l’onglet actif. Ctrl+Maj+T rouvre une adresse récemment fermée en rechargeant la page. Ctrl+clic ouvre un lien dans un nouvel onglet.
7. Le bouton d’accueil ou Alt+Début revient à la page de départ. Essayez les démonstrations hors ligne Layout gallery et Pulsar playground.

Les onglets en arrière-plan mettent leurs minuteries JavaScript en pause et libèrent leur cache de mise en page. Le document, la position de lecture, le zoom et le texte saisi restent disponibles au retour. Chaque onglet conserve jusqu’à 100 entrées d’historique et 12 Mio de pixels d’images décodées; les très grandes images sont réduites. Fermer la fenêtre ou utiliser Alt+F4 ferme tous ses onglets. Les onglets ne sont pas restaurés après avoir quitté l’application.

## Résultat attendu et dépannage
Nebula prend en charge HTML/CSS, flexbox, grid, les importations imbriquées de feuilles de style avec une limite de chargement, les attributs media adaptatifs, les images matricielles courantes, HTTP et TLS. Pulsar fournit JavaScript, les événements DOM, les minuteries, le stockage et les requêtes réseau; les témoins sont pris en charge. Ce moteur maison offre une compatibilité Web partielle : les sites complexes peuvent rester incomplets. Les modules JavaScript, WebP et SVG ne sont pas pris en charge. Les erreurs TLS ne sont pas contournées.

Voir [Bureau et fenêtres](desktop-windows.md) pour les raccourcis communs et [Récupération](reference-recovery.md) si l’application ne s’ouvre pas.
