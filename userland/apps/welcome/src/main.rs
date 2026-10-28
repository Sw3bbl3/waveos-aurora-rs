//! Welcome — the first sign-in: pick a wallpaper and an appearance, and a
//! few things to try.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::rc::Rc;
use core::cell::RefCell;
use waveui::lumen::icons::Icon;
use waveui::lumen::{theme, wallpaper};
use waveui::prelude::*;
use waveui::Request;

/// Where the wallpapers' small pictures are drawn from: decoded once per
/// wallpaper and appearance.
type Thumbs = RefCell<[[Option<Rc<Pixmap>>; 2]; 3]>;

pub struct Welcome {
    thumbs: Thumbs,
}

#[derive(Clone)]
pub enum Msg {
    Wallpaper(u8),
    Appearance(usize),
    Learn,
    Later,
    Start,
}

/// The things to try: a symbol, a title and a line.
const TIPS: [(Symbol, &str, &str); 3] = [
    (Symbol::Clock, "Meet Crest", "It shows the time and rises when something happens."),
    (Symbol::Grid, "Pin your apps", "Drag from Shoal into the first bay of Harbor."),
    (Symbol::Document, "Learn in short dips", "Six lessons, a few minutes each."),
];

/// The colour of each wallpaper's chip.
const DOTS: [Color; 3] = [Color::Blue, Color::Teal, Color::Orange];

impl Welcome {
    fn thumb(&self, i: u8, dark: bool) -> Option<Rc<Pixmap>> {
        let mut thumbs = self.thumbs.borrow_mut();
        let slot = &mut thumbs[i as usize][dark as usize];
        if slot.is_none() {
            let path = wallpaper::file(i, dark)?.replace("/System/Wallpapers/", "/System/Wallpapers/Thumbnails/");
            let img = corekit::fs::read(&path).ok().and_then(|d| aurora_image::decode(&d).ok())?;
            *slot = Some(Rc::new(Pixmap { width: img.width, height: img.height, pixels: img.pixels }));
        }
        slot.clone()
    }

    fn wallpaper_card(&self, i: u8, dark: bool) -> View<'_, Msg> {
        let on = theme::wallpaper() == i;
        let thumb = self.thumb(i, dark);
        let picture = Canvas::new(move |cv, r| {
            match &thumb {
                // Cover the card, cropping what overflows.
                Some(p) => {
                    let (sw, sh) = (p.width as i32, p.height as i32);
                    let k = (r.w * 1000 / sw.max(1)).max(r.h * 1000 / sh.max(1));
                    let (w, h) = (sw * k / 1000, sh * k / 1000);
                    let dst = waveui::lumen::geom::Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, w, h);
                    cv.draw_image_rounded(&p.pixels, p.width, p.height, dst, r, 14);
                }
                None => {
                    let px = wallpaper::generate(i, r.w, r.h);
                    cv.draw_image_rounded(&px, r.w as u32, r.h as u32, r, r, 14);
                }
            }
            if on {
                cv.stroke_round_rect(r, 14, theme::accent());
                cv.stroke_round_rect(r.inset(1), 13, theme::accent());
            }
        })
        .height(128)
        .fill_width();
        let chip = HStack::new()
            .push(
                Canvas::new(move |cv, r| {
                    let (x, y) = r.center();
                    cv.fill_circle(x, y, 4, DOTS[i as usize].argb());
                })
                .size(10, 10),
            )
            .push(
                Text::new(wallpaper::NAMES[i as usize])
                    .style(TextStyle::Sized(12, waveui::style::Weight::Bold))
                    .foreground(Color::Custom(0xFFFF_FFFF)),
            )
            .spacing(6)
            .padding_xy(9, 4)
            .background_rounded(Color::Custom(0xB00B_0E18), 11);
        ZStack::with(vec![
            picture,
            VStack::new()
                .push(Spacer::new())
                .push(HStack::new().push(chip).push(Spacer::new()).fill_width())
                .padding(12)
                .fill(),
        ])
        .height(128)
        .on_tap(Msg::Wallpaper(i))
        .grow(1)
    }

    fn tip<'a>(symbol: Symbol, title: &str, body: &str) -> View<'a, Msg> {
        HStack::new()
            .push(
                Image::symbol(symbol)
                    .symbol_size(18)
                    .foreground(Color::AccentText)
                    .size(40, 40)
                    .background_rounded(Color::Custom(waveui::lumen::canvas::with_alpha(theme::accent(), 0x2E)), 12),
            )
            .push(
                VStack::new()
                    .push(Text::new(title).style(TextStyle::Sized(14, waveui::style::Weight::Bold)))
                    .push(
                        Text::paragraph(body)
                            .style(TextStyle::Sized(12, waveui::style::Weight::Regular))
                            .foreground(Color::SecondaryLabel),
                    )
                    .spacing(2)
                    .alignment(Align::Start)
                    .grow(1),
            )
            .spacing(14)
            .padding(16)
            .background_rounded(Color::Fill, 14)
            .border(Color::Separator, 14)
            .grow(1)
    }
}

impl App for Welcome {
    type Message = Msg;

    fn title(&self) -> String {
        "Welcome to WaveOS Aurora".into()
    }
    fn size(&self) -> (i32, i32) {
        (980, 660)
    }
    fn resizable(&self) -> bool {
        false
    }

    fn update(&mut self, msg: Msg, cx: &mut Context<Msg>) {
        match msg {
            Msg::Wallpaper(i) => cx.request(Request::SetWallpaper(i)),
            Msg::Appearance(i) => cx.request(Request::SetDark(i == 1)),
            Msg::Learn => cx.open_app("Learn"),
            Msg::Later => {
                corekit::prefs::set_bool("welcome_again", true);
                cx.close();
            }
            Msg::Start => {
                corekit::prefs::set_bool("welcome_again", false);
                cx.close();
            }
        }
    }

    fn view(&self, env: &Env) -> View<'_, Msg> {
        let heading = |s: &'static str| Text::new(s).style(TextStyle::Sized(15, waveui::style::Weight::Bold));
        let walls = HStack::with((0..3u8).map(|i| self.wallpaper_card(i, env.dark)).collect()).spacing(20).fill_width();
        let tips = HStack::with(
            TIPS.iter()
                .enumerate()
                .map(|(i, &(s, t, b))| {
                    let card = Self::tip(s, t, b);
                    if i == 2 {
                        card.on_tap(Msg::Learn)
                    } else {
                        card
                    }
                })
                .collect(),
        )
        .spacing(14)
        .alignment(Align::Stretch)
        .fill_width();
        VStack::new()
            .push(Image::app_icon(Icon::Aurora, 56))
            .push(
                VStack::new()
                    .push(
                        Text::new("Welcome to WaveOS Aurora")
                            .style(TextStyle::Sized(34, waveui::style::Weight::Display)),
                    )
                    .push(
                        Text::new("Three quick choices and you're in the water.")
                            .style(TextStyle::Sized(15, waveui::style::Weight::Regular)),
                    )
                    .spacing(8)
                    .padding_insets(Insets { top: 6, leading: 0, bottom: 18, trailing: 0 }),
            )
            .push(
                HStack::new()
                    .push(heading("1 · Pick a wallpaper. The colours follow it.").grow(1))
                    .push(Picker::new(["Light", "Dark"], env.dark as usize).on_change(Msg::Appearance).width(170))
                    .fill_width(),
            )
            .push(walls)
            .push(heading("2 · Then try these").padding_insets(Insets { top: 10, leading: 0, bottom: 0, trailing: 0 }))
            .push(tips)
            .push(Spacer::new())
            .push(
                HStack::new()
                    .push(Spacer::new())
                    .push(Button::new("Show this again later").action(Msg::Later).button_style(ButtonStyle::Plain))
                    .push(
                        Button::new("Start")
                            .action(Msg::Start)
                            .button_style(ButtonStyle::Prominent)
                            .control_size(ControlSize::Large)
                            .shortcut(Shortcut::default_action())
                            .min_width(90),
                    )
                    .spacing(22)
                    .fill_width(),
            )
            .spacing(14)
            .alignment(Align::Stretch)
            .padding_insets(Insets { top: 30, leading: 30, bottom: 28, trailing: 30 })
            .fill()
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    waveui::run(Welcome { thumbs: RefCell::new(Default::default()) })
}
