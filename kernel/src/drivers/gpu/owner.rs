//! Who drives a display device: the desktop, or a program that took it.
//!
//! Lumen Server shows the desktop through [`super::output::Output`], whose
//! commits and presents come here ([`desktop_commit`], [`desktop_dirty`]).
//! A program that drives the display itself — a Linux DRM master doing a
//! mode set, a page flip or an atomic commit — takes the device with its
//! first commit ([`client_commit`]): the desktop's state is kept aside,
//! and while the program holds the device the desktop's commits change
//! only that kept state and its presents reach nothing (the compositor
//! goes on drawing into its back buffer). When the program lets go
//! ([`give_back`]: it dropped master, closed the device, or died and its
//! descriptors were closed) the kept state is committed again and the
//! desktop's picture shown in full. Merely opening the device or reading
//! its state takes nothing.

use super::*;
use crate::sync::Mutex;

struct Taken {
    dev: usize,
    /// Who holds it (the DRM layer's file id).
    client: u64,
    /// The desktop's state, as its commits since left it.
    desktop: (Vec<CrtcState>, Vec<PlaneState>),
}

/// One entry per device a program holds. Commits of either side are made
/// with this lock held, so a takeover never interleaves with a present.
static TAKEN: Mutex<Vec<Taken>> = Mutex::new(Vec::new());

fn key(dev: &dyn KmsDevice) -> usize {
    dev as *const dyn KmsDevice as *const () as usize
}

/// The program holding `dev`, if one does.
pub fn holder(dev: &dyn KmsDevice) -> Option<u64> {
    TAKEN.lock().iter().find(|t| t.dev == key(dev)).map(|t| t.client)
}

/// The framebuffers the desktop showed on `dev` when a program took it.
/// (Programs put back the picture they found when they end, as on Linux,
/// by the framebuffer id they read then.)
pub fn kept_framebuffers(dev: &dyn KmsDevice) -> Vec<Arc<Framebuffer>> {
    let taken = TAKEN.lock();
    let Some(t) = taken.iter().find(|t| t.dev == key(dev)) else { return Vec::new() };
    t.desktop.1.iter().filter_map(|p| p.fb.clone()).collect()
}

/// A commit of the desktop's: applied, or kept for later while a program
/// holds the device (checked against the kept state all the same).
pub fn desktop_commit(dev: &dyn KmsDevice, c: &Commit) -> Result<()> {
    let mut taken = TAKEN.lock();
    match taken.iter_mut().find(|t| t.dev == key(dev)) {
        None => dev.atomic_commit(c),
        Some(t) => {
            let (cs, ps, _) = validate(dev, t.desktop.clone(), c)?;
            if c.flags & commit::TEST_ONLY == 0 {
                t.desktop = (cs, ps);
            }
            Ok(())
        }
    }
}

/// A present of the desktop's: shown unless a program holds the device.
pub fn desktop_dirty(dev: &dyn KmsDevice, fb: &Arc<Framebuffer>, rects: &[Damage]) -> Result<()> {
    let taken = TAKEN.lock();
    if taken.iter().any(|t| t.dev == key(dev)) {
        return Ok(());
    }
    dev.dirty(fb, rects)
}

/// A commit of program `client`'s. Anything but a test takes the device
/// from the desktop (or from the program that held it before: the desktop's
/// kept state stays as it was).
pub fn client_commit(dev: &dyn KmsDevice, client: u64, c: &Commit) -> Result<()> {
    let mut taken = TAKEN.lock();
    if c.flags & commit::TEST_ONLY != 0 {
        return dev.atomic_check(c);
    }
    match taken.iter_mut().find(|t| t.dev == key(dev)) {
        Some(t) => {
            t.client = client;
            dev.atomic_commit(c)
        }
        None => {
            let desktop = dev.state();
            // The desktop's pointer and overlays leave with it.
            let planes = dev.planes();
            let mut c = c.clone();
            for p in &desktop.1 {
                let kind = planes.iter().find(|i| i.id == p.plane).map(|i| i.kind);
                if p.fb.is_some() && kind != Some(PlaneKind::Primary) && !c.planes.iter().any(|q| q.plane == p.plane) {
                    c.planes.push(PlaneState::disabled(p.plane));
                }
            }
            dev.atomic_commit(&c)?;
            log!("gpu", "{}: a program took the display", dev.name());
            taken.push(Taken { dev: key(dev), client, desktop });
            Ok(())
        }
    }
}

/// Program `client` lets go of `dev` (if it held it): the desktop's state
/// is committed again and its picture shown.
pub fn give_back(dev: &dyn KmsDevice, client: u64) {
    let mut taken = TAKEN.lock();
    let Some(i) = taken.iter().position(|t| t.dev == key(dev) && t.client == client) else { return };
    let (crtcs, planes) = taken.remove(i).desktop;
    let c = Commit { crtcs, planes, flags: commit::ALLOW_MODESET, user_data: 0 };
    if let Err(e) = dev.atomic_commit(&c) {
        // Whatever the program left may stand in the way: everything off, then again.
        log!("gpu", "{}: restoring the desktop failed ({:?}); trying from a blank state", dev.name(), e);
        let (cs, ps) = dev.state();
        let off = Commit {
            crtcs: cs
                .iter()
                .map(|s| CrtcState { crtc: s.crtc, active: false, mode: None, connectors: Vec::new() })
                .collect(),
            planes: ps.iter().map(|p| PlaneState::disabled(p.plane)).collect(),
            flags: commit::ALLOW_MODESET,
            user_data: 0,
        };
        let _ = dev.atomic_commit(&off);
        if let Err(e) = dev.atomic_commit(&c) {
            log!("gpu", "{}: could not give the display back to the desktop: {:?}", dev.name(), e);
            return;
        }
    }
    // The desktop drew while it was hidden.
    for p in c.planes.iter().filter(|p| p.crtc.is_some()) {
        if let Some(fb) = &p.fb {
            let _ = dev.dirty(fb, &[Damage { x: 0, y: 0, w: fb.width, h: fb.height }]);
        }
    }
    log!("gpu", "{}: the desktop has the display again", dev.name());
}
