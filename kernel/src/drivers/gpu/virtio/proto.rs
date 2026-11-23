//! virtio-gpu wire format (virtio 1.2, section 5.7): command and response
//! codes, the control header, and little-endian encoding helpers. The 3D
//! commands are listed for the context code that will use the same layer.

#![allow(dead_code)]

// Feature bits.
pub const F_VIRGL: u64 = 1 << 0;
pub const F_EDID: u64 = 1 << 1;
pub const F_RESOURCE_UUID: u64 = 1 << 2;
pub const F_RESOURCE_BLOB: u64 = 1 << 3;
pub const F_CONTEXT_INIT: u64 = 1 << 4;

// Device configuration.
pub const CFG_EVENTS_READ: u64 = 0;
pub const CFG_EVENTS_CLEAR: u64 = 4;
pub const CFG_NUM_SCANOUTS: u64 = 8;
pub const CFG_NUM_CAPSETS: u64 = 12;
pub const EVENT_DISPLAY: u32 = 1;

pub const MAX_SCANOUTS: usize = 16;

// 2D commands.
pub const GET_DISPLAY_INFO: u32 = 0x0100;
pub const RESOURCE_CREATE_2D: u32 = 0x0101;
pub const RESOURCE_UNREF: u32 = 0x0102;
pub const SET_SCANOUT: u32 = 0x0103;
pub const RESOURCE_FLUSH: u32 = 0x0104;
pub const TRANSFER_TO_HOST_2D: u32 = 0x0105;
pub const RESOURCE_ATTACH_BACKING: u32 = 0x0106;
pub const RESOURCE_DETACH_BACKING: u32 = 0x0107;
pub const GET_CAPSET_INFO: u32 = 0x0108;
pub const GET_CAPSET: u32 = 0x0109;
pub const GET_EDID: u32 = 0x010A;
pub const RESOURCE_ASSIGN_UUID: u32 = 0x010B;
pub const RESOURCE_CREATE_BLOB: u32 = 0x010C;
pub const SET_SCANOUT_BLOB: u32 = 0x010D;

// 3D commands (not used yet).
pub const CTX_CREATE: u32 = 0x0200;
pub const CTX_DESTROY: u32 = 0x0201;
pub const CTX_ATTACH_RESOURCE: u32 = 0x0202;
pub const CTX_DETACH_RESOURCE: u32 = 0x0203;
pub const RESOURCE_CREATE_3D: u32 = 0x0204;
pub const TRANSFER_TO_HOST_3D: u32 = 0x0205;
pub const TRANSFER_FROM_HOST_3D: u32 = 0x0206;
pub const SUBMIT_3D: u32 = 0x0207;
pub const RESOURCE_MAP_BLOB: u32 = 0x0208;
pub const RESOURCE_UNMAP_BLOB: u32 = 0x0209;

// Cursor queue.
pub const UPDATE_CURSOR: u32 = 0x0300;
pub const MOVE_CURSOR: u32 = 0x0301;

// Responses.
pub const OK_NODATA: u32 = 0x1100;
pub const OK_DISPLAY_INFO: u32 = 0x1101;
pub const OK_CAPSET_INFO: u32 = 0x1102;
pub const OK_CAPSET: u32 = 0x1103;
pub const OK_EDID: u32 = 0x1104;
pub const OK_RESOURCE_UUID: u32 = 0x1105;
pub const OK_MAP_INFO: u32 = 0x1106;
pub const ERR_UNSPEC: u32 = 0x1200;
pub const ERR_OUT_OF_MEMORY: u32 = 0x1201;
pub const ERR_INVALID_SCANOUT_ID: u32 = 0x1202;
pub const ERR_INVALID_RESOURCE_ID: u32 = 0x1203;
pub const ERR_INVALID_CONTEXT_ID: u32 = 0x1204;
pub const ERR_INVALID_PARAMETER: u32 = 0x1205;

pub const FLAG_FENCE: u32 = 1 << 0;
pub const FLAG_INFO_RING_IDX: u32 = 1 << 1;

// Formats.
pub const FORMAT_B8G8R8A8_UNORM: u32 = 1;
pub const FORMAT_B8G8R8X8_UNORM: u32 = 2;
pub const FORMAT_A8R8G8B8_UNORM: u32 = 3;
pub const FORMAT_X8R8G8B8_UNORM: u32 = 4;

/// The control header that starts every command and response (24 bytes).
#[derive(Clone, Copy, Debug, Default)]
pub struct Header {
    pub kind: u32,
    pub flags: u32,
    pub fence: u64,
    pub ctx: u32,
    pub ring: u8,
}

pub const HEADER_LEN: usize = 24;

impl Header {
    pub fn new(kind: u32) -> Header {
        Header { kind, ..Header::default() }
    }

    /// A command for 3D context `ctx` (0 for 2D).
    pub fn ctx(mut self, ctx: u32) -> Header {
        self.ctx = ctx;
        self
    }

    pub fn decode(b: &[u8]) -> Header {
        Header {
            kind: le32(b, 0),
            flags: le32(b, 4),
            fence: le32(b, 8) as u64 | (le32(b, 12) as u64) << 32,
            ctx: le32(b, 16),
            ring: b[20],
        }
    }

    pub fn is_ok(&self) -> bool {
        (OK_NODATA..ERR_UNSPEC).contains(&self.kind)
    }
}

pub fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// A command being encoded, at most 512 bytes (the variable part of
/// larger ones, like a backing's page list, goes in a second buffer).
pub struct Cmd {
    pub buf: [u8; 512],
    pub len: usize,
    pub header: Header,
}

impl Cmd {
    pub fn new(header: Header) -> Cmd {
        let mut c = Cmd { buf: [0; 512], len: HEADER_LEN, header };
        c.put_header();
        c
    }

    fn put_header(&mut self) {
        let h = self.header;
        self.buf[0..4].copy_from_slice(&h.kind.to_le_bytes());
        self.buf[4..8].copy_from_slice(&h.flags.to_le_bytes());
        self.buf[8..16].copy_from_slice(&h.fence.to_le_bytes());
        self.buf[16..20].copy_from_slice(&h.ctx.to_le_bytes());
        self.buf[20] = h.ring;
    }

    /// Asks for a fence: the response comes once the host has finished.
    pub fn set_fence(&mut self, fence: u64) {
        self.header.flags |= FLAG_FENCE;
        self.header.fence = fence;
        self.put_header();
    }

    pub fn u32(mut self, v: u32) -> Cmd {
        self.buf[self.len..self.len + 4].copy_from_slice(&v.to_le_bytes());
        self.len += 4;
        self
    }

    pub fn u64(mut self, v: u64) -> Cmd {
        self.buf[self.len..self.len + 8].copy_from_slice(&v.to_le_bytes());
        self.len += 8;
        self
    }

    pub fn rect(self, r: Rect) -> Cmd {
        self.u32(r.x).u32(r.y).u32(r.w).u32(r.h)
    }

    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// One scanout of a `GET_DISPLAY_INFO` response.
#[derive(Clone, Copy, Debug, Default)]
pub struct DisplayOne {
    pub rect: Rect,
    pub enabled: bool,
    pub flags: u32,
}

/// Decodes `OK_DISPLAY_INFO` (after the header).
pub fn display_info(b: &[u8]) -> [DisplayOne; MAX_SCANOUTS] {
    let mut out = [DisplayOne::default(); MAX_SCANOUTS];
    for (i, d) in out.iter_mut().enumerate() {
        let o = HEADER_LEN + i * 24;
        if o + 24 > b.len() {
            break;
        }
        d.rect = Rect { x: le32(b, o), y: le32(b, o + 4), w: le32(b, o + 8), h: le32(b, o + 12) };
        d.enabled = le32(b, o + 16) != 0;
        d.flags = le32(b, o + 20);
    }
    out
}

/// Length of a `GET_DISPLAY_INFO` response.
pub const DISPLAY_INFO_LEN: usize = HEADER_LEN + MAX_SCANOUTS * 24;
/// Length of a `GET_EDID` response: header, size, padding, 1024 bytes.
pub const EDID_LEN: usize = HEADER_LEN + 8 + 1024;
