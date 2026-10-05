# WaveOS handbook / Guide WaveOS

The 0.7 handbook is available in [English](0.7/en/start-welcome.md) and [français canadien](0.7/fr-CA/start-welcome.md). Both versions are bundled offline in Learn.

Run `cargo xtask docs` to validate translations, links, anchors and images, and generate `target/help`. Its `index.html` is the downloadable offline entry point; `catalog.json` and the text indexes power Learn.

Markdown sources are authoritative. Add articles with the same filename in both language directories. Use the prefixes `start`, `desktop`, `apps`, `system`, `developer`, or `reference` to select a category. Supported markup: headings, paragraphs, emphasis, lists, fenced code, tables, local images and links. Raw HTML, strikethrough, footnotes, and task lists are rejected. Images belong under docs and use unique filenames. External images are rejected.
