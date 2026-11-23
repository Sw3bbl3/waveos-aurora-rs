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
type Thumbs = RefCell<[[Option<Rc<Pixmap>>; 2]; 6]>;

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
    Tour,
}

const DOTS: [Color; 6] = [Color::Blue, Color::Teal, Color::Orange, Color::Blue, Color::Teal, Color::Gray];

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
        .height(104)
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
        .height(104)
        .on_tap(Msg::Wallpaper(i))
        .grow(1)
    }
}

impl App for Welcome {
    type Message = Msg;

    fn title(&self) -> String {
        "Welcome to WaveOS Aurora".into()
    }
    fn size(&self) -> (i32, i32) {
        (900, 620)
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
            Msg::Tour => {
                corekit::prefs::set_bool("welcome_again", false);
                cx.request(Request::StartTour);
                cx.close();
            }
            Msg::Start => {
                corekit::prefs::set_bool("welcome_again", false);
                cx.close();
            }
        }
    }

    fn view(&self, env: &Env) -> View<'_, Msg> {
        let mut walls = Grid::new(220).spacing(12);
        for i in 0..wallpaper::NAMES.len() as u8 {
            walls = walls.push(self.wallpaper_card(i, env.dark));
        }
        let content = VStack::new()
            .push(HStack::new()
                .push(Image::app_icon(Icon::Aurora, 42))
                .push(VStack::new()
                    .push(Text::new("Welcome to WAVEOS").style(TextStyle::LargeTitle))
                    .push(Text::new("A little space to make your own.").foreground(Color::SecondaryLabel))
                    .alignment(Align::Start).spacing(4).grow(1))
                .spacing(16).fill_width())
            .push(HStack::new()
                .push(Text::new("Choose your view").style(TextStyle::Headline).grow(1))
                .push(Picker::new(["Light", "Dark"], env.dark as usize).on_change(Msg::Appearance).width(160))
                .fill_width())
            .push(walls.fill_width())
            .push(HStack::new()
                .push(Image::symbol(Symbol::Mouse).symbol_size(24).foreground(Color::AccentText).size(40,40))
                .push(VStack::new()
                    .push(Text::new("Get comfortable, one step at a time").style(TextStyle::Headline))
                    .push(Text::paragraph("Try animated tips for Apps, Search, window controls, and layouts. Skip anytime; come back from Help.")
                        .style(TextStyle::Callout).foreground(Color::SecondaryLabel))
                    .spacing(4).alignment(Align::Start).grow(1))
                .spacing(14).padding(16).min_height(96).background_rounded(Color::Fill,12).fill_width())
            .spacing(20).alignment(Align::Stretch).padding(24).fill_width();
        VStack::new()
            .push(ScrollView::new(content).fill())
            .push(Divider::new())
            .push(
                HStack::new()
                    .push(Button::new("Help").action(Msg::Learn).button_style(ButtonStyle::Plain))
                    .push(Button::new("Remind me later").action(Msg::Later).button_style(ButtonStyle::Plain))
                    .push(Spacer::new())
                    .push(Button::new("Explore on my own").action(Msg::Start))
                    .push(
                        Button::new("Take a tour")
                            .action(Msg::Tour)
                            .button_style(ButtonStyle::Prominent)
                            .shortcut(Shortcut::default_action()),
                    )
                    .spacing(12)
                    .padding_xy(24, 16)
                    .fill_width(),
            )
            .spacing(0)
            .fill()
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    waveui::run(Welcome { thumbs: RefCell::new(Default::default()) })
}
