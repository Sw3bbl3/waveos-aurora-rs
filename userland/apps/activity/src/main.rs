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
use waveui::lumen::canvas::{with_alpha, Canvas as Surface};
use waveui::lumen::geom::Rect;
use waveui::lumen::theme;
use waveui::prelude::*;

const GRAPH_H: i32 = 150;
const ROW_H: i32 = 26;
const HISTORY: usize = 90;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Tab {
    Cpu,
    Memory,
    Disk,
    Network,
    System,
}

const TABS: [(Tab, &str); 5] = [
    (Tab::Cpu, "CPU"),
    (Tab::Memory, "Memory"),
    (Tab::Disk, "Disk"),
    (Tab::Network, "Network"),
    (Tab::System, "System"),
];

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
    selected: Option<u32>,
    confirm_quit: Option<(u32, String)>,
    last_sample: u64,
}

#[derive(Clone)]
pub enum Msg {
    Tab(usize),
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
        format!("{:.2} GB", n as f64 / (1u64 << 30) as f64)
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
            tab: Tab::Cpu,
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
    }

    // ------------------------------------------------------------ drawing

    fn graph(cv: &mut Surface, r: Rect, series: &[&VecDeque<u64>], max: u64, colors: &[u32]) {
        let t = theme::current();
        cv.fill_round_rect(r, 10, t.window_bg_alt);
        for k in 1..4 {
            cv.fill_rect(Rect::new(r.x + 10, r.y + r.h * k / 4, r.w - 20, 1), t.separator);
        }
        let inner = r.inset(8);
        for (si, data) in series.iter().enumerate() {
            let n = data.len();
            if n < 2 {
                continue;
            }
            let x_of = |i: usize| inner.x + inner.w * (HISTORY - n + i) as i32 / (HISTORY - 1) as i32;
            let y_of = |v: u64| inner.bottom() - (v.min(max) as i64 * inner.h as i64 / max.max(1) as i64) as i32;
            // Filled area under the line.
            let c = colors[si % colors.len()];
            for i in 0..n - 1 {
                let (x0, x1) = (x_of(i), x_of(i + 1));
                for x in x0..x1 {
                    let v = data[i] as i64
                        + (data[i + 1] as i64 - data[i] as i64) * (x - x0) as i64 / (x1 - x0).max(1) as i64;
                    let y = y_of(v as u64);
                    cv.fill_rect(Rect::new(x, y, 1, inner.bottom() - y), with_alpha(c, 0x30));
                }
            }
            let pts: Vec<(i32, i32)> = data.iter().enumerate().map(|(i, &v)| (x_of(i), y_of(v))).collect();
            cv.polyline(&pts, 2, c);
        }
    }
}

/// Process ids carry the user bit, so kernel tasks and processes don't mix.
fn row_id(r: &Row) -> u32 {
    r.info.pid | (r.info.user << 31)
}

const COLUMNS: [(Col, &str, i32); 6] = [
    (Col::Name, "Process", 0),
    (Col::Cpu, "% CPU", 80),
    (Col::Time, "CPU Time", 100),
    (Col::Threads, "Threads", 70),
    (Col::Memory, "Memory", 90),
    (Col::Pid, "PID", 60),
];

/// A statistic beside a graph: label above, value below.
fn stat<'a>(label: &str, value: String, color: Option<u32>) -> View<'a, Msg> {
    let text = VStack::new()
        .push(Text::new(label).style(TextStyle::Callout).foreground(Color::SecondaryLabel))
        .push(Text::new(value).style(TextStyle::Sized(15, waveui::style::Weight::Bold)))
        .spacing(2)
        .alignment(Align::Start);
    match color {
        Some(c) => HStack::new()
            .push(
                Canvas::new(move |cv, r| {
                    let (x, y) = r.center();
                    cv.fill_circle(x, y, 4, c);
                })
                .size(10, 10),
            )
            .push(text)
            .spacing(6)
            .alignment(Align::Start),
        None => text.padding_insets(Insets { leading: 16, ..Default::default() }),
    }
}

fn graph_view<'a>(series: Vec<VecDeque<u64>>, max: u64, colors: Vec<u32>, label: Option<String>) -> View<'a, Msg> {
    Canvas::new(move |cv, r| {
        let refs: Vec<&VecDeque<u64>> = series.iter().collect();
        Activity::graph(cv, r, &refs, max, &colors);
        if let Some(l) = &label {
            cv.text(r.x + 10, r.y + 18, l, theme::ui(11), theme::current().text_secondary);
        }
    })
    .fill()
}

impl Activity {
    fn graphs<'a>(&self, graph: View<'a, Msg>, stats: Vec<View<'a, Msg>>, height: i32) -> View<'a, Msg> {
        HStack::new()
            .push(graph.grow(1))
            .push(VStack::with(stats).spacing(14).alignment(Align::Start).width(176))
            .spacing(24)
            .alignment(Align::Start)
            .height(height)
            .fill_width()
    }

    fn table(&self) -> View<'_, Msg> {
        let header = HStack::with(
            COLUMNS
                .iter()
                .map(|&(c, label, w)| {
                    let mut t = Text::new(label)
                        .style(if self.sort.0 == c { TextStyle::Headline } else { TextStyle::Subheadline })
                        .foreground(Color::SecondaryLabel)
                        .text_align(if c == Col::Name { Align::Start } else { Align::End })
                        .padding_xy(12, 0)
                        .fill_height()
                        .on_tap(Msg::Sort(c))
                        .corner_radius(6);
                    t = if w == 0 { t.grow(1) } else { t.width(w) };
                    t
                })
                .collect(),
        )
        .spacing(0)
        .height(28)
        .fill_width();
        let rows = &self.rows;
        let list = List::new(rows.len(), ROW_H, move |i| {
            let row = &rows[i];
            let info = &row.info;
            let t = theme::current();
            // A dot for the state, then the name (kernel tasks in secondary text).
            let dot = match info.state {
                PROC_RUNNING => 0xFF30_D158,
                PROC_SLEEPING => with_alpha(t.text_secondary, 0x80),
                _ => 0xFFFF_9F0A,
            };
            let name = match (info.user, info.name()) {
                (0, n) => format!("{n} (kernel)"),
                (_, "Activity") => String::from("Pulse"),
                (_, n) => String::from(n),
            };
            let values = [
                format!("{}.{}", row.cpu_pct / 10, row.cpu_pct % 10),
                human_time(info.cpu_ms),
                format!("{}", info.threads),
                if info.user == 0 { String::from("—") } else { human_bytes(info.mem_kib * 1024) },
                // Kernel tasks have task ids, not process ids.
                if info.user == 0 { String::from("—") } else { format!("{}", info.pid) },
            ];
            let mut line = HStack::new()
                .push(
                    Canvas::new(move |cv, r| {
                        let (x, y) = r.center();
                        cv.fill_circle(x, y, 3, dot);
                    })
                    .size(20, 20),
                )
                .push(
                    Text::new(name)
                        .foreground(if info.user == 0 { Color::SecondaryLabel } else { Color::Label })
                        .grow(1),
                )
                .spacing(4)
                .padding_insets(Insets { leading: 6, ..Default::default() });
            for (k, v) in values.into_iter().enumerate() {
                line = line.push(Text::new(v).text_align(Align::End).padding_xy(12, 0).width(COLUMNS[k + 1].2));
            }
            line.spacing(0).fill()
        })
        .selection(self.selected.and_then(|id| self.rows.iter().position(|r| row_id(r) == id)))
        .on_select(Msg::Select)
        .id("processes");
        VStack::new()
            .push(header)
            .push(Divider::new())
            .push(list)
            .spacing(0)
            .background_rounded(Color::GroupedBackground, 10)
            .clipped()
            .fill()
    }

    fn content(&self) -> View<'_, Msg> {
        let s = self.stats;
        let blue = 0xFF0A_84FF;
        let green = 0xFF30_B050;
        let orange = 0xFFFF_9F0A;
        let deque = |q: &VecDeque<u32>| q.iter().map(|&v| v as u64).collect::<VecDeque<u64>>();
        match self.tab {
            Tab::Cpu => {
                let graph = if self.core_hist.len() > 1 {
                    // One small history graph per core, side by side.
                    HStack::with(
                        self.core_hist
                            .iter()
                            .enumerate()
                            .map(|(i, core)| {
                                graph_view(vec![deque(core)], 1000, vec![theme::accent()], Some(format!("CPU {i}")))
                                    .grow(1)
                            })
                            .collect(),
                    )
                    .spacing(8)
                    .fill()
                } else {
                    graph_view(vec![deque(&self.cpu_hist)], 1000, vec![theme::accent()], None)
                };
                let now = self.cpu_hist.back().copied().unwrap_or(0);
                VStack::new()
                    .push(self.graphs(
                        graph,
                        vec![
                            stat("CPU load", format!("{}.{}%", now / 10, now % 10), Some(theme::accent())),
                            stat("Idle", format!("{}.{}%", (1000 - now) / 10, (1000 - now) % 10), None),
                            stat("Tasks", format!("{} in {} processes", s.tasks, s.processes), None),
                        ],
                        GRAPH_H,
                    ))
                    .push(self.table())
                    .spacing(16)
                    .fill()
            }
            Tab::Memory => {
                let heap = format!("{} of {}", human_bytes(s.heap_used), human_bytes(s.heap_size));
                VStack::new()
                    .push(self.graphs(
                        graph_view(vec![deque(&self.mem_hist)], 1000, vec![green], None),
                        vec![
                            stat("Used", human_bytes(s.mem_used), Some(green)),
                            stat("Physical memory", human_bytes(s.mem_total), None),
                            stat("Kernel heap", heap, None),
                        ],
                        GRAPH_H,
                    ))
                    .push(self.table())
                    .spacing(16)
                    .fill()
            }
            Tab::Disk => {
                let max =
                    self.read_hist.iter().chain(self.write_hist.iter()).copied().max().unwrap_or(0).max(64 * 1024);
                let rd = self.read_hist.back().copied().unwrap_or(0);
                let wr = self.write_hist.back().copied().unwrap_or(0);
                let info = process::sys_info();
                let used = info.disk_total.saturating_sub(info.disk_free);
                let rows = [
                    ("Reads", format!("{} ({})", s.disk_reads, human_bytes(s.disk_read_bytes))),
                    ("Writes", format!("{} ({})", s.disk_writes, human_bytes(s.disk_write_bytes))),
                    ("Startup disk", process::fixed_str(&info.root, info.root_len)),
                ];
                VStack::new()
                    .push(self.graphs(
                        graph_view(
                            vec![self.read_hist.clone(), self.write_hist.clone()],
                            max,
                            vec![blue, orange],
                            None,
                        ),
                        vec![
                            stat("Reading", format!("{}/s", human_bytes(rd)), Some(blue)),
                            stat("Writing", format!("{}/s", human_bytes(wr)), Some(orange)),
                            stat("Scale", format!("{}/s", human_bytes(max)), None),
                        ],
                        GRAPH_H + 60,
                    ))
                    .push(
                        Section::new().extend(
                            rows.into_iter()
                                .map(|(k, v)| row(k, Text::new(v).foreground(Color::SecondaryLabel)).height(34)),
                        ),
                    )
                    .push(
                        VStack::new()
                            .push(ProgressView::new(Some(used as f32 / info.disk_total.max(1) as f32)))
                            .push(
                                Text::new(format!("{} used of {}", human_bytes(used), human_bytes(info.disk_total)))
                                    .style(TextStyle::Callout)
                                    .foreground(Color::SecondaryLabel),
                            )
                            .spacing(8)
                            .alignment(Align::Stretch),
                    )
                    .push(Spacer::new())
                    .spacing(16)
                    .alignment(Align::Stretch)
                    .fill()
            }
            Tab::Network => {
                let (purple, teal) = (0xFFBF_5AF2, 0xFF30_B0C7);
                let max = self.rx_hist.iter().chain(self.tx_hist.iter()).copied().max().unwrap_or(0).max(16 * 1024);
                let rx = self.rx_hist.back().copied().unwrap_or(0);
                let tx = self.tx_hist.back().copied().unwrap_or(0);
                let cols = [("Interface", 120), ("Driver", 120), ("Address", 140), ("Received", 0), ("Sent", 0)];
                let cell = |text: String, w: i32, head: bool| {
                    let t = Text::new(text)
                        .style(if head { TextStyle::Headline } else { TextStyle::Body })
                        .foreground(if head { Color::SecondaryLabel } else { Color::Label });
                    if w == 0 {
                        t.grow(1)
                    } else {
                        t.width(w)
                    }
                };
                let mut table = Section::new().push(
                    HStack::with(cols.iter().map(|&(h, w)| cell(h.into(), w, true)).collect()).height(32).fill_width(),
                );
                for i in corekit::net::interfaces().iter() {
                    let ip = if i.ip == [0; 4] { String::from("—") } else { corekit::net::ip_string(i.ip) };
                    let vals = [
                        String::from(corekit::net::name_of(i)),
                        String::from(corekit::net::driver_of(i)),
                        ip,
                        format!("{} ({} pkts)", human_bytes(i.rx_bytes), i.rx_packets),
                        format!("{} ({} pkts)", human_bytes(i.tx_bytes), i.tx_packets),
                    ];
                    table = table.push(
                        HStack::with(vals.into_iter().zip(cols).map(|(v, (_, w))| cell(v, w, false)).collect())
                            .height(32)
                            .fill_width(),
                    );
                }
                VStack::new()
                    .push(self.graphs(
                        graph_view(vec![self.rx_hist.clone(), self.tx_hist.clone()], max, vec![purple, teal], None),
                        vec![
                            stat("Receiving", format!("{}/s", human_bytes(rx)), Some(purple)),
                            stat("Sending", format!("{}/s", human_bytes(tx)), Some(teal)),
                            stat("Scale", format!("{}/s", human_bytes(max)), None),
                        ],
                        GRAPH_H + 60,
                    ))
                    .push(table)
                    .push(Spacer::new())
                    .spacing(16)
                    .alignment(Align::Stretch)
                    .fill()
            }
            Tab::System => {
                let up = s.uptime_ms / 1000;
                let rows = [
                    ("Uptime", format!("{}h {:02}m {:02}s", up / 3600, up / 60 % 60, up % 60)),
                    ("Processors", format!("{}", s.cpus)),
                    ("Processes", format!("{}", s.processes)),
                    ("Tasks (threads)", format!("{}", s.tasks)),
                    ("Windows", format!("{}", s.windows)),
                    ("Interrupts", format!("{} /s", self.rates.0)),
                    ("System calls", format!("{} /s", self.rates.1)),
                    ("Context switches", format!("{} /s", self.rates.2)),
                    ("Frames drawn", format!("{} /s", self.rates.3)),
                ];
                VStack::new()
                    .push(Section::new().extend(
                        rows.into_iter().map(|(k, v)| row(k, Text::new(v).style(TextStyle::Headline)).height(36)),
                    ))
                    .push(Spacer::new())
                    .alignment(Align::Stretch)
                    .fill()
            }
        }
    }
}

impl App for Activity {
    type Message = Msg;

    fn title(&self) -> String {
        String::from("Pulse")
    }

    fn size(&self) -> (i32, i32) {
        (860, 600)
    }

    fn update(&mut self, msg: Msg, _cx: &mut Context<Msg>) {
        match msg {
            Msg::Tab(i) => {
                self.tab = TABS[i].0;
                if self.tab == Tab::Memory && self.sort.0 == Col::Cpu {
                    self.sort = (Col::Memory, false);
                    self.sort_rows();
                }
            }
            Msg::Sort(c) => {
                self.sort = if self.sort.0 == c { (c, !self.sort.1) } else { (c, c == Col::Name || c == Col::Pid) };
                self.sort_rows();
            }
            Msg::Select(i) => self.selected = self.rows.get(i).map(row_id),
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
        let can_quit = self.selected.is_some_and(|id| id >> 31 == 1) && matches!(self.tab, Tab::Cpu | Tab::Memory);
        let bar = toolbar(vec![
            Spacer::new(),
            Picker::new(TABS.iter().map(|t| t.1), sel).on_change(Msg::Tab).width(450),
            Spacer::new(),
        ]);
        let quit = Button::new("Quit Process")
            .action(Msg::Quit)
            .disabled(!can_quit)
            .help("Quit the selected process")
            .shortcut(Shortcut::key(Key::Delete));
        VStack::new()
            .push(
                ZStack::with(vec![bar, HStack::new().push(Spacer::new()).push(quit).padding_xy(14, 0).fill()])
                    .height(size::TOOLBAR),
            )
            .push(Divider::new())
            .push(self.content().padding(16))
            .spacing(0)
            .fill()
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    waveui::run(Activity::new())
}
