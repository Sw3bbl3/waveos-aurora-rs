# Build an interface with WaveUI
WaveUI is the interface framework of the WaveSDK. Every app that comes with WaveOS is written with it. Your app describes its interface as a function of its state, and WaveUI keeps the window in step with that state. It handles layout, drawing in the WaveOS look (light and dark, the accent colour, reduced motion), the pointer, the keyboard, focus, scrolling, menus and sheets.

> If you know SwiftUI or Elm, you already know the shape: state, messages, `update` and `view`.

## Your first app
An app is a type that implements `App`. It has four parts: its state, a `Message` type for what can happen, `update` to change the state, and `view` to describe the interface.

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
    fn title(&self) -> String { "Counter".into() }

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
                .push(Button::new("Reset").action(Msg::Reset))
                .push(Button::new("Add").action(Msg::Add).button_style(ButtonStyle::Prominent)))
            .spacing(16)
            .fill()
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    waveui::run(Counter { count: 0 })
}
```

When someone presses **Add**, WaveUI sends `Msg::Add` to `update`, then calls `view` again and redraws. You never move controls around or invalidate rectangles yourself.

## Views
Views are small values that you build fresh on every update. Building them is cheap.

| Kind | Views |
|---|---|
| Content | `Text`, `Image` (pictures, app icons and system symbols), `ProgressView`, `Divider` |
| Controls | `Button`, `Toggle`, `Slider`, `Stepper`, `Picker`, `TextField`, `TextEditor` |
| Layout | `VStack`, `HStack`, `ZStack`, `Spacer`, `Grid`, `ScrollView`, `List`, `SplitView` |
| Grouping | `Section` (plain, titled or card), `row`, `row_detail`, `sidebar_item`, `toolbar` |
| Drawing | `Canvas`, for charts, documents and games |

Stacks place their children one after another. A `Spacer` takes the free space, so `HStack::new().push(title).push(Spacer::new()).push(button)` puts the button at the trailing edge. `List` only builds the rows that are on screen, so it suits thousands of items.

## Modifiers
A modifier returns a changed view, so modifiers chain:

```rust
Text::new("Storage")
    .style(TextStyle::Headline)
    .foreground(Color::SecondaryLabel)
    .padding(12)
    .width(240)
```

The common ones are:
- **Layout:** `padding`, `frame`, `fill`, `grow` and `align`.
- **Appearance:** `background`, `material`, `border`, `shadow`, `foreground`, `tint`, `corner_radius` and `opacity`.
- **Behaviour:** `on_tap`, `on_double_tap`, `on_hover`, `context_menu`, `help` (a tooltip), `shortcut`, `draggable`, `on_drop` and `disabled`.
- **Identity:** `id` gives a view a stable identity, so its focus and scroll position survive when the views around it change.

## The design system
Use the semantic styles rather than raw numbers. They follow the reader's text size, appearance and accent colour, and they match the rest of WaveOS.

- **Text styles:** `LargeTitle`, `Title`, `Title2`, `Headline`, `Body`, `Callout`, `Subheadline`, `Footnote`, `Caption` and `Monospaced`.
- **Colours:** `Label`, `SecondaryLabel`, `TertiaryLabel`, `Accent`, `Background`, `SecondaryBackground`, `Red`, `Green` and the other system colours.
- **Spacing:** `space::S` (8), `space::M` (12), `space::L` (16) and `space::XL` (24).
- **Corner radii:** `radius::CONTROL`, `radius::CARD` and `radius::SHEET`.

## Context: talking to the system
`update` receives a `Context`. Use it to:
- Send messages: `send` and `after` (a timer).
- Open apps and documents: `open_app` and `open_file`.
- Use the clipboard: `copy` and `paste`.
- Post a notification: `notify`.
- Close the window: `close`.
- Control focus and scrolling: `focus`, `scroll_to`, and reading the text of a field.

For background work (a pipe, the network or the clock), return an interval from `poll_interval` and do the work in `poll`. Return true from `poll` only when something changed.

## Sheets, alerts and menus
Return a view from `sheet` to show a dialog over the window. The window ignores input until the sheet goes away. `Alert` builds the standard question with roles for the default, cancel and destructive buttons. `Button::menu` makes a pull-down menu, and `context_menu` adds a secondary-click menu to any view.

## Keyboard and accessibility
WaveUI moves focus with Tab and Shift+Tab, activates focused controls with Space and Return, and scrolls focused views into sight. Give buttons a `shortcut` when an action deserves one. Add `accessibility_label` to images and to symbol-only buttons. WaveUI honours Reduce Motion for its animations, and your app can read `env.reduce_motion` for its own.

## Next steps
- Read the [Design guidelines](developer-design.md) before you lay out a window.
- See [WaveSDK](developer-wavesdk.md) for projects, Studio and the `wavesdk` command.
- See [GINA package format](developer-packages.md) to ship your app.
