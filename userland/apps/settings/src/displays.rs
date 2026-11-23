//! Display, with several displays: their arrangement (drag them, or put
//! one left, right, above or below the main display), which one is the
//! main display (the menu bar and Harbor), mirroring, and each display's
//! resolution, scale and wallpaper.
//!
//! Everything is saved as preferences named after each display's identity
//! (`lumen::displays::Record` under `display_<key>`, plus `display_primary`
//! and `display_mirror`); the compositor applies them at once and finds
//! them again whenever that display is plugged in.

use super::{card, footer, page, radio, Msg, Settings};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use corekit::abi::{display, display_info, DisplayInfo};
use corekit::prefs;
use waveui::lumen::displays::{self as layout, Record, Side};
use waveui::lumen::geom::Rect;
use waveui::lumen::{theme, wallpaper};
use waveui::prelude::*;

/// The arrangement picture's size.
const ARRANGE_W: i32 = 460;
const ARRANGE_H: i32 = 210;

#[derive(Clone, Copy, Debug)]
pub enum DisplayMsg {
    Select(usize),
    Press(i32, i32),
    Drag(i32, i32),
    Release,
    Side(usize),
    Scale(usize),
    Wallpaper(usize),
    MakeMain,
    Mirror(bool),
    Mode(usize),
}

#[derive(Default)]
pub struct DisplaysState {
    /// The display the settings below the arrangement are for.
    selected: usize,
    drag: Option<Drag>,
}

/// A display being dragged in the arrangement picture.
#[derive(Clone, Copy)]
struct Drag {
    index: usize,
    /// Where it was grabbed, in points from its corner.
    grab: (i32, i32),
    /// Where it is now.
    pos: (i32, i32),
}

/// The arrangement picture's transform: picture = offset + point × k / 1000.
fn fit(rects: &[Rect]) -> (i32, i32, i32) {
    let b = layout::bounds(rects);
    let k = ((ARRANGE_W - 40) * 1000 / b.w.max(1)).min((ARRANGE_H - 40) * 1000 / b.h.max(1)).max(1);
    let ox = (ARRANGE_W - b.w * k / 1000) / 2 - b.x * k / 1000;
    let oy = (ARRANGE_H - b.h * k / 1000) / 2 - b.y * k / 1000;
    (k, ox, oy)
}

fn rect_of(d: &DisplayInfo) -> Rect {
    Rect::new(d.x, d.y, d.width, d.height)
}

/// The saved record of a display (or what it shows now).
fn record(d: &DisplayInfo) -> Record {
    prefs::get(&layout::pref_key(d.key)).and_then(|v| Record::parse(&v)).unwrap_or(Record {
        x: d.x,
        y: d.y,
        wallpaper: d.wallpaper as i16,
        ..Record::default()
    })
}

/// Saves every display's place (`places[i]` for display `i`), keeping the
/// rest of their records, so the whole arrangement stays as it is seen.
fn save_places(list: &[DisplayInfo], places: &[(i32, i32)]) -> corekit::Result<()> {
    for (d, &(x, y)) in list.iter().zip(places) {
        let r = Record { x, y, ..record(d) };
        prefs::set(&layout::pref_key(d.key), &r.format())?;
    }
    Ok(())
}

fn short(d: &DisplayInfo) -> String {
    let name = if d.connector().is_empty() { d.name() } else { d.connector() };
    if d.is_primary() {
        format!("{name} (main)")
    } else {
        String::from(name)
    }
}

impl Settings {
    /// Settings → Display with more than one display.
    pub(super) fn displays_pane(&self, list: Vec<DisplayInfo>) -> View<'_, Msg> {
        let st = &self.displays;
        let sel = st.selected.min(list.len() - 1);
        let d = list[sel];
        let mirror = list.iter().any(|d| d.flags & display_info::MIRROR != 0)
            || prefs::get(layout::MIRROR_PREF).as_deref() == Some("1");
        let mut rects: Vec<Rect> = list.iter().map(rect_of).collect();
        let (k, ox, oy) = fit(&rects);
        if let Some(d) = st.drag {
            rects[d.index] = Rect::new(d.pos.0, d.pos.1, rects[d.index].w, rects[d.index].h);
        }
        let names: Vec<String> = list.iter().map(short).collect();
        let labels = names.clone();
        let picture = Canvas::new(move |cv, area| {
            let t = theme::current();
            cv.fill_round_rect(area, 12, t.control_bg);
            for (i, r) in rects.iter().enumerate() {
                let p = Rect::new(
                    area.x + ox + r.x * k / 1000,
                    area.y + oy + r.y * k / 1000,
                    (r.w * k / 1000).max(8),
                    (r.h * k / 1000).max(8),
                );
                let on = i == sel;
                cv.fill_round_rect(p.inset(1), 6, if on { theme::accent() } else { t.control_border });
                if i == 0 && !mirror {
                    // The menu bar.
                    cv.fill_rect(Rect::new(p.x + 3, p.y + 3, p.w - 6, 5), 0xC0FF_FFFF);
                }
                let f = theme::ui(12);
                let label = &labels[i];
                let w = f.width(label).min(p.w - 8);
                cv.text_clipped(p.x + (p.w - w) / 2, p.y + p.h / 2 + 5, label, f, 0xFFFF_FFFF, p.w - 8);
            }
        })
        .size(ARRANGE_W, ARRANGE_H)
        .on_input(|e| match e {
            CanvasEvent::Press { x, y, .. } => Some(Msg::Displays(DisplayMsg::Press(x, y))),
            CanvasEvent::Drag { x, y } => Some(Msg::Displays(DisplayMsg::Drag(x, y))),
            CanvasEvent::Release { .. } => Some(Msg::Displays(DisplayMsg::Release)),
            _ => None,
        })
        .id("display/arrangement");

        let mut sections = alloc::vec![footer(
            "Drag a display to arrange it. The menu bar and Harbor are on the main display; the pointer and \
             windows move between displays where they touch."
        )];
        sections.push(Section::titled("Arrangement", picture));
        sections.push(
            card(alloc::vec![row(
                "Mirror displays",
                Toggle::new("", mirror).on_change(|on| Msg::Displays(DisplayMsg::Mirror(on))),
            )])
            .id("display/mirror"),
        );
        let others = Picker::new(names, sel)
            .on_change(|i| Msg::Displays(DisplayMsg::Select(i)))
            .picker_style(PickerStyle::Segmented);
        sections.push(Section::titled("Display", card(alloc::vec![row("Settings for", others)])));
        let rec = record(&d);
        let mut rows = alloc::vec![row_detail(
            d.name(),
            format!(
                "{} × {} pixels at {}×{}",
                d.pixel_width,
                d.pixel_height,
                d.scale,
                if d.mm_width > 0 { format!(", {} × {} mm", d.mm_width, d.mm_height) } else { String::new() }
            ),
            Text::new(""),
        )];
        if !d.is_primary() {
            rows.push(row("Main display", Button::new("Use as Main Display").action(Msg::Displays(DisplayMsg::MakeMain))));
            let side = list
                .iter()
                .find(|m| m.is_primary())
                .and_then(|m| layout::side_of(rect_of(m), rect_of(&d)))
                .map_or(usize::MAX, |s| Side::ALL.iter().position(|&a| a == s).unwrap_or(usize::MAX));
            rows.push(row(
                "Next to the main display",
                Picker::new(Side::ALL.map(Side::name), side)
                    .on_change(|i| Msg::Displays(DisplayMsg::Side(i)))
                    .picker_style(PickerStyle::Segmented),
            ));
        }
        rows.push(row(
            "Draw text and controls at",
            Picker::new(["Automatic", "1×", "2×"], rec.scale.min(2) as usize)
                .on_change(|i| Msg::Displays(DisplayMsg::Scale(i)))
                .picker_style(PickerStyle::Segmented),
        ));
        let mut walls: Vec<String> = alloc::vec![String::from("Same as main")];
        walls.extend(wallpaper::NAMES.iter().map(|n| String::from(*n)));
        let wall = if rec.wallpaper < 0 { 0 } else { (rec.wallpaper as usize + 1).min(walls.len() - 1) };
        rows.push(row(
            "Wallpaper",
            Picker::new(walls, wall).on_change(|i| Msg::Displays(DisplayMsg::Wallpaper(i))).picker_style(PickerStyle::Menu),
        ));
        sections.push(card(rows).id("display/each"));
        // Resolutions, largest first.
        let modes = self.modes.iter().enumerate().rev().map(|(i, m)| {
            let current = m.flags & display::CURRENT != 0;
            let mut tags = Vec::new();
            if m.flags & display::PREFERRED != 0 {
                tags.push("Recommended");
            }
            if current {
                tags.push("In use");
            }
            HStack::new()
                .push(Text::new(format!("{} × {}", m.width, m.height)).grow(1))
                .push(Text::new(tags.join(" · ")).style(TextStyle::Callout).foreground(Color::SecondaryLabel))
                .push(radio(current))
                .spacing(12)
                .min_height(38)
                .fill_width()
                .on_tap(Msg::Displays(DisplayMsg::Mode(i)))
        });
        sections.push(Section::titled("Resolution", card(modes.collect()).id("display/resolution")));
        page("Display", sections)
    }

    /// The modes listed: the main display's through `display_modes` (which
    /// knows about the firmware), another's from the compositor.
    pub(super) fn display_modes_now(&self) -> Vec<corekit::abi::DisplayMode> {
        let list = corekit::display::displays();
        match list.get(self.displays.selected.min(list.len().saturating_sub(1))) {
            Some(d) if !d.is_primary() => corekit::display::modes(d.id),
            _ => prefs::display_modes(),
        }
    }

    pub(super) fn displays_update(&mut self, msg: DisplayMsg, cx: &mut Context<Msg>) {
        let list = corekit::display::displays();
        if list.is_empty() {
            return;
        }
        let sel = self.displays.selected.min(list.len() - 1);
        let rects: Vec<Rect> = list.iter().map(rect_of).collect();
        let mut places: Vec<(i32, i32)> = rects.iter().map(|r| (r.x, r.y)).collect();
        let result = match msg {
            DisplayMsg::Select(i) => {
                self.displays.selected = i.min(list.len() - 1);
                self.modes = self.display_modes_now();
                Ok(())
            }
            DisplayMsg::Press(x, y) => {
                let (k, ox, oy) = fit(&rects);
                let p = ((x - ox) * 1000 / k, (y - oy) * 1000 / k);
                if let Some(i) = layout::at(&rects, p.0, p.1) {
                    self.displays.selected = i;
                    self.modes = self.display_modes_now();
                    // The main display stays where it is; the others move around it.
                    if !list[i].is_primary() {
                        let grab = (p.0 - rects[i].x, p.1 - rects[i].y);
                        self.displays.drag = Some(Drag { index: i, grab, pos: (rects[i].x, rects[i].y) });
                    }
                }
                Ok(())
            }
            DisplayMsg::Drag(x, y) => {
                let (k, ox, oy) = fit(&rects);
                if let Some(d) = self.displays.drag.as_mut() {
                    let p = ((x - ox) * 1000 / k, (y - oy) * 1000 / k);
                    d.pos = (p.0 - d.grab.0, p.1 - d.grab.1);
                }
                Ok(())
            }
            DisplayMsg::Release => match self.displays.drag.take() {
                Some(Drag { index: i, pos, .. }) => {
                    let others: Vec<Rect> = (0..rects.len()).filter(|&j| j != i).map(|j| rects[j]).collect();
                    places[i] = layout::snap(&others, (rects[i].w, rects[i].h), pos);
                    save_places(&list, &places)
                }
                None => Ok(()),
            },
            DisplayMsg::Side(s) => {
                let Some(main) = list.iter().position(|d| d.is_primary()) else { return };
                let side = Side::ALL[s.min(3)];
                let want = layout::beside(rects[main], (rects[sel].w, rects[sel].h), side);
                let others: Vec<Rect> = (0..rects.len()).filter(|&j| j != sel).map(|j| rects[j]).collect();
                places[sel] = layout::snap(&others, (rects[sel].w, rects[sel].h), want);
                save_places(&list, &places)
            }
            DisplayMsg::Scale(i) => {
                let r = Record { scale: i.min(2) as u8, ..record(&list[sel]) };
                prefs::set(&layout::pref_key(list[sel].key), &r.format())
            }
            DisplayMsg::Wallpaper(i) => {
                let wallpaper = if i == 0 { -1 } else { (i - 1) as i16 };
                let r = Record { wallpaper, ..record(&list[sel]) };
                prefs::set(&layout::pref_key(list[sel].key), &r.format())
            }
            DisplayMsg::MakeMain => {
                // The arrangement as it is now goes with it.
                save_places(&list, &places).and_then(|_| prefs::set(layout::PRIMARY_PREF, &format!("{:08x}", list[sel].key)))
            }
            DisplayMsg::Mirror(on) => prefs::set(layout::MIRROR_PREF, if on { "1" } else { "0" }),
            DisplayMsg::Mode(i) => {
                let Some(m) = self.modes.get(i).copied() else { return };
                if list[sel].is_primary() {
                    // As with one display: switched now, then "Keep this resolution?".
                    match prefs::set_display(m.width, m.height) {
                        Ok(true) => Ok(()),
                        Ok(false) => {
                            self.flash(cx, format!("{} × {} will be used when you restart", m.width, m.height));
                            Ok(())
                        }
                        Err(e) => Err(e),
                    }
                } else {
                    let r = Record { width: m.width, height: m.height, ..record(&list[sel]) };
                    prefs::set(&layout::pref_key(list[sel].key), &r.format())
                }
            }
        };
        if let Err(e) = result {
            self.flash(cx, format!("Couldn't change the display: {e}"));
        }
        if !matches!(msg, DisplayMsg::Drag(..) | DisplayMsg::Press(..)) {
            self.modes = self.display_modes_now();
        }
    }
}
