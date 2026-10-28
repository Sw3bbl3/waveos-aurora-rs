//! Pulse — what the computer is doing: processes, CPU, memory,
//! disk and system counters, with live graphs. Select a process and Quit it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use corekit::abi::{ProcInfo, SysStats, PROC_EXITED, PROC_RUNNING, PROC_SLEEPING};
use corekit::process;
use waveui::lumen::canvas::with_alpha;
use waveui::lumen::geom::Rect;
use waveui::lumen::theme;
use waveui::prelude::*;

const ROW_H: i32 = 37;
const HISTORY: usize = 90;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Tab {
    Overview,
    Processes,
    System,
}

const TABS: [(Tab, &str); 3] = [(Tab::Overview, "Overview"), (Tab::Processes, "Processes"), (Tab::System, "System")];

// The four gauges' colours: processor, memory, disk, network.
const TEAL: u32 = 0xFF2D_D4BF;
const PURPLE: u32 = 0xFFA8_55F7;
const AMBER: u32 = 0xFFF5_9E0B;
const GREEN: u32 = 0xFF34_D399;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Col {
    Name,
    Cpu,
    Time,
    Threads,
    Memory,
    Pid,
}

struct Row {
    info: ProcInfo,
    cpu_pct: u32, // tenths of a percent
}

pub struct Activity {
    tab: Tab,
    rows: Vec<Row>,
    prev_cpu: BTreeMap<(u32, u32), u64>,
    prev: Option<SysStats>,
    stats: SysStats,
    cpu_hist: VecDeque<u32>,
    /// Load history of each core (tenths of a percent).
    core_hist: Vec<VecDeque<u32>>,
    mem_hist: VecDeque<u32>,
    read_hist: VecDeque<u64>,
    write_hist: VecDeque<u64>,
    /// Bytes received and sent per second, and the totals last sampled.
    rx_hist: VecDeque<u64>,
    tx_hist: VecDeque<u64>,
    net_prev: Option<(u64, u64)>,
    rates: (u64, u64, u64, u64), // interrupts/s, syscalls/s, switches/s, frames/s
    sort: (Col, bool),
    /// Seek: only processes whose name holds this.
    query: String,
    /// Indices into `rows` of the processes shown.
    shown: Vec<usize>,
    selected: Option<u32>,
    confirm_quit: Option<(u32, String)>,
    last_sample: u64,
}

#[derive(Clone)]
pub enum Msg {
    Tab(usize),
    Seek(String),
    Sort(Col),
    Select(usize),
    Quit,
    ConfirmQuit,
    CancelQuit,
}

fn push<T>(q: &mut VecDeque<T>, v: T) {
    if q.len() == HISTORY {
        q.pop_front();
    }
    q.push_back(v);
}

fn human_bytes(n: u64) -> String {
    if n >= 1 << 30 {
        format!("{:.1} GB", n as f64 / (1u64 << 30) as f64)
    } else if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / (1u64 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

fn human_time(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}.{:02}", s / 60, s % 60, ms / 10 % 100)
    }
}

impl Activity {
    fn new() -> Self {
        let mut a = Activity {
            tab: Tab::Overview,
            rows: Vec::new(),
            prev_cpu: BTreeMap::new(),
            prev: None,
            stats: SysStats::default(),
            cpu_hist: VecDeque::new(),
            core_hist: Vec::new(),
            mem_hist: VecDeque::new(),
            read_hist: VecDeque::new(),
            write_hist: VecDeque::new(),
            rx_hist: VecDeque::new(),
            tx_hist: VecDeque::new(),
            net_prev: None,
            rates: (0, 0, 0, 0),
            sort: (Col::Cpu, false),
            query: String::new(),
            shown: Vec::new(),
            selected: None,
            confirm_quit: None,
            last_sample: 0,
        };
        a.sample();
        a
    }

    /// Reads the counters and turns them into rates since the last sample.
    fn sample(&mut self) {
        let now = corekit::time::uptime_ms();
        let s = process::sys_stats();
        let dt = now.saturating_sub(self.last_sample).max(1);
        if let Some(p) = &self.prev {
            let n = (s.cpus as usize).clamp(1, s.cpu.len());
            self.core_hist.resize_with(n, VecDeque::new);
            let (mut all_busy, mut all_idle) = (0, 0);
            for (i, hist) in self.core_hist.iter_mut().enumerate() {
                let busy = s.cpu[i].busy_ms.saturating_sub(p.cpu[i].busy_ms);
                let idle = s.cpu[i].idle_ms.saturating_sub(p.cpu[i].idle_ms);
                push(hist, (busy * 1000 / (busy + idle).max(1)) as u32);
                all_busy += busy;
                all_idle += idle;
            }
            push(&mut self.cpu_hist, (all_busy * 1000 / (all_busy + all_idle).max(1)) as u32);
            let rate = |a: u64, b: u64| a.saturating_sub(b) * 1000 / dt;
            push(&mut self.read_hist, rate(s.disk_read_bytes, p.disk_read_bytes));
            push(&mut self.write_hist, rate(s.disk_write_bytes, p.disk_write_bytes));
            self.rates = (
                rate(s.interrupts, p.interrupts),
                rate(s.syscalls, p.syscalls),
                rate(s.context_switches, p.context_switches),
                rate(s.frames, p.frames),
            );
        }
        push(&mut self.mem_hist, (s.mem_used * 1000 / s.mem_total.max(1)) as u32);
        // Network: all adapters but loopback.
        let (rx, tx) = corekit::net::interfaces()
            .iter()
            .filter(|i| i.flags & corekit::abi::net::IF_LOOPBACK == 0)
            .fold((0u64, 0u64), |(r, t), i| (r + i.rx_bytes, t + i.tx_bytes));
        if let Some((pr, pt)) = self.net_prev {
            push(&mut self.rx_hist, rx.saturating_sub(pr) * 1000 / dt);
            push(&mut self.tx_hist, tx.saturating_sub(pt) * 1000 / dt);
        }
        self.net_prev = Some((rx, tx));

        let mut rows = Vec::new();
        let mut seen = BTreeMap::new();
        for info in process::list() {
            if info.state == PROC_EXITED {
                continue;
            }
            let key = (info.user, info.pid);
            let before = self.prev_cpu.get(&key).copied().unwrap_or(info.cpu_ms);
            let cpu_pct = (info.cpu_ms.saturating_sub(before) * 1000 / dt) as u32;
            seen.insert(key, info.cpu_ms);
            rows.push(Row { info, cpu_pct: cpu_pct.min(1000) });
        }
        self.prev_cpu = seen;
        self.rows = rows;
        self.sort_rows();
        self.prev = Some(s);
        self.stats = s;
        self.last_sample = now;
    }

    fn sort_rows(&mut self) {
        let (col, asc) = self.sort;
        self.rows.sort_by(|a, b| {
            let o = match col {
                Col::Name => a.info.name().to_lowercase().cmp(&b.info.name().to_lowercase()),
                Col::Cpu => a.cpu_pct.cmp(&b.cpu_pct).then(a.info.cpu_ms.cmp(&b.info.cpu_ms)),
                Col::Time => a.info.cpu_ms.cmp(&b.info.cpu_ms),
                Col::Threads => a.info.threads.cmp(&b.info.threads),
                Col::Memory => a.info.mem_kib.cmp(&b.info.mem_kib),
                Col::Pid => (a.info.user, a.info.pid).cmp(&(b.info.user, b.info.pid)),
            };
            if asc {
                o
            } else {
                o.reverse()
            }
        });
        self.refilter();
    }

    fn refilter(&mut self) {
        let q = self.query.to_lowercase();
        self.shown =
            (0..self.rows.len()).filter(|&i| q.is_empty() || self.name(i).to_lowercase().contains(&q)).collect();
    }

    /// The name a person knows a process by.
    fn name(&self, i: usize) -> String {
        let info = &self.rows[i].info;
        match (info.user, info.name()) {
            (0, n) => format!("{n} (kernel)"),
            (_, "Activity") => String::from("Pulse"),
            (_, n) => String::from(n),
        }
    }
}

/// Process ids carry the user bit, so kernel tasks and processes don't mix.
fn row_id(r: &Row) -> u32 {
    r.info.pid | (r.info.user << 31)
}

/// Table columns: what, heading, width (0 takes what's left).
const COLUMNS: [(Col, &str, i32); 6] = [
    (Col::Name, "Process", 0),
    (Col::Cpu, "CPU", 110),
    (Col::Time, "CPU Time", 120),
    (Col::Memory, "Memory", 130),
    (Col::Threads, "Threads", 110),
    (Col::Pid, "PID", 90),
];

/// A smooth area graph of `data` (0..=max), newest on the right.
fn sparkline<'a>(data: VecDeque<u64>, max: u64, color: u32) -> View<'a, Msg> {
    Canvas::new(move |cv, r| {
        let n = data.len();
        if n < 2 {
            cv.fill_rect(Rect::new(r.x, r.bottom() - 1, r.w, 1), with_alpha(color, 0x60));
            return;
        }
        let x_of = |i: usize| r.x + r.w * (HISTORY - n + i) as i32 / (HISTORY - 1) as i32;
        let top = r.y + 4;
        let y_of = |v: u64| r.bottom() - (v.min(max) as i64 * (r.bottom() - top) as i64 / max.max(1) as i64) as i32;
        for i in 0..n - 1 {
            let (x0, x1) = (x_of(i), x_of(i + 1));
            for x in x0..x1 {
                let v =
                    data[i] as i64 + (data[i + 1] as i64 - data[i] as i64) * (x - x0) as i64 / (x1 - x0).max(1) as i64;
                let y = y_of(v as u64);
                cv.fill_rect(Rect::new(x, y, 1, r.bottom() - y), with_alpha(color, 0x48));
            }
        }
        let pts: Vec<(i32, i32)> = data.iter().enumerate().map(|(i, &v)| (x_of(i), y_of(v))).collect();
        cv.polyline(&pts, 2, color);
    })
    .height(56)
    .fill_width()
}

/// One gauge: a name and a detail, the reading, and its recent past.
fn gauge<'a>(name: &str, detail: String, value: String, graph: View<'a, Msg>) -> View<'a, Msg> {
    VStack::new()
        .push(
            HStack::new()
                .push(
                    Text::new(name)
                        .style(TextStyle::Sized(12, waveui::style::Weight::Bold))
                        .foreground(Color::SecondaryLabel),
                )
                .push(Spacer::new())
                .push(
                    Text::new(detail)
                        .style(TextStyle::Sized(11, waveui::style::Weight::Bold))
                        .foreground(Color::TertiaryLabel),
                )
                .fill_width(),
        )
        .push(Text::new(value).style(TextStyle::Sized(24, waveui::style::Weight::Display)))
        .push(Spacer::new())
        .push(graph)
        .spacing(4)
        .alignment(Align::Stretch)
        .padding_insets(Insets { top: 16, leading: 16, bottom: 10, trailing: 16 })
        .height(150)
        .background_rounded(Color::Fill, 16)
        .border(Color::Separator, 16)
        .grow(1)
}

/// A small square with the first letter, standing in for an icon.
fn badge<'a>(name: &str, kernel: bool) -> View<'a, Msg> {
    let letter: String = name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
    let hue = name.bytes().fold(7u32, |h, b| h.wrapping_mul(31) ^ b as u32);
    let colors = [TEAL, PURPLE, AMBER, GREEN, 0xFF60_A5FA, 0xFFF4_72B6];
    let c = if kernel { 0xFF6B_7284 } else { colors[hue as usize % colors.len()] };
    Canvas::new(move |cv, r| {
        let b = Rect::new(r.x, r.y + (r.h - 20) / 2, 20, 20);
        cv.fill_round_rect(b, 6, c);
        let f = theme::ui_bold(11);
        cv.text_centered(b, &letter, f, 0xFFFF_FFFF);
    })
    .size(20, 22)
}

impl Activity {
    fn gauges(&self) -> View<'_, Msg> {
        let s = self.stats;
        let deque = |q: &VecDeque<u32>| q.iter().map(|&v| v as u64).collect::<VecDeque<u64>>();
        let cpu = self.cpu_hist.back().copied().unwrap_or(0);
        let rd = self.read_hist.back().copied().unwrap_or(0);
        let wr = self.write_hist.back().copied().unwrap_or(0);
        let disk: VecDeque<u64> = self.read_hist.iter().zip(self.write_hist.iter()).map(|(a, b)| a + b).collect();
        let disk_max = disk.iter().copied().max().unwrap_or(0).max(256 * 1024);
        let rx = self.rx_hist.back().copied().unwrap_or(0);
        let tx = self.tx_hist.back().copied().unwrap_or(0);
        let net: VecDeque<u64> = self.rx_hist.iter().zip(self.tx_hist.iter()).map(|(a, b)| a + b).collect();
        let net_max = net.iter().copied().max().unwrap_or(0).max(64 * 1024);
        let adapters =
            corekit::net::interfaces().iter().filter(|i| i.flags & corekit::abi::net::IF_LOOPBACK == 0).count();
        let pressure = if s.mem_used * 10 > s.mem_total * 9 { "under pressure" } else { "no pressure" };
        HStack::new()
            .push(gauge(
                "Processor",
                format!("{} core{}", s.cpus.max(1), if s.cpus == 1 { "" } else { "s" }),
                format!("{}%", (cpu + 5) / 10),
                sparkline(deque(&self.cpu_hist), 1000, TEAL),
            ))
            .push(gauge(
                "Memory",
                format!("of {} · {pressure}", human_bytes(s.mem_total)),
                human_bytes(s.mem_used),
                sparkline(deque(&self.mem_hist), 1000, PURPLE),
            ))
            .push(gauge(
                "Disk",
                format!("read {} · write {}", human_bytes(rd), human_bytes(wr)),
                format!("{}/s", human_bytes(rd + wr)),
                sparkline(disk, disk_max, AMBER),
            ))
            .push(gauge(
                "Network",
                format!("{adapters} adapter{}", if adapters == 1 { "" } else { "s" }),
                format!("{}/s", human_bytes(rx + tx)),
                sparkline(net, net_max, GREEN),
            ))
            .spacing(12)
            .fill_width()
    }

    /// The processes, in a rounded card. `all` adds CPU time and PID.
    fn table(&self, all: bool) -> View<'_, Msg> {
        let cols: Vec<(Col, &str, i32)> =
            COLUMNS.iter().copied().filter(|(c, ..)| all || !matches!(c, Col::Time | Col::Pid)).collect();
        fn cell(t: View<'_, Msg>, c: Col, w: i32) -> View<'_, Msg> {
            let t = t.text_align(if c == Col::Name { Align::Start } else { Align::End });
            if w == 0 {
                t.grow(1)
            } else {
                t.width(w)
            }
        }
        let header = HStack::with(
            cols.iter()
                .map(|&(c, label, w)| {
                    let arrow = if self.sort.0 == c {
                        if self.sort.1 {
                            " ↑"
                        } else {
                            " ↓"
                        }
                    } else {
                        ""
                    };
                    cell(Text::label(format!("{label}{arrow}")), c, w).fill_height().on_tap(Msg::Sort(c))
                })
                .collect(),
        )
        .spacing(0)
        .padding_xy(16, 0)
        .height(36)
        .fill_width();
        let shown = self.shown.clone();
        let list = List::new(shown.len(), ROW_H, move |k| {
            let i = shown[k];
            let row = &self.rows[i];
            let info = &row.info;
            let kernel = info.user == 0;
            let name = self.name(i);
            let mut line = HStack::new().spacing(0).padding_xy(16, 0);
            for &(c, _, w) in &cols {
                let v = match c {
                    Col::Name => {
                        line = line.push(
                            HStack::new()
                                .push(badge(&name, kernel))
                                .push(Text::new(name.clone()).foreground(if kernel {
                                    Color::SecondaryLabel
                                } else {
                                    Color::Label
                                }))
                                .spacing(10)
                                .grow(1),
                        );
                        continue;
                    }
                    Col::Cpu => format!("{}.{}%", row.cpu_pct / 10, row.cpu_pct % 10),
                    Col::Time => human_time(info.cpu_ms),
                    Col::Threads => format!("{}", info.threads),
                    Col::Memory if kernel => String::from("—"),
                    Col::Memory => human_bytes(info.mem_kib * 1024),
                    // Kernel tasks have task ids, not process ids.
                    Col::Pid if kernel => String::from("—"),
                    Col::Pid => format!("{}", info.pid),
                };
                line = line.push(cell(Text::new(v).style(TextStyle::Sized(13, waveui::style::Weight::Bold)), c, w));
            }
            let dot = match info.state {
                PROC_RUNNING => Some(0xFF30_D158u32),
                PROC_SLEEPING => None,
                _ => Some(0xFFFF_9F0A),
            };
            let line = line.fill();
            match dot {
                Some(c) if all => line.overlay_draw(move |cv, r| cv.fill_circle(r.x + 7, r.y + r.h / 2, 3, c)),
                _ => line,
            }
        })
        .selection(self.selected.and_then(|id| self.shown.iter().position(|&i| row_id(&self.rows[i]) == id)))
        .on_select(Msg::Select)
        .id("processes");
        VStack::new()
            .push(header)
            .push(Divider::new())
            .push(list)
            .spacing(0)
            .background_rounded(Color::Fill, 16)
            .border(Color::Separator, 16)
            .clipped()
            .fill()
    }

    fn system(&self) -> View<'_, Msg> {
        let s = self.stats;
        let up = s.uptime_ms / 1000;
        let info = process::sys_info();
        let used = info.disk_total.saturating_sub(info.disk_free);
        let counters = [
            ("Uptime", format!("{}h {:02}m {:02}s", up / 3600, up / 60 % 60, up % 60)),
            ("Processors", format!("{}", s.cpus)),
            ("Processes", format!("{} ({} tasks)", s.processes, s.tasks)),
            ("Windows", format!("{}", s.windows)),
            ("Interrupts", format!("{} /s", self.rates.0)),
            ("System calls", format!("{} /s", self.rates.1)),
            ("Context switches", format!("{} /s", self.rates.2)),
            ("Frames drawn", format!("{} /s", self.rates.3)),
            ("Kernel heap", format!("{} of {}", human_bytes(s.heap_used), human_bytes(s.heap_size))),
        ];
        let disk = [
            ("Startup disk", process::fixed_str(&info.root, info.root_len)),
            ("Used", format!("{} of {}", human_bytes(used), human_bytes(info.disk_total))),
            ("Reads", format!("{} ({})", s.disk_reads, human_bytes(s.disk_read_bytes))),
            ("Writes", format!("{} ({})", s.disk_writes, human_bytes(s.disk_write_bytes))),
        ];
        let mut net = Section::new();
        for i in corekit::net::interfaces().iter() {
            let ip = if i.ip == [0; 4] { String::from("no address") } else { corekit::net::ip_string(i.ip) };
            net = net.push(row_detail(
                format!("{} · {}", corekit::net::name_of(i), corekit::net::driver_of(i)),
                ip,
                Text::new(format!("↓ {}  ↑ {}", human_bytes(i.rx_bytes), human_bytes(i.tx_bytes)))
                    .foreground(Color::SecondaryLabel),
            ));
        }
        let card = |rows: &[(&str, String)]| {
            Section::new()
                .extend(rows.iter().map(|(k, v)| row(*k, Text::new(v.clone()).foreground(Color::SecondaryLabel))))
        };
        ScrollView::new(
            HStack::new()
                .push(Section::titled("Counters", card(&counters)).grow(1))
                .push(
                    VStack::new()
                        .push(Section::titled("Disk", card(&disk)))
                        .push(Section::titled("Network", net))
                        .spacing(18)
                        .alignment(Align::Stretch)
                        .grow(1),
                )
                .spacing(16)
                .alignment(Align::Start)
                .fill_width(),
        )
    }
}

impl App for Activity {
    type Message = Msg;

    fn title(&self) -> String {
        String::from("Pulse")
    }

    fn size(&self) -> (i32, i32) {
        (1140, 720)
    }

    fn update(&mut self, msg: Msg, _cx: &mut Context<Msg>) {
        match msg {
            Msg::Tab(i) => self.tab = TABS[i].0,
            Msg::Seek(q) => {
                self.query = q;
                self.refilter();
            }
            Msg::Sort(c) => {
                self.sort = if self.sort.0 == c { (c, !self.sort.1) } else { (c, c == Col::Name || c == Col::Pid) };
                self.sort_rows();
            }
            Msg::Select(k) => self.selected = self.shown.get(k).map(|&i| row_id(&self.rows[i])),
            Msg::Quit => {
                if let Some(id) = self.selected.filter(|id| id >> 31 == 1) {
                    let pid = id & 0x7FFF_FFFF;
                    let name = self
                        .rows
                        .iter()
                        .find(|r| r.info.user == 1 && r.info.pid == pid)
                        .map(|r| String::from(r.info.name()));
                    self.confirm_quit = Some((pid, name.unwrap_or_default()));
                }
            }
            Msg::ConfirmQuit => {
                if let Some((pid, _)) = self.confirm_quit.take() {
                    let _ = process::kill(pid);
                    self.selected = None;
                }
            }
            Msg::CancelQuit => self.confirm_quit = None,
        }
    }

    fn poll_interval(&self) -> Option<u64> {
        Some(1000)
    }

    fn poll(&mut self, _cx: &mut Context<Msg>) -> bool {
        self.sample();
        true
    }

    fn sheet(&self, _env: &Env) -> Option<View<'_, Msg>> {
        let (_, name) = self.confirm_quit.as_ref()?;
        Some(
            Alert::new(format!("Quit “{name}”?"), "Unsaved changes in it will be lost.")
                .button("Cancel", Msg::CancelQuit, AlertRole::Cancel)
                .button("Force Quit", Msg::ConfirmQuit, AlertRole::Destructive)
                .view(),
        )
    }

    fn view(&self, _env: &Env) -> View<'_, Msg> {
        let sel = TABS.iter().position(|(k, _)| *k == self.tab).unwrap_or(0);
        let can_quit = self.selected.is_some_and(|id| id >> 31 == 1) && self.tab != Tab::System;
        let mut bar = vec![Picker::new(TABS.iter().map(|t| t.1), sel).on_change(Msg::Tab).width(300), Spacer::new()];
        if self.tab != Tab::System {
            bar.push(
                TextField::search("Seek processes", self.query.clone())
                    .on_change(Msg::Seek)
                    .shortcut(Shortcut::ctrl('f'))
                    .id("seek")
                    .width(200),
            );
            bar.push(
                Button::new("Quit process")
                    .action(Msg::Quit)
                    .control_size(ControlSize::Small)
                    .disabled(!can_quit)
                    .help("Quit the selected process")
                    .shortcut(Shortcut::key(Key::Delete)),
            );
        }
        let content = match self.tab {
            Tab::Overview => VStack::new().push(self.gauges()).push(self.table(false)).spacing(16).fill(),
            Tab::Processes => self.table(true),
            Tab::System => self.system(),
        };
        VStack::new().push(toolbar(bar)).push(content.padding(16)).spacing(0).fill()
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    waveui::run(Activity::new())
}
