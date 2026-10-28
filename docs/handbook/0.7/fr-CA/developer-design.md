# Directives de design
Les meilleures applications WaveOS sont calmes, claires et rapides. Ces directives décrivent l’apparence et le comportement des applications intégrées, pour que la vôtre s’y sente chez elle. WaveUI s’occupe de l’essentiel lorsque vous utilisez ses vues et ses styles standard.

## Principes
- **Clarté.** Le contenu passe en premier. Utilisez beaucoup d’espace, une hiérarchie typographique nette et peu de couleurs, pour que l’important ressorte.
- **Profondeur.** Les barres latérales et les barres d’outils utilisent des matériaux translucides, comme de l’eau vive sur le bureau. Les cartes et les feuilles soulèvent le contenu qu’elles portent. Servez-vous de la profondeur pour montrer la structure, pas pour décorer.
- **Discrétion.** L’interface soutient le travail sans le gêner. Préférez les commandes familières aux commandes personnalisées et gardez le cadre sobre.
- **Respect.** Suivez les réglages de la personne : apparence, couleur d’accent, taille du texte, Réduire les animations et Réduire la transparence. Ne perdez jamais son travail.

## Mise en page
- Placez la navigation dans une barre latérale au bord de début avec `SplitView`. Affichez le contenu de l’élément choisi du côté de fin.
- Placez les actions de la vue courante dans une barre d’outils en haut. Mettez l’action la plus fréquente à l’extrémité de fin.
- Regroupez les réglages apparentés dans des cartes avec `Section::card`. Donnez à chaque groupe un titre court et ajoutez une note lorsqu’un réglage mérite une explication.
- Utilisez l’échelle d’espacement : 8 entre les éléments liés, 16 entre les groupes et 24 autour du contenu d’une fenêtre.
- Alignez le texte sur un bord de début commun. Évitez de centrer les longs textes.
- Faites fonctionner votre fenêtre à sa taille minimale. Laissez les listes et les éditeurs occuper l’espace supplémentaire lorsqu’elle s’agrandit.

## Typographie
WaveOS utilise Manrope. Utilisez les styles de texte plutôt que des tailles fixes :

| Style | Usage |
|---|---|
| Large Title | Le titre d’une page ou un grand nombre |
| Title | Le titre du contenu d’une fenêtre |
| Title 2 | Les titres de section |
| Headline | Les titres de rangée et le texte mis en évidence |
| Body | Tout le reste |
| Callout et Subheadline | Les explications secondaires et les détails de rangée |
| Footnote et Caption | Les petits caractères et les libellés des petites commandes |

Écrivez les libellés en minuscules, sauf la première lettre. Gardez-les courts, et faites-leur dire ce qui va se passer.

## Couleur
- Utilisez les couleurs sémantiques : `Label` pour le texte principal, `SecondaryLabel` pour le texte d’appoint, `Background` et `SecondaryBackground` pour les surfaces. Elles s’adaptent automatiquement aux apparences claire et sombre.
- Réservez la couleur d’accent à la sélection, aux liens et à l’unique action principale d’une vue. N’en peignez pas des zones entières.
- N’utilisez `Red` que pour les actions destructives et les erreurs.
- Ne vous fiez jamais à la couleur seule. Associez les couleurs d’état à un symbole ou à du texte.
- Vérifiez le contraste dans les deux apparences, et avec Augmenter le contraste.

## Commandes
- **Boutons :** le style proéminent pour la seule action la plus probable, le style bordé pour les autres, le style destructif pour les suppressions. Dans les barres d’outils, utilisez des symboles avec une infobulle `help`.
- **Interrupteurs :** ils activent ou désactivent un réglage immédiatement. Préférez-les aux cases à cocher dans les réglages.
- **Sélecteurs :** segmentés pour deux à cinq choix courts, en menu pour les listes plus longues.
- **Champs de texte :** donnez-leur un texte indicatif qui décrit le contenu attendu, pas une répétition du libellé.
- **Curseurs :** pour les valeurs continues comme le volume ou la luminosité. Affichez la valeur lorsqu’elle compte.
- **Progression :** une barre lorsque vous connaissez la quantité restante, une roue sinon. Si le travail dure plus de quelques secondes, permettez de l’annuler.

## Alertes et feuilles
- N’utilisez une alerte que lorsqu’une décision immédiate est nécessaire, par exemple avant de supprimer des fichiers ou d’abandonner des changements. N’en utilisez pas pour une réussite ordinaire.
- Formulez le titre comme une question (« Supprimer 3 éléments? »). Nommez les boutons avec des verbes (« Supprimer », « Annuler »), pas « Oui » et « Non ».
- Marquez les boutons destructifs comme tels, et faites d’Annuler le choix par défaut lorsque l’action est irréversible.
- Utilisez une feuille pour une courte tâche liée à la fenêtre, comme renommer un élément ou créer une connexion. Placez ses boutons dans le coin inférieur de fin.

## Symboles et icônes
- Utilisez les symboles du système avec `Image::symbol` dans les barres d’outils, les barres latérales et les rangées. Ils s’accordent avec le poids du texte et suivent l’accent et l’apparence.
- Donnez à l’icône de votre application une forme simple sur un carré arrondi, comme les icônes de Harbor. Elle doit rester lisible à 16 pixels.
- Donnez une étiquette d’accessibilité à chaque bouton formé d’un seul symbole.

## Mouvement
- Gardez les animations courtes (moins de 250 millisecondes) et utiles : elles montrent d’où vient une chose ou où elle va.
- N’animez jamais lorsque Réduire les animations est activé. WaveUI le fait pour ses propres animations; vérifiez `env.reduce_motion` pour les vôtres.
- Ne déplacez pas le contenu que la personne lit ou saisit.

## Clavier
- Tout doit fonctionner sans pointeur. Tab et Maj+Tab passent d’une commande à l’autre, Espace et Retour les activent, Échap ferme les menus et devrait aussi annuler vos feuilles.
- Prenez en charge les raccourcis standard : Ctrl+N, Ctrl+O, Ctrl+S, Ctrl+W, Ctrl+Z, Ctrl+C, Ctrl+V et Ctrl+F.
- Affichez les raccourcis dans les menus pour qu’on puisse les apprendre.

## Rédaction
- Vouvoyez les gens. Soyez direct, cordial et bref.
- Dans les messages d’erreur, dites ce qui s’est passé et quoi faire ensuite : « Le fichier n’a pas pu être enregistré, car le disque est plein. Supprimez des fichiers et réessayez. »
- Ne blâmez personne, et n’employez ni jargon ni code d’erreur dans le message principal.
- Traduisez votre application dans le plus de langues possible. WaveOS est offert en anglais et en français canadien.

Consultez [WaveUI](developer-waveui.md) pour les vues qui mettent ces directives en œuvre.
