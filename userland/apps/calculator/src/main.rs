//! Calculator.

#![no_std]
#![no_main]

extern crate alloc;

use waveui::lumen::canvas::rgb;
use waveui::prelude::*;
use waveui::style::Weight;

const KEYS: [[&str; 4]; 5] =
    [["C", "±", "%", "÷"], ["7", "8", "9", "×"], ["4", "5", "6", "−"], ["1", "2", "3", "+"], ["0", "", ".", "="]];

pub struct Calculator {
    display: String,
    acc: Option<f64>,
    op: Option<char>,
    fresh: bool,
}

#[derive(Clone)]
pub enum Msg {
    Key(&'static str),
    Digit(char),
    Backspace,
}

impl Calculator {
    fn new() -> Self {
        Self { display: "0".into(), acc: None, op: None, fresh: true }
    }

    fn value(&self) -> f64 {
        self.display.parse().unwrap_or(0.0)
    }

    fn show(&mut self, v: f64) {
        self.display = if !v.is_finite() {
            "Error".into()
        } else if v == (v as i64) as f64 && v.abs() < 1e15 {
            format!("{}", v as i64)
        } else {
            let s = format!("{:.10}", v);
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        };
    }

    fn apply(&mut self) {
        if let (Some(a), Some(op)) = (self.acc, self.op) {
            let b = self.value();
            let r = match op {
                '+' => a + b,
                '-' => a - b,
                '*' => a * b,
                '/' => {
                    if b == 0.0 {
                        f64::NAN
                    } else {
                        a / b
                    }
                }
                _ => b,
            };
            self.show(r);
        }
        self.acc = None;
        self.op = None;
    }

    fn press(&mut self, key: &str) {
        match key {
            "C" => *self = Self::new(),
            "±" => {
                let v = -self.value();
                self.show(v);
            }
            "%" => {
                let v = self.value() / 100.0;
                self.show(v);
            }
            "." => {
                if self.fresh {
                    self.display = "0".into();
                    self.fresh = false;
                }
                if !self.display.contains('.') {
                    self.display.push('.');
                }
            }
            "=" => {
                self.apply();
                self.fresh = true;
            }
            "+" | "−" | "×" | "÷" => {
                if self.op.is_some() && !self.fresh {
                    self.apply();
                }
                self.acc = Some(self.value());
                self.op = Some(match key {
                    "+" => '+',
                    "−" => '-',
                    "×" => '*',
                    _ => '/',
                });
                self.fresh = true;
            }
            d => {
                let d = d.to_string();
                if self.fresh || self.display == "0" || self.display == "Error" {
                    self.display.clear();
                    self.fresh = false;
                }
                if self.display.len() < 16 {
                    self.display.push_str(&d);
                }
            }
        }
    }

    /// The pending operation ("12 ×"), shown above the display.
    fn hint(&self) -> Option<String> {
        let (a, op) = (self.acc?, self.op?);
        let mut tmp = Calculator::new();
        tmp.show(a);
        let sym = match op {
            '+' => "+",
            '-' => "−",
            '*' => "×",
            _ => "÷",
        };
        Some(format!("{} {}", tmp.display, sym))
    }
}

impl App for Calculator {
    type Message = Msg;

    fn title(&self) -> String {
        "Calculator".into()
    }
    fn size(&self) -> (i32, i32) {
        (300, 440)
    }
    fn resizable(&self) -> bool {
        false
    }

    fn update(&mut self, msg: Msg, _cx: &mut Context<Msg>) {
        match msg {
            Msg::Key(k) => self.press(k),
            Msg::Digit(c) => self.press(c.encode_utf8(&mut [0; 4])),
            Msg::Backspace => {
                if !self.fresh && self.display.len() > 1 {
                    self.display.pop();
                } else {
                    self.display = "0".into();
                    self.fresh = true;
                }
            }
        }
    }

    fn key(&self, ev: &KeyEvent) -> Option<Msg> {
        Some(match (ev.code, ev.ch) {
            (KeyCode::Enter, _) => Msg::Key("="),
            (KeyCode::Escape, _) => Msg::Key("C"),
            (KeyCode::Backspace, _) => Msg::Backspace,
            (_, Some(c @ '0'..='9')) => Msg::Digit(c),
            (_, Some('.' | ',')) => Msg::Key("."),
            (_, Some('+')) => Msg::Key("+"),
            (_, Some('-')) => Msg::Key("−"),
            (_, Some('*' | 'x')) => Msg::Key("×"),
            (_, Some('/')) => Msg::Key("÷"),
            (_, Some('=')) => Msg::Key("="),
            (_, Some('%')) => Msg::Key("%"),
            (_, Some('c' | 'C')) => Msg::Key("C"),
            _ => return None,
        })
    }

    fn view(&self, env: &Env) -> View<'_, Msg> {
        let dark = env.dark;
        let (pad, gap) = (14, 10);
        let cw = (env.size.0 - 2 * pad - 3 * gap) / 4;
        let ch = 56;
        let keypad = VStack::with(
            KEYS.iter()
                .enumerate()
                .map(|(r, keys)| {
                    HStack::with(
                        keys.iter()
                            .enumerate()
                            .filter(|(_, k)| !k.is_empty())
                            .map(|(c, k)| {
                                let w = if r == 4 && c == 0 { 2 * cw + gap } else { cw };
                                let (style, tint) = if c == 3 {
                                    (ButtonStyle::Prominent, Color::Orange)
                                } else if r == 0 {
                                    let g = if dark { rgb(0x5A, 0x5A, 0x66) } else { rgb(0xD4, 0xD4, 0xDC) };
                                    (ButtonStyle::Bordered, Color::Custom(g))
                                } else {
                                    let g = if dark { rgb(0x36, 0x36, 0x40) } else { 0xFFFF_FFFF };
                                    (ButtonStyle::Bordered, Color::Custom(g))
                                };
                                Button::with(Text::new(*k).style(TextStyle::Sized(20, Weight::Regular)))
                                    .action(Msg::Key(k))
                                    .button_style(style)
                                    .tint(tint)
                                    .corner_radius(ch / 2)
                                    .size(w, ch)
                                    .accessibility_label(*k)
                            })
                            .collect(),
                    )
                    .spacing(gap)
                })
                .collect(),
        )
        .spacing(gap);
        let size = if self.display.len() > 11 { 28 } else { 40 };
        let bg = if dark { rgb(0x1C, 0x1C, 0x22) } else { rgb(0xF2, 0xF2, 0xF6) };
        VStack::new()
            .push(
                Text::new(self.hint().unwrap_or_default())
                    .style(TextStyle::Sized(14, Weight::Regular))
                    .foreground(Color::SecondaryLabel)
                    .text_align(Align::End)
                    .fill_width(),
            )
            .push(
                Text::new(self.display.clone())
                    .style(TextStyle::Sized(size, Weight::Regular))
                    .text_align(Align::End)
                    .fill_width(),
            )
            .push(Spacer::new())
            .push(keypad)
            .spacing(4)
            .alignment(Align::End)
            .padding_insets(Insets { top: 18, leading: 20, bottom: pad, trailing: 20 })
            .background(Color::Custom(bg))
            .fill()
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    waveui::run(Calculator::new())
}
