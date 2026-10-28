//! WaveUI Gallery — the WaveSDK's example app, installed from a `.gina`
//! package: every control, in a few lines each.
#![no_std]
#![no_main]
extern crate alloc;

use waveui::prelude::*;

const COUNT: &str = "/AppData/dev.example.gallery/count.txt";

struct Gallery {
    count: u32,
    on: bool,
    value: f32,
    choice: usize,
    quantity: i64,
    name: String,
    agree: bool,
    confirm: bool,
}

#[derive(Clone)]
enum Msg {
    Click,
    Switch(bool),
    Slide(f32),
    Choose(usize),
    Quantity(i64),
    Name(String),
    Agree(bool),
    Reset,
    ConfirmReset,
    Cancel,
}

impl App for Gallery {
    type Message = Msg;

    fn title(&self) -> String {
        "WaveUI Gallery".into()
    }

    fn size(&self) -> (i32, i32) {
        (580, 620)
    }

    fn update(&mut self, msg: Msg, _cx: &mut Context<Msg>) {
        match msg {
            Msg::Click => {
                self.count += 1;
                let _ = corekit::fs::write(COUNT, format!("{}", self.count).as_bytes());
                corekit::println!("Count: {}", self.count);
            }
            Msg::Switch(on) => self.on = on,
            Msg::Slide(v) => self.value = v,
            Msg::Choose(i) => self.choice = i,
            Msg::Quantity(q) => self.quantity = q,
            Msg::Name(n) => self.name = n,
            Msg::Agree(a) => self.agree = a,
            Msg::Reset => self.confirm = true,
            Msg::ConfirmReset => {
                self.count = 0;
                self.confirm = false;
                let _ = corekit::fs::write(COUNT, b"0");
            }
            Msg::Cancel => self.confirm = false,
        }
    }

    fn sheet(&self, _env: &Env) -> Option<View<'_, Msg>> {
        self.confirm.then(|| {
            Alert::new("Reset the counter?", "The count saved in this app's data folder goes back to zero.")
                .button("Cancel", Msg::Cancel, AlertRole::Cancel)
                .button("Reset", Msg::ConfirmReset, AlertRole::Destructive)
                .view()
        })
    }

    fn view(&self, _env: &Env) -> View<'_, Msg> {
        let greeting =
            if self.name.trim().is_empty() { String::from("Hello!") } else { format!("Hello, {}!", self.name.trim()) };
        let content = VStack::new()
            .push(Text::new("Made with WaveUI").style(TextStyle::LargeTitle))
            .push(
                Text::paragraph("A real native app, installed from a .gina package.").foreground(Color::SecondaryLabel),
            )
            .push(Section::titled(
                "Buttons",
                Section::new()
                    .push(row(
                        format!("Clicked {} times", self.count),
                        HStack::new()
                            .push(Button::new("Reset").action(Msg::Reset).disabled(self.count == 0))
                            .push(Button::new("Click Me").action(Msg::Click).button_style(ButtonStyle::Prominent))
                            .spacing(8),
                    ))
                    .push(row("A plain button", Button::new("Learn more").button_style(ButtonStyle::Plain))),
            ))
            .push(Section::titled(
                "Controls",
                Section::new()
                    .push(Toggle::new("A switch", self.on).on_change(Msg::Switch).min_height(size::ROW))
                    .push(row("A slider", Slider::new(self.value, 0.0..=100.0).on_change(Msg::Slide).width(220)))
                    .push(row(
                        "A picker",
                        Picker::new(["Small", "Medium", "Large"], self.choice).on_change(Msg::Choose).width(240),
                    ))
                    .push(
                        Stepper::new("A stepper", self.quantity, 0..=10).on_change(Msg::Quantity).min_height(size::ROW),
                    )
                    .push(Toggle::checkbox("A check box", self.agree).on_change(Msg::Agree).min_height(size::ROW)),
            ))
            .push(Section::titled(
                "Text",
                Section::new()
                    .push(row("Your name", TextField::new("Name", self.name.clone()).on_change(Msg::Name).width(220)))
                    .push(row(greeting, ProgressView::new(Some(self.value / 100.0)).width(160))),
            ))
            .push(
                Text::paragraph("The click count is saved in this app's data folder.")
                    .style(TextStyle::Footnote)
                    .foreground(Color::SecondaryLabel),
            )
            .spacing(18)
            .alignment(Align::Stretch)
            .padding(28)
            .fill_width();
        ScrollView::new(content)
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    let count = corekit::fs::read_to_string(COUNT).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    waveui::run(Gallery {
        count,
        on: false,
        value: 60.0,
        choice: 1,
        quantity: 2,
        name: String::new(),
        agree: true,
        confirm: false,
    })
}
