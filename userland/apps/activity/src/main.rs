//! Activity Monitor — what the computer is doing: processes, CPU, memory,
//! disk and system counters, with live graphs. Select a process and Quit it.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aurora::abi::{ProcInfo, SysStats, PROC_EXITED, PROC_RUNNING, PROC_SLEEPING};
use aurora::process;
use ripple::canvas::{with_alpha, Canvas};
use ripple::geom::Rect;
use ripple::theme;
use ripple::widgets::{self, button, ButtonStyle};
use ripple::{App, Env, KeyCode, KeyEvent};

const TOOLBAR_H: i32 = 52;
const GRAPH_H: i32 = 150;
const ROW_H: i32 = 26;
const HISTORY: usize = 90;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Cpu,
    Memory,
    Disk,
    Network,
    System,
}

const TABS: [(Tab, &str); 5] =
    [(Tab::Cpu, "CPU"), (Tab::Memory, "Memory"), (Tab::Disk, "Disk"), (Tab::Network, "Network"), (Tab::System, "System")];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Col {
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
    scroll: i32,
    hover: Option<Hover>,
    confirm_quit: Option<(u32, String)>,
    last_sample: u64,
    area: Rect,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hover {
    Tab(usize),
    Column(Col),
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
            scroll: 0,
            hover: None,
            confirm_quit: None,
            last_sample: 0,
            area: Rect::new(0, 0, 820, 560),
        };
        a.sample();
        a
    }

    /// Reads the counters and turns them into rates since the last sample.
    fn sample(&mut self) {
        let now = aurora::time::uptime_ms();
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
        let (rx, tx) = aurora::net::interfaces()
            .iter()
            .filter(|i| i.flags & aurora::abi::net::IF_LOOPBACK == 0)
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

    fn tabs(area: Rect) -> Rect {
        Rect::new(area.x + (area.w - 450) / 2, area.y + 11, 450, 30)
    }

    fn quit_button(area: Rect) -> Rect {
        Rect::new(area.right() - 110, area.y + 11, 96, 30)
    }

    fn graph_rect(area: Rect) -> Rect {
        Rect::new(area.x + 16, area.y + TOOLBAR_H + 14, area.w - 32, GRAPH_H)
    }

    fn table_rect(&self, area: Rect) -> Rect {
        let top = match self.tab {
            Tab::Cpu | Tab::Memory => Self::graph_rect(area).bottom() + 16,
            _ => area.bottom(),
        };
        Rect::new(area.x + 16, top, area.w - 32, area.bottom() - top - 12)
    }

    fn columns(&self, area: Rect) -> Vec<(Col, &'static str, Rect)> {
        let t = self.table_rect(area);
        let widths = [
            (Col::Cpu, "% CPU", 80),
            (Col::Time, "CPU Time", 100),
            (Col::Threads, "Threads", 70),
            (Col::Memory, "Memory", 90),
            (Col::Pid, "PID", 60),
        ];
        let fixed: i32 = widths.iter().map(|w| w.2).sum();
        let mut v = alloc::vec![(Col::Name, "Process", Rect::new(t.x, t.y, t.w - fixed, ROW_H))];
        let mut x = t.x + t.w - fixed;
        for (c, label, w) in widths {
            v.push((c, label, Rect::new(x, t.y, w, ROW_H)));
            x += w;
        }
        v
    }

    fn row_at(&self, area: Rect, y: i32) -> Option<usize> {
        let t = self.table_rect(area);
        let i = (y - t.y - ROW_H + self.scroll).div_euclid(ROW_H);
        (y > t.y + ROW_H && i >= 0 && (i as usize) < self.rows.len()).then_some(i as usize)
    }

    fn confirm_rects(area: Rect) -> (Rect, Rect, Rect) {
        let d = Rect::new(area.x + (area.w - 400) / 2, area.y + 90, 400, 150);
        (d, Rect::new(d.right() - 124, d.bottom() - 50, 108, 34), Rect::new(d.right() - 232, d.bottom() - 50, 96, 34))
    }

    // ------------------------------------------------------------ drawing

    fn graph(cv: &mut Canvas, r: Rect, series: &[&VecDeque<u64>], max: u64, colors: &[u32]) {
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

    fn stat(cv: &mut Canvas, x: i32, y: i32, label: &str, value: &str, color: Option<u32>) {
        let t = theme::current();
        if let Some(c) = color {
            cv.fill_circle(x + 4, y - 4, 4, c);
        }
        let lx = if color.is_some() { x + 14 } else { x };
        cv.text(lx, y, label, theme::ui(12), t.text_secondary);
        cv.text(lx, y + 20, value, theme::ui_bold(15), t.text);
    }

    fn draw_table(&self, cv: &mut Canvas, area: Rect, env: &Env) {
        let t = theme::current();
        let tr = self.table_rect(area);
        cv.fill_round_rect(tr, 10, t.window_bg_alt);
        for (c, label, r) in self.columns(area) {
            if self.hover == Some(Hover::Column(c)) {
                cv.fill_rect(r.inset(2), t.hover);
            }
            let f = if self.sort.0 == c { theme::ui_bold(12) } else { theme::ui(12) };
            let x = if c == Col::Name { r.x + 12 } else { r.right() - 12 - f.width(label) };
            cv.text(x, r.y + 18, label, f, t.text_secondary);
        }
        cv.fill_rect(Rect::new(tr.x, tr.y + ROW_H, tr.w, 1), t.separator);
        let body = Rect::new(tr.x, tr.y + ROW_H + 1, tr.w, tr.h - ROW_H - 2);
        let cols = self.columns(area);
        cv.with_clip(body, |cv| {
            for (i, row) in self.rows.iter().enumerate() {
                let y = body.y + i as i32 * ROW_H - self.scroll;
                if y > body.bottom() {
                    break;
                }
                if y + ROW_H < body.y {
                    continue;
                }
                let r = Rect::new(body.x + 4, y, body.w - 8, ROW_H);
                let id = row.info.pid | (row.info.user << 31);
                let selected = self.selected == Some(id);
                if selected {
                    cv.fill_round_rect(
                        r,
                        6,
                        if env.focused { theme::accent() } else { with_alpha(t.text_secondary, 0x40) },
                    );
                } else if i % 2 == 1 {
                    cv.fill_rect(r, with_alpha(t.text_secondary, 0x0C));
                }
                let fg = if selected && env.focused { 0xFFFF_FFFF } else { t.text };
                let fg2 = if selected && env.focused { 0xDDFF_FFFF } else { t.text_secondary };
                let info = &row.info;
                // A dot for the state, then the name (kernel tasks in secondary text).
                let dot = match info.state {
                    PROC_RUNNING => 0xFF30_D158,
                    PROC_SLEEPING => with_alpha(t.text_secondary, 0x80),
                    _ => 0xFFFF_9F0A,
                };
                cv.fill_circle(r.x + 12, y + ROW_H / 2, 3, dot);
                let name = match (info.user, info.name()) {
                    (0, n) => format!("{n} (kernel)"),
                    (_, "Activity") => String::from("Activity Monitor"),
                    (_, n) => String::from(n),
                };
                cv.text_clipped(
                    r.x + 24,
                    y + 18,
                    &name,
                    theme::ui(13),
                    if info.user == 0 { fg2 } else { fg },
                    cols[0].2.w - 30,
                );
                let values = [
                    format!("{}.{}", row.cpu_pct / 10, row.cpu_pct % 10),
                    human_time(info.cpu_ms),
                    format!("{}", info.threads),
                    if info.user == 0 { String::from("—") } else { human_bytes(info.mem_kib * 1024) },
                    // Kernel tasks have task ids, not process ids.
                    if info.user == 0 { String::from("—") } else { format!("{}", info.pid) },
                ];
                let f = theme::ui(13);
                for (k, v) in values.iter().enumerate() {
                    let cr = cols[k + 1].2;
                    cv.text(cr.right() - 12 - f.width(v), y + 18, v, f, fg);
                }
            }
        });
    }

    fn draw_tab(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        let t = theme::current();
        let s = self.stats;
        let g = Self::graph_rect(area);
        let blue = 0xFF0A_84FF;
        let green = 0xFF30_B050;
        let orange = 0xFFFF_9F0A;
        match self.tab {
            Tab::Cpu => {
                let gr = Rect::new(g.x, g.y, g.w - 200, g.h);
                if self.core_hist.len() > 1 {
                    // One small history graph per core, side by side.
                    let n = self.core_hist.len() as i32;
                    let gap = 8;
                    let w = (gr.w - gap * (n - 1)) / n;
                    for (i, core) in self.core_hist.iter().enumerate() {
                        let hist: VecDeque<u64> = core.iter().map(|&v| v as u64).collect();
                        let cr = Rect::new(gr.x + i as i32 * (w + gap), gr.y, w, gr.h);
                        Self::graph(cv, cr, &[&hist], 1000, &[theme::accent()]);
                        cv.text(cr.x + 10, cr.y + 18, &format!("CPU {i}"), theme::ui(11), t.text_secondary);
                    }
                } else {
                    let hist: VecDeque<u64> = self.cpu_hist.iter().map(|&v| v as u64).collect();
                    Self::graph(cv, gr, &[&hist], 1000, &[theme::accent()]);
                }
                let now = self.cpu_hist.back().copied().unwrap_or(0);
                let x = gr.right() + 24;
                Self::stat(cv, x, g.y + 22, "CPU load", &format!("{}.{}%", now / 10, now % 10), Some(theme::accent()));
                Self::stat(cv, x, g.y + 70, "Idle", &format!("{}.{}%", (1000 - now) / 10, (1000 - now) % 10), None);
                Self::stat(cv, x, g.y + 118, "Tasks", &format!("{} in {} processes", s.tasks, s.processes), None);
                self.draw_table(cv, area, env);
            }
            Tab::Memory => {
                let hist: VecDeque<u64> = self.mem_hist.iter().map(|&v| v as u64).collect();
                let gr = Rect::new(g.x, g.y, g.w - 200, g.h);
                Self::graph(cv, gr, &[&hist], 1000, &[green]);
                let x = gr.right() + 24;
                Self::stat(cv, x, g.y + 22, "Used", &human_bytes(s.mem_used), Some(green));
                Self::stat(cv, x, g.y + 70, "Physical memory", &human_bytes(s.mem_total), None);
                let heap = format!("{} of {}", human_bytes(s.heap_used), human_bytes(s.heap_size));
                Self::stat(cv, x, g.y + 118, "Kernel heap", &heap, None);
                self.draw_table(cv, area, env);
            }
            Tab::Disk => {
                let max =
                    self.read_hist.iter().chain(self.write_hist.iter()).copied().max().unwrap_or(0).max(64 * 1024);
                let gr = Rect::new(g.x, g.y, g.w - 200, g.h + 60);
                Self::graph(cv, gr, &[&self.read_hist, &self.write_hist], max, &[blue, orange]);
                let x = gr.right() + 24;
                let rd = self.read_hist.back().copied().unwrap_or(0);
                let wr = self.write_hist.back().copied().unwrap_or(0);
                Self::stat(cv, x, g.y + 22, "Reading", &format!("{}/s", human_bytes(rd)), Some(blue));
                Self::stat(cv, x, g.y + 70, "Writing", &format!("{}/s", human_bytes(wr)), Some(orange));
                Self::stat(cv, x, g.y + 118, "Scale", &format!("{}/s", human_bytes(max)), None);
                let info = process::sys_info();
                let y = gr.bottom() + 34;
                let rows = [
                    ("Reads", format!("{} ({})", s.disk_reads, human_bytes(s.disk_read_bytes))),
                    ("Writes", format!("{} ({})", s.disk_writes, human_bytes(s.disk_write_bytes))),
                    ("Startup disk", process::fixed_str(&info.root, info.root_len)),
                ];
                for (k, (label, value)) in rows.iter().enumerate() {
                    cv.text(g.x + 8, y + k as i32 * 26, label, theme::ui(13), t.text_secondary);
                    cv.text(g.x + 160, y + k as i32 * 26, value, theme::ui(13), t.text);
                }
                let used = info.disk_total.saturating_sub(info.disk_free);
                let bar = Rect::new(g.x + 8, y + 86, g.w - 16, 10);
                widgets::progress(cv, bar, (used * 1000 / info.disk_total.max(1)) as i32, theme::accent());
                let cap = format!("{} used of {}", human_bytes(used), human_bytes(info.disk_total));
                cv.text(g.x + 8, bar.bottom() + 22, &cap, theme::ui(12), t.text_secondary);
            }
            Tab::Network => {
                let (purple, teal) = (0xFFBF_5AF2, 0xFF30_B0C7);
                let max = self.rx_hist.iter().chain(self.tx_hist.iter()).copied().max().unwrap_or(0).max(16 * 1024);
                let gr = Rect::new(g.x, g.y, g.w - 200, g.h + 60);
                Self::graph(cv, gr, &[&self.rx_hist, &self.tx_hist], max, &[purple, teal]);
                let x = gr.right() + 24;
                let rx = self.rx_hist.back().copied().unwrap_or(0);
                let tx = self.tx_hist.back().copied().unwrap_or(0);
                Self::stat(cv, x, g.y + 22, "Receiving", &format!("{}/s", human_bytes(rx)), Some(purple));
                Self::stat(cv, x, g.y + 70, "Sending", &format!("{}/s", human_bytes(tx)), Some(teal));
                Self::stat(cv, x, g.y + 118, "Scale", &format!("{}/s", human_bytes(max)), None);
                let y = gr.bottom() + 34;
                let f = theme::ui(13);
                let cols = [(8, "Interface"), (130, "Driver"), (250, "Address"), (390, "Received"), (530, "Sent")];
                for (cx, h) in cols {
                    cv.text(g.x + cx, y, h, theme::ui_bold(12), t.text_secondary);
                }
                for (k, i) in aurora::net::interfaces().iter().enumerate() {
                    let ry = y + 28 + k as i32 * 26;
                    let ip = if i.ip == [0; 4] { String::from("—") } else { aurora::net::ip_string(i.ip) };
                    let vals = [
                        String::from(aurora::net::name_of(i)),
                        String::from(aurora::net::driver_of(i)),
                        ip,
                        format!("{} ({} pkts)", human_bytes(i.rx_bytes), i.rx_packets),
                        format!("{} ({} pkts)", human_bytes(i.tx_bytes), i.tx_packets),
                    ];
                    for ((cx, _), v) in cols.iter().zip(vals.iter()) {
                        cv.text_clipped(g.x + cx, ry, v, f, t.text, 136);
                    }
                }
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
                let card = Rect::new(g.x, g.y, g.w, rows.len() as i32 * 36 + 8);
                cv.fill_round_rect(card, 10, t.window_bg_alt);
                for (k, (label, value)) in rows.iter().enumerate() {
                    let y = card.y + 26 + k as i32 * 36;
                    cv.text(card.x + 16, y, label, theme::ui(13), t.text_secondary);
                    let f = theme::ui_bold(13);
                    cv.text(card.right() - 16 - f.width(value), y, value, f, t.text);
                    if k + 1 < rows.len() {
                        cv.fill_rect(Rect::new(card.x + 16, y + 12, card.w - 32, 1), t.separator);
                    }
                }
            }
        }
    }
}

impl App for Activity {
    fn title(&self) -> String {
        String::from("Activity Monitor")
    }

    fn size(&self) -> (i32, i32) {
        (860, 600)
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        self.area = area;
        let t = theme::current();
        let bar = Rect::new(area.x, area.y, area.w, TOOLBAR_H);
        cv.fill_rect(bar, t.titlebar);
        cv.fill_rect(Rect::new(area.x, bar.bottom() - 1, area.w, 1), t.separator);
        let labels: Vec<&str> = TABS.iter().map(|(_, l)| *l).collect();
        let sel = TABS.iter().position(|(k, _)| *k == self.tab).unwrap_or(0);
        let hover = match self.hover {
            Some(Hover::Tab(i)) => Some(i),
            _ => None,
        };
        widgets::segmented(cv, Self::tabs(area), &labels, sel, hover);
        let can_quit = self.selected.is_some_and(|id| id >> 31 == 1) && matches!(self.tab, Tab::Cpu | Tab::Memory);
        if can_quit {
            button(cv, Self::quit_button(area), "Quit", ButtonStyle::Secondary, self.hover == Some(Hover::Quit));
        }
        self.draw_tab(cv, area, env);
        if let Some((_, name)) = &self.confirm_quit {
            cv.fill_rect(area, with_alpha(0x000000, 0x30));
            let (d, ok, cancel) = Self::confirm_rects(area);
            cv.shadow(d, 12, 20, 6, t.shadow);
            cv.fill_round_rect(d, 12, t.window_bg);
            cv.stroke_round_rect(d, 12, t.window_border);
            cv.text_clipped(d.x + 20, d.y + 36, &format!("Quit “{name}”?"), theme::ui_bold(15), t.text, d.w - 40);
            cv.text(d.x + 20, d.y + 60, "Unsaved changes in it will be lost.", theme::ui(13), t.text_secondary);
            button(cv, ok, "Force Quit", ButtonStyle::Danger, self.hover == Some(Hover::ConfirmQuit));
            button(cv, cancel, "Cancel", ButtonStyle::Secondary, self.hover == Some(Hover::CancelQuit));
        }
    }

    fn click(&mut self, x: i32, y: i32, area: Rect, _env: &mut Env) -> bool {
        if let Some((pid, _)) = self.confirm_quit.clone() {
            let (_, ok, cancel) = Self::confirm_rects(area);
            if ok.contains(x, y) {
                let _ = process::kill(pid);
                self.confirm_quit = None;
                self.selected = None;
            } else if cancel.contains(x, y) {
                self.confirm_quit = None;
            }
            return true;
        }
        let tabs = widgets::segments(Self::tabs(area), TABS.len());
        if let Some(i) = tabs.iter().position(|r| r.contains(x, y)) {
            self.tab = TABS[i].0;
            self.scroll = 0;
            if self.tab == Tab::Memory && self.sort.0 == Col::Cpu {
                self.sort = (Col::Memory, false);
                self.sort_rows();
            }
            return true;
        }
        if Self::quit_button(area).contains(x, y) {
            if let Some(id) = self.selected.filter(|id| id >> 31 == 1) {
                let pid = id & 0x7FFF_FFFF;
                let name = self
                    .rows
                    .iter()
                    .find(|r| r.info.user == 1 && r.info.pid == pid)
                    .map(|r| String::from(r.info.name()));
                self.confirm_quit = Some((pid, name.unwrap_or_default()));
            }
            return true;
        }
        if matches!(self.tab, Tab::Cpu | Tab::Memory) {
            if let Some((c, _, _)) = self.columns(area).into_iter().find(|(_, _, r)| r.contains(x, y)) {
                self.sort = if self.sort.0 == c { (c, !self.sort.1) } else { (c, c == Col::Name || c == Col::Pid) };
                self.sort_rows();
                return true;
            }
            if self.table_rect(area).contains(x, y) {
                self.selected = self.row_at(area, y).map(|i| self.rows[i].info.pid | (self.rows[i].info.user << 31));
                return true;
            }
        }
        false
    }

    fn hover(&mut self, x: i32, y: i32, area: Rect) -> bool {
        let h = if x < 0 {
            None
        } else if self.confirm_quit.is_some() {
            let (_, ok, cancel) = Self::confirm_rects(area);
            if ok.contains(x, y) {
                Some(Hover::ConfirmQuit)
            } else if cancel.contains(x, y) {
                Some(Hover::CancelQuit)
            } else {
                None
            }
        } else if let Some(i) = widgets::segments(Self::tabs(area), TABS.len()).iter().position(|r| r.contains(x, y)) {
            Some(Hover::Tab(i))
        } else if Self::quit_button(area).contains(x, y) {
            Some(Hover::Quit)
        } else if matches!(self.tab, Tab::Cpu | Tab::Memory) {
            self.columns(area).into_iter().find(|(_, _, r)| r.contains(x, y)).map(|(c, _, _)| Hover::Column(c))
        } else {
            None
        };
        core::mem::replace(&mut self.hover, h) != h
    }

    fn scroll(&mut self, delta: i32, area: Rect) -> bool {
        let t = self.table_rect(area);
        let max = (self.rows.len() as i32 * ROW_H - (t.h - ROW_H - 2)).max(0);
        let s = (self.scroll + delta * ROW_H * 2).clamp(0, max);
        core::mem::replace(&mut self.scroll, s) != s
    }

    fn key(&mut self, ev: &KeyEvent, _env: &mut Env) -> bool {
        if !ev.pressed {
            return false;
        }
        match ev.code {
            KeyCode::Escape if self.confirm_quit.is_some() => self.confirm_quit = None,
            KeyCode::Down | KeyCode::Up if !self.rows.is_empty() => {
                let cur =
                    self.selected.and_then(|id| self.rows.iter().position(|r| r.info.pid | (r.info.user << 31) == id));
                let next = match (cur, ev.code) {
                    (None, _) => 0,
                    (Some(i), KeyCode::Down) => (i + 1).min(self.rows.len() - 1),
                    (Some(i), _) => i.saturating_sub(1),
                };
                self.selected = Some(self.rows[next].info.pid | (self.rows[next].info.user << 31));
            }
            _ => return false,
        }
        true
    }

    fn tick(&mut self, env: &mut Env) -> bool {
        if env.now_ms >= self.last_sample + 1000 {
            self.sample();
            return true;
        }
        false
    }

    fn tick_interval(&self) -> u64 {
        250
    }
}

aurora::entry!(main);

fn main(_: aurora::Args) -> i32 {
    ripple::run(Activity::new())
}
