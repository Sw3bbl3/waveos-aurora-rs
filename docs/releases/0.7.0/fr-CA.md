# WaveOS Aurora 0.7.0

21 octobre 2026

Cette version ajoute un guide hors ligne consultable, de nouveaux parcours pour le bureau et les fichiers, ainsi que le moteur JavaScript Pulsar et des médias natifs dans le navigateur.

## Learn
- Consultez 60 articles hors ligne répartis entre l’anglais et le français canadien : guides d’utilisation, instructions de développement et notes de version.
- Recherchez dans le texte complet, suivez le sommaire, ajustez la taille de lecture et changez de langue en conservant l’article.

## Bureau et Files
- Choisissez une fenêtre ouverte ou réduite avec le sélecteur visuel Alt+Tab.
- Placez les fenêtres avec les aperçus de bord et Super+flèches, puis rétablissez leur taille flottante.
- Utilisez jusqu’à huit onglets Files et des transferts en arrière-plan avec progression, choix de conflit et annulation.

## Nebula et Pulsar
- Exécutez le JavaScript pris en charge avec les événements DOM, les minuteries, fetch, le stockage, les témoins et une invite interactive dans Terminal.
- Utilisez les onglets du navigateur, les événements de stockage partagé, le canevas 2D et d’autres interfaces DOM/CSS.
- Réutilisez les scripts compilés et les caches de propriétés; reportez la compilation des fonctions admissibles jusqu’à leur utilisation.
- Essayez la lecture native expérimentale MP4 H.264/AAC-LC et les sources multimédias MP4 fragmentées.

## Réseau et accessibilité
- IPv6 ajoute la découverte de voisins, SLAAC, ICMPv6, TCP et UDP à la pile IPv4 existante.
- Recherchez des pages et des commandes individuelles dans Settings; utilisez des touches cohérentes pour l’activation et les curseurs.
- Appliquez les préférences existantes de mouvement, de transparence et de contraste au bureau et à Learn.

## Performance
- Les mises à jour localisées du bureau redessinent les zones modifiées admissibles. Dans la comparaison de développement identifiée du 5 octobre, le temps médian de composition/copie de Notes est passé de 47 à 10 ms, avec 93.0% moins de pixels peints par lot.
- Ces mesures historiques décrivent les compilations nommées, avant l’intégration ultérieure du navigateur et des médias. Le p95 du déplacement et du changement de fenêtre a augmenté dans cette comparaison. Consultez la [portée des mesures et les données brutes](../../handbook/0.7/fr-CA/reference-validation.md#portee-des-mesures); aucun pourcentage d’accélération supplémentaire n’est annoncé pour la version combinée.

## Correctifs
- Active la réception multidiffusion des adaptateurs Intel pour transmettre les annonces de routeur et la découverte de voisins IPv6 à la pile réseau.
- Corrige les accusés de réception des retransmissions TCP qui pouvaient bloquer les transferts à un seul processeur.
- Protège les valeurs JavaScript entre les rappels du navigateur et les tâches Promise pour éviter une collecte prématurée.
- Conserve une image de démarrage optique EFI limitée tout en gardant séparément les symboles complets de débogage du noyau sur l’hôte.

## Limites connues
Les outils natifs de compilation et de débogage Rust restent absents. La compatibilité Web est partielle : modules JavaScript, BigInt, Intl, DRM et lecture générale YouTube/TikTok ne sont pas établis. Les vérifications QEMU ne certifient ni le matériel physique ni VirtualBox. Consultez le [guide complet](../../handbook/0.7/fr-CA/start-welcome.md) et le [guide de validation](../../handbook/0.7/fr-CA/reference-validation.md).
