# Design guidelines
Great WaveOS apps feel calm, clear and quick. These guidelines describe how built-in apps look and behave, so that your app feels at home next to them. WaveUI does most of this for you when you use its standard views and styles.

## Principles
- **Clarity.** Content comes first. Use plenty of space, a clear type hierarchy and only a few colours, so the important things stand out.
- **Depth.** Sidebars and toolbars use translucent materials, like living water over the desktop. Cards and sheets lift the content they hold. Use depth to show structure, not as decoration.
- **Deference.** The interface supports the work and stays out of its way. Prefer familiar controls to custom ones, and keep chrome quiet.
- **Respect.** Follow the person's settings: appearance, accent colour, text size, Reduce Motion and Reduce Transparency. Never lose their work.

## Layout
- Put navigation in a sidebar on the leading edge with `SplitView`. Show the selected item's content on the trailing side.
- Put actions for the current view in a toolbar at the top. Put the most frequent action at the trailing end.
- Group related settings into cards with `Section::card`. Give each group a short title, and add a footnote when a setting needs explaining.
- Use the spacing scale: 8 between related items, 16 between groups and 24 around the content of a window.
- Align text to a common leading edge. Avoid centring long text.
- Make your window work at its smallest size. Let lists and editors take the extra space when it grows.

## Typography
WaveOS uses Manrope. Use the text styles rather than fixed sizes:

| Style | Use it for |
|---|---|
| Large Title | The title of a page or a big number |
| Title | The title of a window's content |
| Title 2 | Section titles |
| Headline | Row titles and emphasized text |
| Body | Everything else |
| Callout and Subheadline | Secondary explanations and row details |
| Footnote and Caption | Small print and labels on small controls |

Write in sentence case ("Show battery level", not "Show Battery Level"). Keep labels short, and make them say what will happen.

## Colour
- Use the semantic colours. `Label` for primary text, `SecondaryLabel` for supporting text, and `Background` and `SecondaryBackground` for surfaces. They adapt to light and dark appearance automatically.
- Use the accent colour for selection, links and the one primary action in a view. Don't paint whole areas in it.
- Use `Red` only for destructive actions and errors.
- Never rely on colour alone. Pair status colours with a symbol or text.
- Check contrast in both appearances, and with Increase Contrast turned on.

## Controls
- **Buttons:** use the prominent style for the single most likely action, bordered for the others and destructive for actions that delete. In toolbars, use symbols with a tooltip from `help`.
- **Toggles** turn a setting on or off at once. Use them instead of a checkbox in settings.
- **Pickers:** segmented for two to five short choices, a menu for longer lists.
- **Text fields** need a placeholder that describes the expected content, not a label repeated.
- **Sliders** suit continuous values such as volume or brightness. Show the value when it matters.
- **Progress:** show a bar when you know how much is left and a spinner when you don't. If work takes more than a few seconds, let people cancel it.

## Alerts and sheets
- Use an alert only when people must decide something now, for example before deleting files or discarding unsaved changes. Don't use one for routine success.
- Title the alert with the question ("Delete 3 items?"). Name the buttons with verbs ("Delete", "Cancel"), not "Yes" and "No".
- Mark destructive buttons as destructive, and make Cancel the default when the action can't be undone.
- Use a sheet for a short task tied to the window, such as a rename or a new connection. Keep the task's buttons at the bottom trailing corner.

## Symbols and icons
- Use system symbols with `Image::symbol` in toolbars, sidebars and rows. They match the weight of the text and follow the accent and appearance.
- Give your app icon a simple shape on a rounded square, like the icons in Harbor. It should read at 16 pixels.
- Give every symbol-only button an accessibility label.

## Motion
- Keep animations short (under 250 milliseconds) and purposeful. Show where something came from or went.
- Never animate when Reduce Motion is on. WaveUI does this for its own animations; check `env.reduce_motion` for yours.
- Don't move content the person is reading or typing into.

## Keyboard
- Everything must work without a pointer. Tab and Shift+Tab move between controls, Space and Return activate them, Escape closes menus, and it should cancel your sheets too.
- Support the standard shortcuts: Ctrl+N, Ctrl+O, Ctrl+S, Ctrl+W, Ctrl+Z, Ctrl+C, Ctrl+V and Ctrl+F.
- Show shortcuts in menus, so people can learn them.

## Writing
- Address people as "you". Be direct, friendly and brief.
- Describe what happened and what to do next in error messages: "The file could not be saved because the disk is full. Delete some files and try again."
- Don't blame people, and don't use jargon or error codes in the main message.
- Ship your app in every language you can. WaveOS comes in English and Canadian French.

See [WaveUI](developer-waveui.md) for the views behind these guidelines.
