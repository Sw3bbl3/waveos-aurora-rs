# Créer une interface avec WaveUI
WaveUI est le cadriciel d’interface du WaveSDK. Toutes les applications fournies avec WaveOS l’utilisent. Votre application décrit son interface en fonction de son état, et WaveUI garde la fenêtre à jour. Il s’occupe de la mise en page, du dessin dans le style de WaveOS (clair et sombre, couleur d’accent, mouvement réduit), du pointeur, du clavier, du focus, du défilement, des menus et des feuilles.

> Si vous connaissez SwiftUI ou Elm, vous connaissez déjà le principe : un état, des messages, `update` et `view`.

## Votre première application
Une application est un type qui implémente `App`. Elle comporte quatre parties : son état, un type `Message` pour ce qui peut arriver, `update` pour modifier l’état et `view` pour décrire l’interface.

```rust
#![no_std]
#![no_main]
extern crate alloc;
use waveui::prelude::*;

struct Counter { count: i64 }

#[derive(Clone)]
enum Msg { Add, Reset }

impl App for Counter {
    type Message = Msg;
    fn title(&self) -> String { "Compteur".into() }

    fn update(&mut self, msg: Msg, _cx: &mut Context<Msg>) {
        match msg {
            Msg::Add => self.count += 1,
            Msg::Reset => self.count = 0,
        }
    }

    fn view(&self, _env: &Env) -> View<'_, Msg> {
        VStack::new()
            .push(Text::new(format!("{}", self.count)).style(TextStyle::LargeTitle))
            .push(HStack::new()
                .push(Button::new("Réinitialiser").action(Msg::Reset))
                .push(Button::new("Ajouter").action(Msg::Add).button_style(ButtonStyle::Prominent)))
            .spacing(16)
            .fill()
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    waveui::run(Counter { count: 0 })
}
```

Lorsqu’on appuie sur **Ajouter**, WaveUI envoie `Msg::Add` à `update`, appelle de nouveau `view` et redessine la fenêtre. Vous ne déplacez jamais les commandes et n’invalidez jamais de rectangles vous-même.

## Vues
Les vues sont de petites valeurs que vous reconstruisez à chaque mise à jour. Leur construction est peu coûteuse.

| Type | Vues |
|---|---|
| Contenu | `Text`, `Image` (images, icônes d’application et symboles du système), `ProgressView`, `Divider` |
| Commandes | `Button`, `Toggle`, `Slider`, `Stepper`, `Picker`, `TextField`, `TextEditor` |
| Mise en page | `VStack`, `HStack`, `ZStack`, `Spacer`, `Grid`, `ScrollView`, `List`, `SplitView` |
| Regroupement | `Section` (simple, titrée ou carte), `row`, `row_detail`, `sidebar_item`, `toolbar` |
| Dessin | `Canvas`, pour les graphiques, les documents et les jeux |

Les piles placent leurs enfants les uns après les autres. Un `Spacer` occupe l’espace libre; `HStack::new().push(title).push(Spacer::new()).push(button)` place donc le bouton au bord de fin. `List` ne construit que les rangées visibles et convient à des milliers d’éléments.

## Modificateurs
Un modificateur retourne une vue modifiée; on peut donc les enchaîner :

```rust
Text::new("Stockage")
    .style(TextStyle::Headline)
    .foreground(Color::SecondaryLabel)
    .padding(12)
    .width(240)
```

Les plus courants sont :
- **Mise en page :** `padding`, `frame`, `fill`, `grow` et `align`.
- **Apparence :** `background`, `material`, `border`, `shadow`, `foreground`, `tint`, `corner_radius` et `opacity`.
- **Comportement :** `on_tap`, `on_double_tap`, `on_hover`, `context_menu`, `help` (une infobulle), `shortcut`, `draggable`, `on_drop` et `disabled`.
- **Identité :** `id` donne à une vue une identité stable; son focus et sa position de défilement survivent aux changements des vues voisines.

## Le système de design
Utilisez les styles sémantiques plutôt que des valeurs brutes. Ils suivent la taille du texte, l’apparence et la couleur d’accent choisies, et ils s’accordent avec le reste de WaveOS.

- **Styles de texte :** `LargeTitle`, `Title`, `Title2`, `Headline`, `Body`, `Callout`, `Subheadline`, `Footnote`, `Caption` et `Monospaced`.
- **Couleurs :** `Label`, `SecondaryLabel`, `TertiaryLabel`, `Accent`, `Background`, `SecondaryBackground`, `Red`, `Green` et les autres couleurs du système.
- **Espacement :** `space::S` (8), `space::M` (12), `space::L` (16) et `space::XL` (24).
- **Rayons des coins :** `radius::CONTROL`, `radius::CARD` et `radius::SHEET`.

## Context : communiquer avec le système
`update` reçoit un `Context`. Utilisez-le pour :
- Envoyer des messages : `send` et `after` (une minuterie).
- Ouvrir des applications et des documents : `open_app` et `open_file`.
- Utiliser le presse-papiers : `copy` et `paste`.
- Publier une notification : `notify`.
- Fermer la fenêtre : `close`.
- Gérer le focus et le défilement : `focus`, `scroll_to` et la lecture du texte d’un champ.

Pour le travail en arrière-plan (un tube, le réseau ou l’horloge), retournez un intervalle depuis `poll_interval` et faites le travail dans `poll`. `poll` doit retourner vrai seulement si quelque chose a changé.

## Feuilles, alertes et menus
Retournez une vue depuis `sheet` pour afficher une boîte de dialogue par-dessus la fenêtre. La fenêtre ignore les entrées tant que la feuille est affichée. `Alert` construit la question standard, avec des rôles pour les boutons par défaut, d’annulation et destructifs. `Button::menu` crée un menu déroulant, et `context_menu` ajoute un menu contextuel à n’importe quelle vue.

## Clavier et accessibilité
WaveUI déplace le focus avec Tab et Maj+Tab, active la commande ciblée avec Espace et Retour, et fait défiler la vue ciblée pour la rendre visible. Donnez un `shortcut` aux actions qui le méritent. Ajoutez `accessibility_label` aux images et aux boutons formés d’un seul symbole. WaveUI respecte Réduire les animations pour ses propres animations; votre application peut lire `env.reduce_motion` pour les siennes.

## Étapes suivantes
- Lisez les [Directives de design](developer-design.md) avant de concevoir une fenêtre.
- Consultez [WaveSDK](developer-wavesdk.md) pour les projets, Studio et la commande `wavesdk`.
- Consultez [Format des paquets GINA](developer-packages.md) pour distribuer votre application.
