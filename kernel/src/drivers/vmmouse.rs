//! VMware backdoor absolute pointer ("vmmouse"), implemented by QEMU and VMware.

use core::arch::asm;

const MAGIC: u32 = 0x564D_5868;
const PORT: u16 = 0x5658;
const CMD_GETVERSION: u32 = 10;
const CMD_ABSPOINTER_DATA: u32 = 39;
const CMD_ABSPOINTER_STATUS: u32 = 40;
const CMD_ABSPOINTER_COMMAND: u32 = 41;
const ABSPOINTER_ENABLE: u32 = 0x4541_4552;
const ABSPOINTER_ABSOLUTE: u32 = 0x5342_4152;
const VERSION_ID: u32 = 0x3442_554A;

pub struct Packet {
    pub x: u32,
    pub y: u32,
    pub buttons: u8,
    pub wheel: i8,
}

/// Issues a backdoor call; returns (eax, ebx, ecx, edx).
fn call(cmd: u32, arg: u32) -> (u32, u32, u32, u32) {
    let (a, c, d): (u32, u32, u32);
    let mut b: u64 = arg as u64;
    unsafe {
        asm!(
            "xchg rbx, {b}",
            "in eax, dx",
            "xchg rbx, {b}",
            b = inout(reg) b,
            inout("eax") MAGIC => a,
            inout("ecx") cmd => c,
            inout("edx") PORT as u32 => d,
            options(nostack)
        );
    }
    (a, b as u32, c, d)
}

pub fn init() -> bool {
    let (_, magic, _, _) = call(CMD_GETVERSION, !MAGIC);
    if magic != MAGIC {
        return false;
    }
    call(CMD_ABSPOINTER_COMMAND, ABSPOINTER_ENABLE);
    let (status, ..) = call(CMD_ABSPOINTER_STATUS, 0);
    if status & 0xFFFF == 0 {
        return false;
    }
    let (id, ..) = call(CMD_ABSPOINTER_DATA, 1);
    if id != VERSION_ID {
        return false;
    }
    call(CMD_ABSPOINTER_COMMAND, ABSPOINTER_ABSOLUTE);
    true
}

pub fn poll() -> Option<Packet> {
    let (status, ..) = call(CMD_ABSPOINTER_STATUS, 0);
    let queued = status & 0xFFFF;
    if queued == 0xFFFF {
        // Error: re-enable and carry on.
        call(CMD_ABSPOINTER_COMMAND, ABSPOINTER_ENABLE);
        call(CMD_ABSPOINTER_COMMAND, ABSPOINTER_ABSOLUTE);
        return None;
    }
    if queued < 4 {
        return None;
    }
    let (flags, x, y, z) = call(CMD_ABSPOINTER_DATA, 4);
    let mut buttons = 0;
    if flags & 0x20 != 0 {
        buttons |= super::input::BUTTON_LEFT;
    }
    if flags & 0x10 != 0 {
        buttons |= super::input::BUTTON_RIGHT;
    }
    if flags & 0x08 != 0 {
        buttons |= super::input::BUTTON_MIDDLE;
    }
    Some(Packet { x, y, buttons, wheel: z as i8 })
}
