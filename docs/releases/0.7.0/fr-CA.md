# WaveOS Aurora 0.7.0

5 octobre 2026

Cette mise à jour présente Learn, de nouvelles façons d’organiser les fenêtres et de passer de l’une à l’autre, ainsi que des améliorations du rendu et de la navigation au clavier.

## Learn
- Consultez le guide complet de WaveOS hors ligne, en anglais ou en français canadien.
- Trouvez les guides avec la recherche dans le texte complet, utilisez le sommaire et ajustez la taille de lecture.
- Changez de langue en conservant le même article.

## Bureau
- Passez d’une fenêtre ouverte ou réduite à une autre avec le sélecteur visuel Alt+Tab.
- Organisez les fenêtres avec les aperçus aux bords de l’écran et les raccourcis Super+flèche.
- Retrouvez les dimensions libres après l’ancrage et conservez la disposition lorsque l’affichage change.

## Accessibilité
- Parcourez la recherche, les pages et les commandes de Settings au clavier.
- Utilisez des touches cohérentes pour activer les commandes et régler les curseurs dans Settings et Control Center.
- Appliquez les préférences existantes de contraste, de transparence et de mouvement aux nouvelles commandes.

## Performance
- Les mises à jour localisées admissibles redessinent moins de zones du bureau. Dans une saisie contrôlée avec Notes, le temps médian de composition et de copie à l’écran est passé de 47 à 10 ms, avec 93.0% moins de pixels peints par lot d’images par rapport à la base de développement `1011e1a`.
- Le déplacement et le changement de fenêtre ont montré un p95 plus élevé. Consultez les [mesures et limites](../../handbook/0.7/fr-CA/reference-validation.md#resultats-du-bureau-5-octobre-2026) avant d’appliquer ces résultats à d’autres charges.

## Correctifs
- Corrige un problème de retransmission TCP pouvant bloquer les transferts sur un système à un seul processeur.

## Limites connues
Le compilateur et le débogueur natifs demeurent indisponibles. Les limites du navigateur et du matériel s’appliquent toujours. Consultez le [guide complet](../../handbook/0.7/fr-CA/start-welcome.md) et le [guide de validation](../../handbook/0.7/fr-CA/reference-validation.md).
