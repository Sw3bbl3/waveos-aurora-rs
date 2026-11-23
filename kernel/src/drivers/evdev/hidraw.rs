//! Raw HID devices (`/dev/hidraw*`): a HID device's report descriptor and
//! its reports as they come, for programs that speak a controller's own
//! protocol (SDL's and Steam's HIDAPI drivers do, for PlayStation and
//! Nintendo controllers). The USB HID driver registers each interface it
//! drives and hands every input report to [`Hidraw::input`]; output and
//! feature reports go back through the device's [`HidIo`].

use super::{Class, Location};
use crate::sync::IrqMutex;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use aurora_abi::err::*;
use core::sync::atomic::{AtomicBool, Ordering};

/// Reports a reader may have waiting (Linux's `HIDRAW_BUFFER_SIZE`).
const QUEUE: usize = 64;

/// Report kinds (HID 1.11, 7.2.1).
pub const INPUT: u8 = 1;
pub const OUTPUT: u8 = 2;
pub const FEATURE: u8 = 3;

/// How reports reach the device.
pub trait HidIo: Send + Sync {
    /// An output report (first byte: the report id, 0 if it has none).
    fn output(&self, report: &[u8]) -> Result<usize, isize>;
    /// GET_REPORT of `kind`: `buf[0]` holds the report id; fills `buf`.
    fn get_report(&self, kind: u8, buf: &mut [u8]) -> Result<usize, isize>;
    /// SET_REPORT of `kind` (first byte: the report id).
    fn set_report(&self, kind: u8, report: &[u8]) -> Result<usize, isize>;
}

pub struct Hidraw {
    pub num: u32,
    pub name: String,
    pub phys: String,
    pub uniq: String,
    pub bustype: u16,
    pub vendor: u16,
    pub product: u16,
    pub descriptor: Vec<u8>,
    pub location: Location,
    /// What it is: game controllers' raw reports are open to the logged-in
    /// account; keyboards' and pointers' only to the system.
    pub class: Class,
    pub io: Arc<dyn HidIo>,
    st: IrqMutex<State>,
}

struct State {
    clients: Vec<Weak<Client>>,
    gone: bool,
}

pub struct Client {
    pub dev: Arc<Hidraw>,
    pub pid: u32,
    q: IrqMutex<VecDeque<Vec<u8>>>,
    pub gone: AtomicBool,
}

impl Client {
    pub fn key(&self) -> usize {
        crate::proc::readiness::key(self)
    }

    pub fn ready(&self) -> bool {
        !self.q.lock().is_empty()
    }

    pub fn take(&self) -> Option<Vec<u8>> {
        self.q.lock().pop_front()
    }
}

static DEVICES: IrqMutex<Vec<Arc<Hidraw>>> = IrqMutex::new(Vec::new());

/// Everything about a new raw device but its number.
pub struct Spec {
    pub name: String,
    pub phys: String,
    pub uniq: String,
    pub bustype: u16,
    pub vendor: u16,
    pub product: u16,
    pub descriptor: Vec<u8>,
    pub location: Location,
    pub class: Class,
    pub io: Arc<dyn HidIo>,
}

pub fn add(s: Spec) -> Arc<Hidraw> {
    let d = {
        let mut all = DEVICES.lock();
        let num = super::lowest_free(all.iter().map(|d| d.num));
        let d = Arc::new(Hidraw {
            num,
            name: s.name,
            phys: s.phys,
            uniq: s.uniq,
            bustype: s.bustype,
            vendor: s.vendor,
            product: s.product,
            descriptor: s.descriptor,
            location: s.location,
            class: s.class,
            io: s.io,
            st: IrqMutex::new(State { clients: Vec::new(), gone: false }),
        });
        all.push(d.clone());
        d
    };
    crate::compat::linux::input::hidraw_changed(&d, true);
    d
}

pub fn remove(d: &Arc<Hidraw>) {
    DEVICES.lock().retain(|x| !Arc::ptr_eq(x, d));
    let keys: Vec<usize> = {
        let mut st = d.st.lock();
        st.gone = true;
        st.clients
            .iter()
            .filter_map(|c| c.upgrade())
            .map(|c| {
                c.gone.store(true, Ordering::Release);
                c.key()
            })
            .collect()
    };
    for k in keys {
        crate::proc::readiness::notify(k);
    }
    crate::compat::linux::input::hidraw_changed(d, false);
}

pub fn devices() -> Vec<Arc<Hidraw>> {
    DEVICES.lock().clone()
}

pub fn by_num(num: u32) -> Option<Arc<Hidraw>> {
    DEVICES.lock().iter().find(|d| d.num == num).cloned()
}

impl Hidraw {
    /// Whether process `pid` may read its reports now.
    pub fn may_read(&self, pid: u32) -> bool {
        pid == 0
            || match self.class {
                Class::Joystick => crate::drivers::gamepad::may_read(pid),
                _ => super::has_focus(pid),
            }
    }

    pub fn open(self: &Arc<Self>, pid: u32) -> Result<Arc<Client>, isize> {
        let mut st = self.st.lock();
        if st.gone {
            return Err(ENODEV);
        }
        let c = Arc::new(Client {
            dev: self.clone(),
            pid,
            q: IrqMutex::new(VecDeque::with_capacity(QUEUE)),
            gone: AtomicBool::new(false),
        });
        st.clients.retain(|w| w.strong_count() > 0);
        st.clients.push(Arc::downgrade(&c));
        Ok(c)
    }

    /// An input report from the device (with its id first, if numbered).
    pub fn input(&self, report: &[u8]) {
        let clients: Vec<Arc<Client>> = self.st.lock().clients.iter().filter_map(|c| c.upgrade()).collect();
        for c in clients {
            if !self.may_read(c.pid) {
                continue;
            }
            {
                let mut q = c.q.lock();
                if q.len() == QUEUE {
                    q.pop_front();
                }
                q.push_back(report.to_vec());
            }
            crate::proc::readiness::notify(c.key());
        }
    }
}
