//! One source of truth for the downloadable handbook and offline Learn library.
use aurora_help::{normalize, Article};
use pulldown_cmark::{html, Event, Options, Parser, Tag, TagEnd};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
fn slug(s: &str) -> String {
    normalize(s)
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
fn parsed(source: &str) -> Result<(Vec<Event<'_>>, Vec<(String, String)>, String), String> {
    let mut events: Vec<_> = Parser::new_ext(source, Options::ENABLE_TABLES).collect();
    let mut headings = Vec::new();
    let mut plain = String::new();
    let mut used = BTreeSet::new();
    for i in 0..events.len() {
        if matches!(events[i], Event::Html(_) | Event::InlineHtml(_)) {
            return Err("Raw HTML is not supported in handbook sources".into());
        }
        if matches!(events[i], Event::Start(Tag::Heading { .. })) {
            let title = events[i + 1..]
                .iter()
                .take_while(|e| !matches!(e, Event::End(TagEnd::Heading(_))))
                .filter_map(|e| match e {
                    Event::Text(t) | Event::Code(t) => Some(t.as_ref()),
                    _ => None,
                })
                .collect::<String>();
            let base = slug(&title);
            let mut id = base.clone();
            let mut n = 1;
            while !used.insert(id.clone()) {
                n += 1;
                id = format!("{base}-{n}");
            }
            if let Event::Start(Tag::Heading { id: ref mut anchor, .. }) = events[i] {
                *anchor = Some(id.clone().into());
            }
            headings.push((id, title));
        }
        match &events[i] {
            Event::Text(t) | Event::Code(t) => {
                plain.push_str(t);
                plain.push(' ');
            }
            Event::SoftBreak | Event::HardBreak => plain.push('\n'),
            _ => {}
        }
    }
    if headings.is_empty() {
        return Err("Article has no heading".into());
    }
    Ok((events, headings, plain))
}
pub fn build(root: &Path) -> Result<PathBuf, String> {
    let base = root.join("docs/handbook/0.7");
    let out = root.join("target/help");
    fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut sources = BTreeMap::new();
    let mut ids = BTreeMap::new();
    for locale in ["en", "fr-CA"] {
        let mut names = BTreeSet::new();
        for e in fs::read_dir(base.join(locale)).map_err(|e| e.to_string())? {
            let p = e.map_err(|e| e.to_string())?.path();
            if p.extension().is_none_or(|s| s != "md") {
                continue;
            }
            let id = p.file_stem().unwrap().to_str().unwrap().to_string();
            names.insert(id.clone());
            sources.insert(p.canonicalize().unwrap(), (locale.to_string(), id));
        }
        ids.insert(locale, names);
        for entry in fs::read_dir(root.join("docs/releases")).map_err(|e| e.to_string())? {
            let release = entry.map_err(|e| e.to_string())?.path();
            if !release.is_dir() {
                continue;
            }
            let version = release.file_name().unwrap().to_str().ok_or("Invalid release path")?;
            sources.insert(
                release
                    .join(format!("{locale}.md"))
                    .canonicalize()
                    .map_err(|_| format!("Missing {locale} translation for release {version}"))?,
                (locale.into(), format!("release-{version}")),
            );
        }
    }
    if ids["en"] != ids["fr-CA"] {
        return Err("English and Canadian French article IDs differ".into());
    }
    let mut anchors = BTreeMap::new();
    for path in sources.keys() {
        let src = fs::read_to_string(path).map_err(|e| e.to_string())?;
        let (_, h, _) = parsed(&src)?;
        anchors.insert(path.clone(), h);
    }
    let mut catalog = Vec::new();
    for (path, (locale, id)) in &sources {
        let source = fs::read_to_string(path).map_err(|e| e.to_string())?;
        let (mut events, headings, text) = parsed(&source)?;
        for e in &mut events {
            let (url, image) = match e {
                Event::Start(Tag::Link { dest_url, .. }) => (dest_url, false),
                Event::Start(Tag::Image { dest_url, .. }) => (dest_url, true),
                _ => continue,
            };
            let raw = url.to_string();
            if raw.starts_with("https://") || raw.starts_with("http://") {
                if image {
                    return Err("Images must be offline".into());
                }
                continue;
            }
            let (file, anchor) = raw.split_once('#').unwrap_or((&raw, ""));
            let resolved = if file.is_empty() {
                path.clone()
            } else {
                path.parent()
                    .unwrap()
                    .join(file)
                    .canonicalize()
                    .map_err(|_| format!("Broken link in {}: {raw}", path.display()))?
            };
            if image {
                if !resolved.starts_with(root.join("docs").canonicalize().unwrap()) {
                    return Err("Image outside docs".into());
                }
                let name = resolved.file_name().unwrap().to_str().unwrap();
                let dest = out.join("images");
                fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
                let bytes = fs::read(&resolved).map_err(|e| e.to_string())?;
                if dest.join(name).exists() && fs::read(dest.join(name)).unwrap() != bytes {
                    return Err(format!("Duplicate image name {name}"));
                }
                fs::write(dest.join(name), bytes).map_err(|e| e.to_string())?;
                *url = format!("../images/{name}").into();
            } else {
                let (lang, target) = sources
                    .get(&resolved)
                    .ok_or_else(|| format!("Unbundled link {} in {}", resolved.display(), path.display()))?;
                if !anchor.is_empty() && !anchors[&resolved].iter().any(|(a, _)| a == anchor) {
                    return Err(format!("Missing anchor {raw}"));
                }
                *url = format!(
                    "../{lang}/{target}.html{}",
                    if anchor.is_empty() { String::new() } else { format!("#{anchor}") }
                )
                .into();
            }
        }
        let mut body = String::new();
        html::push_html(&mut body, events.into_iter());
        let title = headings[0].1.clone();
        let language = if locale == "en" { "English" } else { "Français (Canada)" };
        let html=format!("<!doctype html><html lang=\"{locale}\"><head><meta charset=\"utf-8\"><title>{}</title><style>body{{font-family:sans-serif;font-size:16px;line-height:1.55;max-width:850px;margin:24px auto;padding:0 20px;color:#202331}}h1{{font-size:30px}}h2{{font-size:23px;margin-top:28px}}a{{color:#5550c8}}pre{{background:#edf0f6;padding:16px;white-space:pre-wrap}}code{{font-family:monospace}}img{{max-width:100%;height:auto}}table{{width:100%;border-collapse:collapse}}td,th{{padding:8px;border:1px solid #c8cad4}}nav{{margin-bottom:24px}}</style></head><body><nav><a href=\"../index.html\">WaveOS Learn · {language}</a></nav>{body}</body></html>",escape(&title));
        fs::create_dir_all(out.join(locale)).map_err(|e| e.to_string())?;
        fs::write(out.join(locale).join(format!("{id}.html")), html).map_err(|e| e.to_string())?;
        catalog.push(Article {
            id: id.clone(),
            locale: locale.clone(),
            title,
            category: id.split('-').next().unwrap().into(),
            path: format!("{locale}/{id}.html"),
            text,
            headings,
        });
    }
    catalog.sort_by(|a, b| a.locale.cmp(&b.locale).then(a.id.cmp(&b.id)));
    fs::write(out.join("catalog.json"), serde_json::to_vec(&catalog).unwrap()).map_err(|e| e.to_string())?;
    let mut index=String::from("<!doctype html><html><head><meta charset=\"utf-8\"><title>WaveOS Learn 0.7</title></head><body><h1>WaveOS Learn 0.7</h1>");
    for locale in ["en", "fr-CA"] {
        index.push_str(&format!("<h2>{}</h2><ul>", if locale == "en" { "English" } else { "Français (Canada)" }));
        let mut search = String::new();
        for a in catalog.iter().filter(|a| a.locale == locale) {
            index.push_str(&format!("<li><a href=\"{}\">{}</a></li>", a.path, escape(&a.title)));
            search.push_str(&format!("{}\t{}\t{}\n", a.id, a.title, a.text.replace('\n', " ")));
        }
        index.push_str("</ul>");
        fs::write(out.join(format!("search-{locale}.txt")), search).map_err(|e| e.to_string())?;
    }
    index.push_str("</body></html>");
    fs::write(out.join("index.html"), index).map_err(|e| e.to_string())?;
    Ok(out)
}
pub fn bundle(t: &mut crate::tar::Tar, root: &Path) {
    let help = build(root).unwrap_or_else(|e| panic!("documentation: {e}"));
    fn walk(t: &mut crate::tar::Tar, path: &Path, dest: &str) {
        t.dir(dest);
        let mut paths = fs::read_dir(path).unwrap().map(|p| p.unwrap().path()).collect::<Vec<_>>();
        paths.sort();
        for p in paths {
            let name = format!("{dest}/{}", p.file_name().unwrap().to_str().unwrap());
            if p.is_dir() {
                walk(t, &p, &name)
            } else {
                t.file(&name, &fs::read(p).unwrap());
            }
        }
    }
    walk(t, &help, "Help");
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_html_and_make_unique_anchors() {
        assert!(parsed("# Good\n<script>bad</script>").is_err());
        let (_, h, _) = parsed("# Réseau\n## Réseau\n").unwrap();
        assert_eq!(h[0].0, "reseau");
        assert_eq!(h[1].0, "reseau-2");
    }
    #[test]
    fn complete_handbook_validates() {
        build(&crate::root()).unwrap();
    }
    #[test]
    fn missing_translation_link_anchor_and_image_are_rejected() {
        let root = std::env::temp_dir().join(format!("waveos-help-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for lang in ["en", "fr-CA"] {
            fs::create_dir_all(root.join(format!("docs/handbook/0.7/{lang}"))).unwrap();
            fs::create_dir_all(root.join("docs/releases/0.7.0")).unwrap();
            fs::write(root.join(format!("docs/releases/0.7.0/{lang}.md")), "# Release\n").unwrap();
        }
        let en = root.join("docs/handbook/0.7/en/start.md");
        let fr = root.join("docs/handbook/0.7/fr-CA/start.md");
        fs::write(&en, "# Start\n").unwrap();
        assert!(build(&root).unwrap_err().contains("IDs differ"));
        fs::write(&fr, "# Départ\n").unwrap();
        assert!(build(&root).is_ok());
        for source in [
            "# Start\n[Missing](no.md)",
            "# Start\n[Missing](#absent)",
            "# Start\n![Missing](lost.png)",
            "# Start\n![Remote](https://example.com/a.png)",
        ] {
            fs::write(&en, source).unwrap();
            assert!(build(&root).is_err(), "{source}");
        }
        fs::write(&en, "# Start\n[Here](#start)").unwrap();
        assert!(build(&root).is_ok());
        fs::remove_dir_all(root).unwrap();
    }
}
