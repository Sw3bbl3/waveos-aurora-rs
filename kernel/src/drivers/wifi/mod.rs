//! Wi-Fi: the radios (`Radio`), the station that joins networks
//! (`aurora_wifi::station`, shared and tested on the host) and the network
//! interface (wlanN) the TCP/IP stack sees.
//!
//! A radio driver hands over every 802.11 frame it receives (`received`)
//! and transmits what the station gives it. Everything above the radio —
//! scanning results, authentication, association, the WPA2 handshake and
//! CCMP encryption — happens here, in software, so drivers stay small.

pub mod iwlwifi;
pub mod sim;

use crate::sync::{IrqMutex, WaitQueue};
use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_wifi::station::{Output, State, Station};
use aurora_wifi::{Bss, Security};
use core::sync::atomic::{AtomicBool, Ordering};

/// What the station needs from the hardware.
pub trait Radio: Send + Sync {
    /// E.g. "Intel Wi-Fi 6 AX200".
    fn name(&self) -> String;
    fn mac(&self) -> [u8; 6];
    /// Why the radio can't be used yet (firmware missing, ...), or `None` when ready.
    fn unavailable(&self) -> Option<String>;
    /// Starts a scan of every channel; beacons and probe responses come back
    /// through [`received`]. The station's probe request is offered for
    /// radios that send their own.
    fn scan(&self, probe: &[u8]);
    /// Tunes to the network and prepares to exchange frames with it.
    fn join(&self, bss: &Bss) -> bool;
    fn leave(&self);
    /// Transmits one 802.11 frame (management or data, already encrypted).
    fn transmit(&self, frame: &[u8]) -> bool;
}

struct Wifi {
    radio: Arc<dyn Radio>,
    station: Station,
    /// The network stack's interface for this radio.
    iface: usize,
    /// When the last scan started (`None`: never).
    last_scan: Option<u64>,
}

static WIFI: IrqMutex<Option<Wifi>> = IrqMutex::new(None);
static RX: IrqMutex<VecDeque<Vec<u8>>> = IrqMutex::new(VecDeque::new());
static WAKE: WaitQueue = WaitQueue::new();
static KICK: AtomicBool = AtomicBool::new(false);

const SCAN_EVERY_MS: u64 = 30_000;
const TICK_MS: u64 = 200;

pub fn init() {
    for d in crate::drivers::pci::devices() {
        if let Some(radio) = iwlwifi::probe(&d) {
            register(radio);
            return; // one Wi-Fi adapter is plenty
        }
    }
    // Simulated networks for virtual machines (boot option wifi_sim=1).
    if crate::gui::prefs::boot_option("wifi_sim").as_deref() == Some("1") {
        sim::start();
    }
}

/// Whether a Wi-Fi adapter (real or simulated) is registered.
#[cfg_attr(not(feature = "ktest"), allow(dead_code))] // the self-tests start the simulator if not
pub fn present() -> bool {
    WIFI.lock().is_some()
}

/// The network stack's view of the radio.
struct Interface(Arc<dyn Radio>);

impl crate::net::Nic for Interface {
    fn driver(&self) -> String {
        self.0.name()
    }
    fn mac(&self) -> [u8; 6] {
        self.0.mac()
    }
    fn link_up(&self) -> bool {
        WIFI.lock().as_ref().is_some_and(|w| *w.station.state() == State::Connected)
    }
    fn send(&self, frame: &[u8]) -> bool {
        let tx = WIFI.lock().as_mut().and_then(|w| w.station.send(frame));
        tx.is_some_and(|f| self.0.transmit(&f))
    }
    fn wireless(&self) -> bool {
        true
    }
}

/// Makes a radio the system's Wi-Fi adapter.
pub fn register(radio: Arc<dyn Radio>) {
    let iface = crate::net::register(Arc::new(Interface(radio.clone())));
    log!("wifi", "{} ({})", radio.name(), radio.unavailable().unwrap_or_else(|| "ready".to_string()));
    *WIFI.lock() = Some(Wifi { station: Station::new(radio.mac()), radio, iface, last_scan: None });
    crate::sched::spawn("wifi", run);
}

/// A frame the radio received (any context, interrupts included).
pub fn received(frame: Vec<u8>) {
    let mut q = RX.lock();
    if q.len() < 256 {
        q.push_back(frame);
    }
    drop(q);
    KICK.store(true, Ordering::Release);
    WAKE.wake_all();
}

/// Carries out what the station asked for (without holding the lock).
fn perform(radio: &Arc<dyn Radio>, iface: usize, out: Vec<Output>) {
    for o in out {
        match o {
            Output::Transmit(f) => {
                radio.transmit(&f);
            }
            Output::Receive(eth) => crate::net::receive(iface, eth),
            Output::Link(up) => crate::net::link_changed(iface, up),
        }
    }
}

/// The "wifi" task: received frames, timeouts and periodic scans.
fn run() {
    loop {
        KICK.store(false, Ordering::Release);
        let now = crate::time::uptime_ms();
        loop {
            let Some(frame) = RX.lock().pop_front() else { break };
            let (radio, iface, out) = {
                let mut g = WIFI.lock();
                let Some(w) = g.as_mut() else { break };
                (w.radio.clone(), w.iface, w.station.receive(&frame, now))
            };
            perform(&radio, iface, out);
        }
        let (radio, iface, out, scan) = {
            let mut g = WIFI.lock();
            let Some(w) = g.as_mut() else { return };
            let out = w.station.tick(now);
            // Scan now and then while not connected (the list stays fresh).
            let idle = matches!(w.station.state(), State::Idle | State::Failed(_));
            let scan = (idle && w.last_scan.is_none_or(|t| now >= t + SCAN_EVERY_MS)).then(|| {
                w.last_scan = Some(now);
                w.station.probe()
            });
            (w.radio.clone(), w.iface, out, scan)
        };
        perform(&radio, iface, out);
        if let Some(probe) = scan {
            if radio.unavailable().is_none() {
                radio.scan(&probe);
            }
        }
        WAKE.wait(TICK_MS, || KICK.load(Ordering::Acquire));
    }
}

// --------------------------------------------------------------- syscalls

fn security_code(s: Security) -> u32 {
    use aurora_abi::wifi::*;
    match s {
        Security::Open => OPEN,
        Security::Wpa2Psk => WPA2,
        Security::Wpa2Wpa3 => WPA2_WPA3,
        _ => UNSUPPORTED,
    }
}

fn copy_into(dst: &mut [u8], src: &[u8]) -> u32 {
    let n = src.len().min(dst.len());
    dst[..n].copy_from_slice(&src[..n]);
    n as u32
}

pub fn status() -> Option<aurora_abi::WifiStatus> {
    use aurora_abi::wifi::*;
    let g = WIFI.lock();
    let w = g.as_ref()?;
    let mut s = aurora_abi::WifiStatus::default();
    s.driver_len = copy_into(&mut s.driver, w.radio.name().as_bytes());
    if let Some(bss) = w.station.network() {
        s.ssid_len = copy_into(&mut s.ssid, &bss.ssid_raw);
    }
    let (state, message) = match (w.radio.unavailable(), w.station.state()) {
        (Some(why), _) => (UNAVAILABLE, why),
        (None, State::Idle) => (IDLE, String::new()),
        (None, State::Connected) => (CONNECTED, String::new()),
        (None, State::Failed(why)) => (FAILED, why.clone()),
        (None, State::Handshake) => (CONNECTING, String::from("Checking the password…")),
        (None, _) => (CONNECTING, String::from("Joining…")),
    };
    s.state = state;
    s.message_len = copy_into(&mut s.message, message.as_bytes());
    Some(s)
}

/// Networks heard recently; also asks for a fresh scan.
pub fn networks() -> Vec<aurora_abi::WifiNetwork> {
    let now = crate::time::uptime_ms();
    let (list, radio, probe) = {
        let mut g = WIFI.lock();
        let Some(w) = g.as_mut() else { return Vec::new() };
        let probe = w.last_scan.is_none_or(|t| now >= t + 5000).then(|| {
            w.last_scan = Some(now);
            w.station.probe()
        });
        (w.station.networks(now), w.radio.clone(), probe)
    };
    if let Some(p) = probe {
        if radio.unavailable().is_none() {
            radio.scan(&p);
        }
    }
    list.iter()
        .map(|b| {
            let mut n = aurora_abi::WifiNetwork::default();
            n.ssid_len = copy_into(&mut n.ssid, &b.ssid_raw);
            n.security = security_code(b.security);
            n.channel = b.channel as u32;
            n
        })
        .collect()
}

/// Starts joining the network named `ssid`.
pub fn connect(ssid: &[u8], password: &str) -> Result<(), isize> {
    use aurora_abi::err::*;
    let now = crate::time::uptime_ms();
    let mut snonce = [0u8; 32];
    crate::random::fill(&mut snonce);
    let (radio, iface, out) = {
        let mut g = WIFI.lock();
        let w = g.as_mut().ok_or(ENODEV)?;
        if w.radio.unavailable().is_some() {
            return Err(ENODEV);
        }
        let bss = w.station.networks(now).into_iter().find(|b| b.ssid_raw == ssid).ok_or(ENOENT)?;
        if !w.radio.join(&bss) {
            return Err(EIO);
        }
        let pass = (!password.is_empty()).then_some(password);
        (w.radio.clone(), w.iface, w.station.connect(bss, pass, snonce, now))
    };
    perform(&radio, iface, out);
    KICK.store(true, Ordering::Release);
    WAKE.wake_all();
    Ok(())
}

pub fn disconnect() {
    let (radio, iface, out) = {
        let mut g = WIFI.lock();
        let Some(w) = g.as_mut() else { return };
        (w.radio.clone(), w.iface, w.station.disconnect())
    };
    perform(&radio, iface, out);
    radio.leave();
}

/// "Wi-Fi · Network" for the menu bar, if connected.
pub fn connected_network() -> Option<String> {
    let g = WIFI.lock();
    let w = g.as_ref()?;
    (*w.station.state() == State::Connected).then(|| w.station.network().map(|b| b.ssid.clone()).unwrap_or_default())
}
