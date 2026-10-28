//! Calculator.

#![no_std]
#![no_main]

extern crate alloc;

use waveui::prelude::*;
use waveui::style::Weight;

const KEYS: [[&str; 4]; 5] =
    [["C", "±", "%", "÷"], ["7", "8", "9", "×"], ["4", "5", "6", "−"], ["1", "2", "3", "+"], ["0", "", ".", "="]];

pub struct Calculator {
    display: String,
    acc: Option<f64>,
    op: Option<char>,
    fresh: bool,
    /// Finished sums, oldest first: ("128 × 3", "384").
    tape: Vec<(String, String)>,
    show_tape: bool,
}

#[derive(Clone)]
pub enum Msg {
    Key(&'static str),
    Digit(char),
    Backspace,
    Copy,
    ToggleTape,
    ClearTape,
}

impl Calculator {
    fn new() -> Self {
        Self { display: "0".into(), acc: None, op: None, fresh: true, tape: Vec::new(), show_tape: false }
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
            let expr = format!(
                "{} {} {}",
                self.hint().unwrap_or_default().trim_end_matches(&[' ', '+', '−', '×', '÷'][..]),
                symbol(op),
                grouped(&self.display)
            );
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
            self.tape.push((expr, grouped(&self.display)));
            if self.tape.len() > 50 {
                self.tape.remove(0);
            }
        }
        self.acc = None;
        self.op = None;
    }

    fn press(&mut self, key: &str) {
        match key {
            "C" => {
                if self.fresh || self.display == "0" {
                    // AC: everything.
                    self.acc = None;
                    self.op = None;
                }
                self.display = "0".into();
                self.fresh = true;
            }
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
        Some(format!("{} {}", grouped(&tmp.display), symbol(op)))
    }

    /// AC clears everything; C only the number being typed.
    fn clear_label(&self) -> &'static str {
        if self.fresh || self.display == "0" {
            "AC"
        } else {
            "C"
        }
    }
}

fn symbol(op: char) -> &'static str {
    match op {
        '+' => "+",
        '-' => "−",
        '*' => "×",
        _ => "÷",
    }
}

/// "1024.5" as "1,024.5".
fn grouped(s: &str) -> String {
    let (sign, rest) = s.strip_prefix('-').map_or(("", s), |r| ("−", r));
    if !rest.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
        return String::from(s);
    }
    let (int, frac) = rest.split_once('.').map_or((rest, None), |(i, f)| (i, Some(f)));
    let mut out = String::from(sign);
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if let Some(f) = frac {
        out.push('.');
        out.push_str(f);
    }
    out
}

impl App for Calculator {
    type Message = Msg;

    fn title(&self) -> String {
        "Calculator".into()
    }
    fn size(&self) -> (i32, i32) {
        (330, 520)
    }
    fn resizable(&self) -> bool {
        false
    }

    fn update(&mut self, msg: Msg, cx: &mut Context<Msg>) {
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
            Msg::Copy => cx.copy(&self.display),
            Msg::ToggleTape => self.show_tape = !self.show_tape,
            Msg::ClearTape => self.tape.clear(),
        }
    }

    fn key(&self, ev: &KeyEvent) -> Option<Msg> {
        if ev.mods.ctrl {
            return match ev.ch.map(|c| c.to_ascii_lowercase()) {
                Some('c') => Some(Msg::Copy),
                Some('t') => Some(Msg::ToggleTape),
                _ => None,
            };
        }
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

    fn sheet(&self, _env: &Env) -> Option<View<'_, Msg>> {
        if !self.show_tape {
            return None;
        }
        let mut rows = VStack::new()
            .push(
                HStack::new()
                    .push(Text::label("Tape"))
                    .push(Spacer::new())
                    .push(
                        Button::new("CLEAR")
                            .action(Msg::ClearTape)
                            .button_style(ButtonStyle::Plain)
                            .style(TextStyle::Label)
                            .foreground(Color::TertiaryLabel)
                            .disabled(self.tape.is_empty()),
                    )
                    .padding_insets(Insets { top: 0, leading: 0, bottom: 6, trailing: 0 })
                    .fill_width(),
            )
            .spacing(0)
            .alignment(Align::Stretch);
        if self.tape.is_empty() {
            rows = rows.push(
                Text::new("Finished sums show here.")
                    .style(TextStyle::Callout)
                    .foreground(Color::SecondaryLabel)
                    .padding_xy(0, 10),
            );
        }
        for (expr, result) in self.tape.iter().rev().take(12).rev() {
            rows = rows
                .push(
                    HStack::new()
                        .push(Text::new(expr.clone()).style(TextStyle::Monospaced).foreground(Color::SecondaryLabel))
                        .push(Spacer::new())
                        .push(Text::new(result.clone()).style(TextStyle::Sized(13, Weight::Bold)))
                        .height(32)
                        .fill_width(),
                )
                .push(Divider::new());
        }
        rows = rows.push(
            HStack::new()
                .push(Text::new("Copy result").style(TextStyle::Callout).foreground(Color::SecondaryLabel))
                .push(Spacer::new())
                .push(Text::new("^C").style(TextStyle::Sized(13, Weight::Bold)).foreground(Color::AccentText))
                .height(34)
                .fill_width()
                .on_tap(Msg::Copy),
        );
        rows = rows.push(
            HStack::new()
                .push(Spacer::new())
                .push(
                    Button::new("Done")
                        .action(Msg::ToggleTape)
                        .shortcut(Shortcut::cancel())
                        .control_size(ControlSize::Small),
                )
                .padding_insets(Insets { top: 8, leading: 0, bottom: 0, trailing: 0 })
                .fill_width(),
        );
        Some(rows.width(264))
    }

    fn view(&self, _env: &Env) -> View<'_, Msg> {
        let (gap, kw, kh) = (10, 66, 58);
        let pending = if self.fresh { self.op } else { None };
        let key = |k: &'static str, r: usize, c: usize| -> View<'_, Msg> {
            let label = if k == "C" { self.clear_label() } else { k };
            let w = if r == 4 && c == 0 { 2 * kw + gap } else { kw };
            let text = Text::new(label).style(TextStyle::Sized(if c == 3 { 22 } else { 20 }, Weight::Regular));
            let text = if r == 4 && c == 0 { text.padding_xy(22, 0).fill_width().align(Align::Start) } else { text };
            let b = Button::with(text).action(Msg::Key(k)).corner_radius(kh / 2).size(w, kh).accessibility_label(label);
            if c == 3 {
                let op = match k {
                    "+" => Some('+'),
                    "−" => Some('-'),
                    "×" => Some('*'),
                    "÷" => Some('/'),
                    _ => None,
                };
                if op.is_some() && op == pending {
                    // The operation waiting for its second number: a ring.
                    b.button_style(ButtonStyle::Toolbar).border(Color::Accent, kh / 2).foreground(Color::AccentText)
                } else {
                    b.button_style(ButtonStyle::Prominent)
                }
            } else {
                let fill = if r == 0 { Color::TertiaryFill } else { Color::SecondaryFill };
                b.button_style(ButtonStyle::Toolbar).background_rounded(fill, kh / 2)
            }
        };
        let keypad = VStack::with(
            KEYS.iter()
                .enumerate()
                .map(|(r, keys)| {
                    HStack::with(
                        keys.iter().enumerate().filter(|(_, k)| !k.is_empty()).map(|(c, k)| key(k, r, c)).collect(),
                    )
                    .spacing(gap)
                })
                .collect(),
        )
        .spacing(gap);
        let shown = grouped(&self.display);
        let n = shown.chars().count();
        let size = if n > 12 {
            30
        } else if n > 9 {
            40
        } else {
            56
        };
        VStack::new()
            .push(Spacer::new())
            .push(
                Text::new(self.hint().unwrap_or_default())
                    .style(TextStyle::Sized(15, Weight::Regular))
                    .foreground(Color::SecondaryLabel)
                    .text_align(Align::End)
                    .fill_width(),
            )
            .push(
                Text::new(shown)
                    .style(TextStyle::Sized(size, Weight::Light))
                    .text_align(Align::End)
                    .fill_width()
                    .context_menu(vec![
                        MenuItem::new("Copy", Msg::Copy),
                        MenuItem::new(if self.show_tape { "Hide Tape" } else { "Show Tape" }, Msg::ToggleTape),
                    ]),
            )
            .push(keypad.padding_insets(Insets { top: 14, leading: 0, bottom: 0, trailing: 0 }))
            .spacing(2)
            .alignment(Align::End)
            .padding_insets(Insets { top: 18, leading: 18, bottom: 22, trailing: 18 })
            .fill()
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    waveui::run(Calculator::new())
}
