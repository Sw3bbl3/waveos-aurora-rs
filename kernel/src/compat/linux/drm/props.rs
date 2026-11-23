//! KMS properties, property blobs and the atomic ioctl.
//!
//! The properties are derived from the core's objects and state, with
//! Linux's names, types and flags: a plane's `type`, `FB_ID`, `CRTC_ID`,
//! `SRC_*`, `CRTC_*`, `IN_FORMATS` (its formats, each with the linear
//! modifier) and `HOTSPOT_*` on cursor planes for clients that ask;
//! a CRTC's `ACTIVE` and `MODE_ID`; a connector's `EDID`, `DPMS`,
//! `CRTC_ID`, `link-status`, `non-desktop`. Atomic-only ones are shown to
//! atomic clients only. Writes become a core `Commit` built on the current
//! state; the core checks it.

use super::{get_array, kms, put_array, Args, File};
use crate::drivers::gpu::{self, ConnectorInfo, CrtcInfo, CrtcState, Mode, ObjId, PlaneInfo, PlaneKind, PlaneState};
use crate::sync::Mutex;
use crate::syscall::user;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::err::*;

pub const OBJECT_CRTC: u32 = 0xcccc_cccc;
pub const OBJECT_CONNECTOR: u32 = 0xc0c0_c0c0;
pub const OBJECT_ENCODER: u32 = 0xe0e0_e0e0;
pub const OBJECT_PROPERTY: u32 = 0xb0b0_b0b0;
pub const OBJECT_FB: u32 = 0xfbfb_fbfb;
pub const OBJECT_BLOB: u32 = 0xbbbb_bbbb;
pub const OBJECT_PLANE: u32 = 0xeeee_eeee;

const PROP_RANGE: u32 = 1 << 1;
const PROP_IMMUTABLE: u32 = 1 << 2;
const PROP_ENUM: u32 = 1 << 3;
const PROP_BLOB: u32 = 1 << 4;
const PROP_OBJECT: u32 = 1 << 6;
const PROP_SIGNED_RANGE: u32 = 2 << 6;
const PROP_ATOMIC: u32 = 0x8000_0000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum P {
    Type,
    FbId,
    CrtcId,
    SrcX,
    SrcY,
    SrcW,
    SrcH,
    CrtcX,
    CrtcY,
    CrtcW,
    CrtcH,
    HotspotX,
    HotspotY,
    InFormats,
    Active,
    ModeId,
    Edid,
    Dpms,
    LinkStatus,
    NonDesktop,
}

enum Kind {
    Range(u64, u64),
    Signed(i64, i64),
    Enum(&'static [(u64, &'static str)]),
    Blob,
    Object(u32),
}

struct Def {
    p: P,
    name: &'static str,
    kind: Kind,
    flags: u32,
}

const PLANE_TYPES: &[(u64, &str)] = &[(0, "Overlay"), (1, "Primary"), (2, "Cursor")];
const DPMS: &[(u64, &str)] = &[(0, "On"), (1, "Standby"), (2, "Suspend"), (3, "Off")];
const LINK: &[(u64, &str)] = &[(0, "Good"), (1, "Bad")];

const DEFS: &[Def] = &[
    Def { p: P::Type, name: "type", kind: Kind::Enum(PLANE_TYPES), flags: PROP_IMMUTABLE },
    Def { p: P::FbId, name: "FB_ID", kind: Kind::Object(OBJECT_FB), flags: PROP_ATOMIC },
    Def { p: P::CrtcId, name: "CRTC_ID", kind: Kind::Object(OBJECT_CRTC), flags: PROP_ATOMIC },
    Def { p: P::SrcX, name: "SRC_X", kind: Kind::Range(0, u32::MAX as u64), flags: PROP_ATOMIC },
    Def { p: P::SrcY, name: "SRC_Y", kind: Kind::Range(0, u32::MAX as u64), flags: PROP_ATOMIC },
    Def { p: P::SrcW, name: "SRC_W", kind: Kind::Range(0, u32::MAX as u64), flags: PROP_ATOMIC },
    Def { p: P::SrcH, name: "SRC_H", kind: Kind::Range(0, u32::MAX as u64), flags: PROP_ATOMIC },
    Def { p: P::CrtcX, name: "CRTC_X", kind: Kind::Signed(i32::MIN as i64, i32::MAX as i64), flags: PROP_ATOMIC },
    Def { p: P::CrtcY, name: "CRTC_Y", kind: Kind::Signed(i32::MIN as i64, i32::MAX as i64), flags: PROP_ATOMIC },
    Def { p: P::CrtcW, name: "CRTC_W", kind: Kind::Range(0, i32::MAX as u64), flags: PROP_ATOMIC },
    Def { p: P::CrtcH, name: "CRTC_H", kind: Kind::Range(0, i32::MAX as u64), flags: PROP_ATOMIC },
    Def { p: P::HotspotX, name: "HOTSPOT_X", kind: Kind::Signed(i32::MIN as i64, i32::MAX as i64), flags: PROP_ATOMIC },
    Def { p: P::HotspotY, name: "HOTSPOT_Y", kind: Kind::Signed(i32::MIN as i64, i32::MAX as i64), flags: PROP_ATOMIC },
    Def { p: P::InFormats, name: "IN_FORMATS", kind: Kind::Blob, flags: PROP_IMMUTABLE },
    Def { p: P::Active, name: "ACTIVE", kind: Kind::Range(0, 1), flags: PROP_ATOMIC },
    Def { p: P::ModeId, name: "MODE_ID", kind: Kind::Blob, flags: PROP_ATOMIC },
    Def { p: P::Edid, name: "EDID", kind: Kind::Blob, flags: PROP_IMMUTABLE },
    Def { p: P::Dpms, name: "DPMS", kind: Kind::Enum(DPMS), flags: 0 },
    Def { p: P::LinkStatus, name: "link-status", kind: Kind::Enum(LINK), flags: 0 },
    Def { p: P::NonDesktop, name: "non-desktop", kind: Kind::Range(0, 1), flags: PROP_IMMUTABLE },
];

/// The properties' ids: object ids like any other, given out once.
static IDS: spin::Once<Vec<u32>> = spin::Once::new();

fn ids() -> &'static [u32] {
    IDS.call_once(|| DEFS.iter().map(|_| gpu::new_id()).collect())
}

pub fn prop_id(p: P) -> u32 {
    ids()[DEFS.iter().position(|d| d.p == p).unwrap()]
}

fn def(id: u32) -> Option<&'static Def> {
    ids().iter().position(|&i| i == id).map(|i| &DEFS[i])
}

pub fn is_property(id: u32) -> bool {
    def(id).is_some()
}

// ------------------------------------------------------------ blobs

struct Blob {
    data: Arc<Vec<u8>>,
    /// The file that made it (0: the kernel's).
    owner: u64,
}

static BLOBS: Mutex<BTreeMap<u32, Blob>> = Mutex::new(BTreeMap::new());
/// Kernel blobs kept at most (modes and EDIDs; old ones go first).
const KERNEL_BLOBS: usize = 64;

/// The kernel's blob holding `data` (one per distinct content).
fn kernel_blob(data: Vec<u8>) -> u32 {
    let mut blobs = BLOBS.lock();
    if let Some((&id, _)) = blobs.iter().find(|(_, b)| b.owner == 0 && *b.data == data) {
        return id;
    }
    let ours: Vec<u32> = blobs.iter().filter(|(_, b)| b.owner == 0).map(|(&id, _)| id).collect();
    if ours.len() >= KERNEL_BLOBS {
        blobs.remove(&ours[0]);
    }
    let id = gpu::new_id();
    blobs.insert(id, Blob { data: Arc::new(data), owner: 0 });
    id
}

pub fn blob(id: u32) -> Option<Arc<Vec<u8>>> {
    BLOBS.lock().get(&id).map(|b| b.data.clone())
}

pub fn is_blob(id: u32) -> bool {
    BLOBS.lock().contains_key(&id)
}

/// A file's blobs go with it.
pub fn forget_file(file: u64) {
    BLOBS.lock().retain(|_, b| b.owner != file);
}

/// `struct drm_mode_get_blob`: blob_id, length, data. Copied only when the
/// length given is the blob's.
pub fn get_blob(_f: &File, a: &mut Args) -> Result<(), isize> {
    let data = blob(a.u32(0)).ok_or(ENOENT)?;
    if a.u32(4) as usize == data.len() && a.u64(8) != 0 {
        user::put_bytes(a.u64(8), &data)?;
    }
    a.set32(4, data.len() as u32);
    Ok(())
}

/// `struct drm_mode_create_blob`: data, length, blob_id.
pub fn create_blob(f: &File, a: &mut Args) -> Result<(), isize> {
    let len = a.u32(8) as u64;
    if len == 0 {
        return Err(EINVAL);
    }
    if len > 1 << 20 {
        return Err(ENOMEM);
    }
    let data = user::slice(a.u64(0), len)?;
    let id = gpu::new_id();
    BLOBS.lock().insert(id, Blob { data: Arc::new(data), owner: f.id });
    a.set32(12, id);
    Ok(())
}

pub fn destroy_blob(f: &File, id: u32) -> Result<(), isize> {
    let mut blobs = BLOBS.lock();
    match blobs.get(&id) {
        None => Err(ENOENT),
        Some(b) if b.owner != f.id => Err(EPERM),
        Some(_) => {
            blobs.remove(&id);
            Ok(())
        }
    }
}

// ------------------------------------------------------------ modes

/// `struct drm_mode_modeinfo` (68 bytes) for `m`.
pub fn modeinfo(m: &Mode) -> [u8; 68] {
    let mut b = [0u8; 68];
    let u16s =
        [m.hdisplay, m.hsync_start, m.hsync_end, m.htotal, 0, m.vdisplay, m.vsync_start, m.vsync_end, m.vtotal, 0];
    b[0..4].copy_from_slice(&m.clock.to_le_bytes());
    for (i, v) in u16s.iter().enumerate() {
        b[4 + 2 * i..6 + 2 * i].copy_from_slice(&(*v as u16).to_le_bytes());
    }
    b[24..28].copy_from_slice(&m.vrefresh.to_le_bytes());
    b[28..32].copy_from_slice(&m.flags.to_le_bytes());
    b[32..36].copy_from_slice(&m.kind.to_le_bytes());
    let name = m.name();
    let n = name.len().min(31);
    b[36..36 + n].copy_from_slice(&name.as_bytes()[..n]);
    b
}

/// A mode from a `struct drm_mode_modeinfo`, checked as Linux does.
pub fn mode_from(b: &[u8]) -> Result<Mode, isize> {
    if b.len() < 68 {
        return Err(EINVAL);
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[4 + 2 * i], b[5 + 2 * i]]) as u32;
    let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let mut m = Mode {
        clock: u32_at(0),
        hdisplay: u16_at(0),
        hsync_start: u16_at(1),
        hsync_end: u16_at(2),
        htotal: u16_at(3),
        vdisplay: u16_at(5),
        vsync_start: u16_at(6),
        vsync_end: u16_at(7),
        vtotal: u16_at(8),
        vrefresh: u32_at(24),
        flags: u32_at(28),
        kind: u32_at(32),
    };
    let ordered = |a: u32, b: u32, c: u32, d: u32| a > 0 && a <= b && b <= c && c <= d;
    if m.clock == 0
        || !ordered(m.hdisplay, m.hsync_start, m.hsync_end, m.htotal)
        || !ordered(m.vdisplay, m.vsync_start, m.vsync_end, m.vtotal)
    {
        return Err(EINVAL);
    }
    if m.vrefresh == 0 {
        let pixels = m.htotal as u64 * m.vtotal as u64;
        m.vrefresh = ((m.clock as u64 * 1000 + pixels / 2) / pixels) as u32;
    }
    Ok(m)
}

/// `struct drm_format_modifier_blob` for a plane taking `formats`, all
/// with the one modifier there is, `DRM_FORMAT_MOD_LINEAR` (0): a header
/// (version 1, flags, count and offset of the formats, count and offset of
/// the modifiers), the fourccs, then 8-aligned `struct drm_format_modifier`s
/// (a bit mask of the formats it applies to, from format `offset`, and the
/// modifier).
pub fn in_formats(formats: &[u32]) -> Vec<u8> {
    let formats = &formats[..formats.len().min(64)];
    let fmt_off = 24usize;
    let mod_off = (fmt_off + 4 * formats.len()).next_multiple_of(8);
    let mut b = alloc::vec![0u8; mod_off + 24];
    let header = [1u32, 0, formats.len() as u32, fmt_off as u32, 1, mod_off as u32];
    for (i, v) in header.iter().chain(formats).enumerate() {
        b[4 * i..4 * i + 4].copy_from_slice(&v.to_le_bytes());
    }
    let mask = if formats.len() == 64 { u64::MAX } else { (1u64 << formats.len()) - 1 };
    b[mod_off..mod_off + 8].copy_from_slice(&mask.to_le_bytes());
    // offset 0, pad, modifier LINEAR (0): already zero.
    b
}

/// The blob id of `m` as a `MODE_ID` value.
pub fn mode_blob(m: &Mode) -> u32 {
    kernel_blob(modeinfo(m).to_vec())
}

// ------------------------------------------------------------ objects

/// A KMS object with properties, as found by id.
pub enum Obj {
    Crtc(CrtcInfo),
    Plane(PlaneInfo),
    Connector(ConnectorInfo),
    /// Exists but has no properties (encoders, framebuffers, blobs, properties).
    Other(u32),
}

impl Obj {
    fn kind(&self) -> u32 {
        match self {
            Obj::Crtc(_) => OBJECT_CRTC,
            Obj::Plane(_) => OBJECT_PLANE,
            Obj::Connector(_) => OBJECT_CONNECTOR,
            Obj::Other(k) => *k,
        }
    }
}

pub fn find(f: &File, id: u32) -> Option<Obj> {
    let dev = &*f.dev;
    if let Some(c) = dev.crtcs().into_iter().find(|c| c.id == id) {
        return Some(Obj::Crtc(c));
    }
    if let Some(p) = dev.planes().into_iter().find(|p| p.id == id) {
        return Some(Obj::Plane(p));
    }
    if let Some(c) = dev.connectors().into_iter().find(|c| c.id == id) {
        return Some(Obj::Connector(c));
    }
    if dev.encoders().iter().any(|e| e.id == id) {
        return Some(Obj::Other(OBJECT_ENCODER));
    }
    if kms::fb_by_id(f, id).is_some() {
        return Some(Obj::Other(OBJECT_FB));
    }
    if is_blob(id) {
        return Some(Obj::Other(OBJECT_BLOB));
    }
    if is_property(id) {
        return Some(Obj::Other(OBJECT_PROPERTY));
    }
    None
}

/// The properties an object has (for this client: atomic ones for atomic
/// clients, or for all when `every`), in order.
fn props_of(f: &File, obj: &Obj, every: bool) -> Vec<P> {
    let mut caps = f.state.lock().caps;
    caps.atomic |= every;
    let list: Vec<P> = match obj {
        Obj::Plane(info) => {
            let mut v = alloc::vec![
                P::Type,
                P::FbId,
                P::CrtcId,
                P::SrcX,
                P::SrcY,
                P::SrcW,
                P::SrcH,
                P::CrtcX,
                P::CrtcY,
                P::CrtcW,
                P::CrtcH,
                P::InFormats
            ];
            if info.kind == PlaneKind::Cursor && caps.hotspot {
                v.extend([P::HotspotX, P::HotspotY]);
            }
            v
        }
        Obj::Crtc(_) => alloc::vec![P::Active, P::ModeId],
        Obj::Connector(_) => alloc::vec![P::Edid, P::Dpms, P::LinkStatus, P::NonDesktop, P::CrtcId],
        Obj::Other(_) => Vec::new(),
    };
    list.into_iter()
        .filter(|p| caps.atomic || DEFS.iter().find(|d| d.p == *p).unwrap().flags & PROP_ATOMIC == 0)
        .collect()
}

/// The current value of property `p` of `obj`.
fn value(f: &File, obj: &Obj, p: P, state: &(Vec<CrtcState>, Vec<PlaneState>)) -> u64 {
    let (cs, ps) = state;
    match obj {
        Obj::Plane(info) => {
            let s = ps.iter().find(|s| s.plane == info.id);
            let s = s.cloned().unwrap_or_else(|| PlaneState::disabled(info.id));
            match p {
                P::Type => match info.kind {
                    PlaneKind::Overlay => 0,
                    PlaneKind::Primary => 1,
                    PlaneKind::Cursor => 2,
                },
                P::FbId => s.fb.as_ref().map_or(0, |fb| fb.id as u64),
                P::CrtcId => s.crtc.unwrap_or(0) as u64,
                P::SrcX => s.src.0 as u64,
                P::SrcY => s.src.1 as u64,
                P::SrcW => s.src.2 as u64,
                P::SrcH => s.src.3 as u64,
                P::CrtcX => s.dst.0 as i64 as u64,
                P::CrtcY => s.dst.1 as i64 as u64,
                P::CrtcW => s.dst.2 as u64,
                P::CrtcH => s.dst.3 as u64,
                P::HotspotX => s.hotspot.0 as i64 as u64,
                P::HotspotY => s.hotspot.1 as i64 as u64,
                P::InFormats => kernel_blob(in_formats(info.formats)) as u64,
                _ => 0,
            }
        }
        Obj::Crtc(info) => {
            let s = cs.iter().find(|s| s.crtc == info.id);
            match p {
                P::Active => s.is_some_and(|s| s.active) as u64,
                P::ModeId => s.and_then(|s| s.mode).map_or(0, |m| mode_blob(&m) as u64),
                _ => 0,
            }
        }
        Obj::Connector(c) => {
            let on = cs.iter().find(|s| s.connectors.contains(&c.id));
            match p {
                P::Edid => c.edid.as_ref().map_or(0, |e| kernel_blob(e.to_vec()) as u64),
                P::Dpms => {
                    if on.is_some_and(|s| s.active) {
                        0
                    } else {
                        3
                    }
                }
                P::CrtcId => on.map_or(0, |s| s.crtc as u64),
                _ => 0,
            }
        }
        Obj::Other(_) => {
            let _ = f;
            0
        }
    }
}

/// `struct drm_mode_obj_get_properties`: props_ptr, prop_values_ptr,
/// count_props, obj_id, obj_type.
pub fn obj_get_properties(f: &File, a: &mut Args) -> Result<(), isize> {
    let (props_ptr, values_ptr, room, id, kind) = (a.u64(0), a.u64(8), a.u32(16), a.u32(20), a.u32(24));
    let obj = find(f, id).ok_or(ENOENT)?;
    if kind != 0 && kind != obj.kind() {
        return Err(ENOENT);
    }
    if matches!(obj, Obj::Other(_)) {
        return Err(EINVAL);
    }
    let list = props_of(f, &obj, false);
    let state = f.dev.state();
    let ids: Vec<u32> = list.iter().map(|&p| prop_id(p)).collect();
    let values: Vec<u64> = list.iter().map(|&p| value(f, &obj, p, &state)).collect();
    put_array(props_ptr, room, &ids)?;
    put_array(values_ptr, room, &values)?;
    a.set32(16, list.len() as u32);
    Ok(())
}

/// `struct drm_mode_get_property`: values_ptr, enum_blob_ptr, prop_id,
/// flags, name[32], count_values, count_enum_blobs.
pub fn get_property(_f: &File, a: &mut Args) -> Result<(), isize> {
    let d = def(a.u32(16)).ok_or(ENOENT)?;
    let (values_ptr, enums_ptr, room_values, room_enums) = (a.u64(0), a.u64(8), a.u32(56), a.u32(60));
    let (kind_flags, values): (u32, Vec<u64>) = match &d.kind {
        Kind::Range(lo, hi) => (PROP_RANGE, alloc::vec![*lo, *hi]),
        Kind::Signed(lo, hi) => (PROP_SIGNED_RANGE, alloc::vec![*lo as u64, *hi as u64]),
        Kind::Enum(e) => (PROP_ENUM, e.iter().map(|(v, _)| *v).collect()),
        Kind::Blob => (PROP_BLOB, Vec::new()),
        Kind::Object(t) => (PROP_OBJECT, alloc::vec![*t as u64]),
    };
    a.set32(20, d.flags | kind_flags);
    let name = a.bytes(24, 32);
    name.fill(0);
    name[..d.name.len()].copy_from_slice(d.name.as_bytes());
    put_array(values_ptr, room_values, &values)?;
    a.set32(56, values.len() as u32);
    match &d.kind {
        Kind::Enum(e) => {
            // struct drm_mode_property_enum { u64 value; char name[32]; }
            for (i, (v, n)) in e.iter().enumerate().take(room_enums as usize) {
                let mut rec = [0u8; 40];
                rec[0..8].copy_from_slice(&v.to_le_bytes());
                rec[8..8 + n.len()].copy_from_slice(n.as_bytes());
                user::put_bytes(enums_ptr + i as u64 * 40, &rec)?;
            }
            a.set32(60, e.len() as u32);
        }
        Kind::Blob => a.set32(60, 0),
        _ => {}
    }
    Ok(())
}

// ------------------------------------------------------------ writes

/// A state change built from property writes on top of the current state.
pub struct Request<'a> {
    f: &'a File,
    current: (Vec<CrtcState>, Vec<PlaneState>),
    crtcs: Vec<CrtcState>,
    planes: Vec<PlaneState>,
    /// Connectors moved to a CRTC (or off any).
    moves: Vec<(ObjId, Option<ObjId>)>,
    /// Legacy writes reach atomic-only properties too.
    legacy: bool,
}

impl<'a> Request<'a> {
    pub fn new(f: &'a File) -> Request<'a> {
        Request { f, current: f.dev.state(), crtcs: Vec::new(), planes: Vec::new(), moves: Vec::new(), legacy: false }
    }

    fn crtc(&mut self, id: ObjId) -> Result<&mut CrtcState, isize> {
        if !self.crtcs.iter().any(|c| c.crtc == id) {
            let cur = self.current.0.iter().find(|c| c.crtc == id).cloned().ok_or(EINVAL)?;
            self.crtcs.push(cur);
        }
        Ok(self.crtcs.iter_mut().find(|c| c.crtc == id).unwrap())
    }

    fn plane(&mut self, id: ObjId) -> &mut PlaneState {
        if !self.planes.iter().any(|p| p.plane == id) {
            let cur =
                self.current.1.iter().find(|p| p.plane == id).cloned().unwrap_or_else(|| PlaneState::disabled(id));
            self.planes.push(cur);
        }
        self.planes.iter_mut().find(|p| p.plane == id).unwrap()
    }

    /// Property `prop` of object `obj` becomes `v`.
    pub fn set(&mut self, obj: u32, prop: u32, v: u64) -> Result<(), isize> {
        let o = find(self.f, obj).ok_or(ENOENT)?;
        if matches!(o, Obj::Other(_)) {
            return Err(EINVAL);
        }
        let d = def(prop).ok_or(ENOENT)?;
        if !props_of(self.f, &o, self.legacy).contains(&d.p) {
            return Err(ENOENT);
        }
        // Values out of the property's range, and unknown objects, are refused.
        match &d.kind {
            Kind::Range(lo, hi) if v < *lo || v > *hi => return Err(EINVAL),
            Kind::Signed(lo, hi) if (v as i64) < *lo || (v as i64) > *hi => return Err(EINVAL),
            Kind::Enum(e) if !e.iter().any(|(x, _)| *x == v) => return Err(EINVAL),
            _ => {}
        }
        if d.flags & PROP_IMMUTABLE != 0 {
            return Err(EINVAL);
        }
        let as_i32 = |v: u64| v as i64 as i32;
        match (o, d.p) {
            (Obj::Crtc(c), P::Active) => self.crtc(c.id)?.active = v == 1,
            (Obj::Crtc(c), P::ModeId) => {
                let mode = match v {
                    0 => None,
                    id => {
                        let b = blob(id as u32).ok_or(EINVAL)?;
                        if b.len() != 68 {
                            return Err(EINVAL);
                        }
                        Some(mode_from(&b)?)
                    }
                };
                self.crtc(c.id)?.mode = mode;
            }
            (Obj::Plane(pl), p) => {
                let fb =
                    if p == P::FbId && v != 0 { Some(kms::fb_by_id(self.f, v as u32).ok_or(EINVAL)?) } else { None };
                if p == P::CrtcId && v != 0 && !self.f.dev.crtcs().iter().any(|c| c.id == v as u32) {
                    return Err(EINVAL);
                }
                let s = self.plane(pl.id);
                match p {
                    P::FbId => s.fb = fb,
                    P::CrtcId => s.crtc = (v != 0).then_some(v as u32),
                    P::SrcX => s.src.0 = v as u32,
                    P::SrcY => s.src.1 = v as u32,
                    P::SrcW => s.src.2 = v as u32,
                    P::SrcH => s.src.3 = v as u32,
                    P::CrtcX => s.dst.0 = as_i32(v),
                    P::CrtcY => s.dst.1 = as_i32(v),
                    P::CrtcW => s.dst.2 = v as u32,
                    P::CrtcH => s.dst.3 = v as u32,
                    P::HotspotX => s.hotspot.0 = as_i32(v),
                    P::HotspotY => s.hotspot.1 = as_i32(v),
                    _ => return Err(EINVAL),
                }
            }
            (Obj::Connector(c), P::CrtcId) => {
                if v != 0 && !self.f.dev.crtcs().iter().any(|k| k.id == v as u32) {
                    return Err(EINVAL);
                }
                self.moves.retain(|(k, _)| *k != c.id);
                self.moves.push((c.id, (v != 0).then_some(v as u32)));
            }
            // DPMS is changed through the legacy call only (as on Linux);
            // the screen stays as the CRTC's state says.
            (Obj::Connector(_), P::Dpms) => return Err(EINVAL),
            _ => return Err(EINVAL),
        }
        Ok(())
    }

    /// Commits (or tests) the request with core `flags`; `event`: send a
    /// flip-complete event with this user data for each CRTC it touches.
    pub fn commit(mut self, flags: u32, event: Option<u64>) -> Result<(), isize> {
        for (conn, to) in core::mem::take(&mut self.moves) {
            let holding: Vec<ObjId> =
                self.current.0.iter().filter(|s| s.connectors.contains(&conn)).map(|s| s.crtc).collect();
            for crtc in holding {
                if Some(crtc) != to {
                    self.crtc(crtc)?.connectors.retain(|k| *k != conn);
                }
            }
            if let Some(crtc) = to {
                let s = self.crtc(crtc)?;
                if !s.connectors.contains(&conn) {
                    s.connectors.push(conn);
                }
            }
        }
        // The CRTCs whose picture changes: those in the request, and those
        // the changed planes were or will be on.
        let mut touched: Vec<ObjId> = self.crtcs.iter().map(|c| c.crtc).collect();
        for p in &self.planes {
            let old = self.current.1.iter().find(|s| s.plane == p.plane).and_then(|s| s.crtc);
            for c in [old, p.crtc].into_iter().flatten() {
                if !touched.contains(&c) {
                    touched.push(c);
                }
            }
        }
        let c = gpu::Commit { crtcs: self.crtcs, planes: self.planes, flags, user_data: 0 };
        kms::commit(self.f, c, &touched, event)
    }
}

/// `DRM_IOCTL_MODE_ATOMIC`: `struct drm_mode_atomic` { flags, count_objs,
/// objs_ptr, count_props_ptr, props_ptr, prop_values_ptr, reserved, user_data }.
pub fn atomic(f: &File, a: &mut Args) -> Result<(), isize> {
    const PAGE_FLIP_EVENT: u32 = 0x1;
    const PAGE_FLIP_ASYNC: u32 = 0x2;
    const TEST_ONLY: u32 = 0x100;
    const NONBLOCK: u32 = 0x200;
    const ALLOW_MODESET: u32 = 0x400;
    if !f.state.lock().caps.atomic {
        return Err(EINVAL);
    }
    let (flags, count) = (a.u32(0), a.u32(4));
    let (objs_ptr, counts_ptr, props_ptr, values_ptr) = (a.u64(8), a.u64(16), a.u64(24), a.u64(32));
    let (reserved, user_data) = (a.u64(40), a.u64(48));
    if flags & !(PAGE_FLIP_EVENT | PAGE_FLIP_ASYNC | TEST_ONLY | NONBLOCK | ALLOW_MODESET) != 0 || reserved != 0 {
        return Err(EINVAL);
    }
    // No asynchronous (tearing) flips; and a test sends no events.
    if flags & PAGE_FLIP_ASYNC != 0 || (flags & TEST_ONLY != 0 && flags & PAGE_FLIP_EVENT != 0) {
        return Err(EINVAL);
    }
    let objs: Vec<u32> = get_array(objs_ptr, count)?;
    let counts: Vec<u32> = get_array(counts_ptr, count)?;
    let total: u64 = counts.iter().map(|&c| c as u64).sum();
    if total > 1 << 16 {
        return Err(EINVAL);
    }
    let props: Vec<u32> = get_array(props_ptr, total as u32)?;
    let values: Vec<u64> = get_array(values_ptr, total as u32)?;
    let mut req = Request::new(f);
    let mut k = 0;
    for (obj, n) in objs.iter().zip(&counts) {
        for _ in 0..*n {
            req.set(*obj, props[k], values[k])?;
            k += 1;
        }
    }
    let core_flags = flags & (TEST_ONLY | NONBLOCK | ALLOW_MODESET);
    req.commit(core_flags, (flags & PAGE_FLIP_EVENT != 0).then_some(user_data))
}

/// `struct drm_mode_obj_set_property`: value, prop_id, obj_id, obj_type.
pub fn obj_set_property(f: &File, a: &mut Args) -> Result<(), isize> {
    let (v, prop, obj, kind) = (a.u64(0), a.u32(8), a.u32(12), a.u32(16));
    set_one(f, obj, kind, prop, v)
}

/// `struct drm_mode_connector_set_property`: value, prop_id, connector_id.
pub fn set_connector_property(f: &File, a: &mut Args) -> Result<(), isize> {
    let (v, prop, obj) = (a.u64(0), a.u32(8), a.u32(12));
    set_one(f, obj, OBJECT_CONNECTOR, prop, v)
}

/// The legacy property write: one property, committed at once.
fn set_one(f: &File, obj: u32, kind: u32, prop: u32, v: u64) -> Result<(), isize> {
    let o = find(f, obj).ok_or(ENOENT)?;
    if kind != o.kind() {
        return Err(ENOENT);
    }
    let d = def(prop).ok_or(EINVAL)?;
    if !props_of(f, &o, true).contains(&d.p) {
        return Err(EINVAL);
    }
    if d.p == P::Dpms {
        // Accepted; the display stays as its CRTC says.
        return if v <= 3 { Ok(()) } else { Err(EINVAL) };
    }
    let mut req = Request::new(f);
    req.legacy = true;
    req.set(obj, prop, v)?;
    req.commit(0, None)
}
