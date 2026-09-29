//! NVMe controller driver: admin queue + one I/O queue pair. Completions
//! arrive by MSI-X (the issuing task sleeps meanwhile), or are polled.

use super::{check_io, register, wait_for, wait_irq, BlockDevice, BlockResult, Dma};
use crate::drivers::pci;
use crate::sync::{Mutex, WaitQueue};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use aurora_abi::err::*;
use core::sync::atomic::{fence, AtomicUsize, Ordering};

const REG_CAP: u64 = 0x00;
const REG_CC: u64 = 0x14;
const REG_CSTS: u64 = 0x1C;
const REG_AQA: u64 = 0x24;
const REG_ASQ: u64 = 0x28;
const REG_ACQ: u64 = 0x30;

const ADMIN_CREATE_SQ: u8 = 0x01;
const ADMIN_CREATE_CQ: u8 = 0x05;
const ADMIN_IDENTIFY: u8 = 0x06;
const IO_FLUSH: u8 = 0x00;
const IO_WRITE: u8 = 0x01;
const IO_READ: u8 = 0x02;

const QDEPTH: u16 = 16;
const MAX_BYTES: usize = 64 * 1024;

struct QueuePair {
    sq: Dma,
    cq: Dma,
    sq_tail: u16,
    cq_head: u16,
    phase: u16,
    sq_doorbell: u64,
    cq_doorbell: u64,
    cid: u16,
    irq: Option<&'static WaitQueue>,
}

/// MSI-X handler: completions are read by the waiting task itself.
fn interrupt(arg: usize) {
    unsafe { &*(arg as *const WaitQueue) }.wake_all();
}

impl QueuePair {
    fn new(regs: u64, qid: u16, stride: u64, irq: Option<&'static WaitQueue>) -> Option<QueuePair> {
        Some(QueuePair {
            sq: Dma::new(QDEPTH as usize * 64)?,
            cq: Dma::new(QDEPTH as usize * 16)?,
            sq_tail: 0,
            cq_head: 0,
            phase: 1,
            sq_doorbell: regs + 0x1000 + (2 * qid as u64) * stride,
            cq_doorbell: regs + 0x1000 + (2 * qid as u64 + 1) * stride,
            cid: 0,
            irq,
        })
    }

    /// Empties the queue (after the controller was reset).
    fn reset(&mut self) {
        self.sq_tail = 0;
        self.cq_head = 0;
        self.phase = 1;
        self.cq.bytes_mut().fill(0);
    }

    /// Submits a command (16 dwords; CID is filled in) and waits for its completion.
    fn run(&mut self, mut cmd: [u32; 16]) -> BlockResult<u32> {
        self.cid = self.cid.wrapping_add(1);
        cmd[0] = (cmd[0] & 0xFFFF) | (self.cid as u32) << 16;
        let slot = self.sq_tail as usize * 64;
        for (i, dw) in cmd.iter().enumerate() {
            self.sq.write::<u32>(slot + i * 4, *dw);
        }
        self.sq_tail = (self.sq_tail + 1) % QDEPTH;
        fence(Ordering::SeqCst);
        unsafe { (self.sq_doorbell as *mut u32).write_volatile(self.sq_tail as u32) };

        let entry = self.cq_head as usize * 16;
        let phase = self.phase;
        let cq = &self.cq;
        wait_irq(self.irq, 5000, || (cq.read::<u16>(entry + 14) & 1) == phase)?;
        let status = self.cq.read::<u16>(entry + 14) >> 1;
        let result = self.cq.read::<u32>(entry);
        self.cq_head = (self.cq_head + 1) % QDEPTH;
        if self.cq_head == 0 {
            self.phase ^= 1;
        }
        unsafe { (self.cq_doorbell as *mut u32).write_volatile(self.cq_head as u32) };
        if status & 0x7FF != 0 {
            log!("nvme", "command {:#x} failed: status {:#x}", cmd[0] & 0xFF, status);
            return Err(EIO);
        }
        Ok(result)
    }
}

struct Io {
    queue: QueuePair,
    buf: Dma,
    /// PRP list for transfers larger than two pages.
    prp_list: Dma,
}

pub struct NvmeDisk {
    index: usize,
    regs: u64,
    /// Controller ready timeout (ms).
    timeout: u64,
    irq: Option<&'static WaitQueue>,
    admin: Mutex<QueuePair>,
    io: Mutex<Io>,
    sectors: u64,
    sector_size: u32,
    model: String,
}

impl NvmeDisk {
    fn transfer(&self, opcode: u8, lba: u64, len: usize, io: &mut Io) -> BlockResult<()> {
        let ss = self.sector_size as usize;
        let pages = len.div_ceil(4096);
        let prp1 = io.buf.phys;
        let prp2 = match pages {
            0 | 1 => 0,
            2 => io.buf.phys + 4096,
            _ => {
                for i in 1..pages {
                    io.prp_list.write::<u64>((i - 1) * 8, io.buf.phys + i as u64 * 4096);
                }
                io.prp_list.phys
            }
        };
        let mut cmd = [0u32; 16];
        cmd[0] = opcode as u32;
        cmd[1] = 1; // namespace 1
        cmd[6] = prp1 as u32;
        cmd[7] = (prp1 >> 32) as u32;
        cmd[8] = prp2 as u32;
        cmd[9] = (prp2 >> 32) as u32;
        cmd[10] = lba as u32;
        cmd[11] = (lba >> 32) as u32;
        cmd[12] = (len / ss).saturating_sub(1) as u32;
        io.queue.run(cmd).map(|_| ())
    }
}

impl BlockDevice for NvmeDisk {
    fn name(&self) -> String {
        format!("nvme{}n1", self.index)
    }
    fn sector_size(&self) -> u32 {
        self.sector_size
    }
    fn sectors(&self) -> u64 {
        self.sectors
    }
    fn describe(&self) -> String {
        format!("NVMe — {}", self.model)
    }
    fn read(&self, lba: u64, buf: &mut [u8]) -> BlockResult<()> {
        check_io(self, lba, buf.len())?;
        let mut io = self.io.lock();
        let per = (MAX_BYTES / self.sector_size as usize) as u64;
        for (i, chunk) in buf.chunks_mut(MAX_BYTES).enumerate() {
            self.transfer(IO_READ, lba + i as u64 * per, chunk.len(), &mut io)?;
            chunk.copy_from_slice(&io.buf.bytes_mut()[..chunk.len()]);
        }
        Ok(())
    }
    fn write(&self, lba: u64, buf: &[u8]) -> BlockResult<()> {
        check_io(self, lba, buf.len())?;
        let mut io = self.io.lock();
        let per = (MAX_BYTES / self.sector_size as usize) as u64;
        for (i, chunk) in buf.chunks(MAX_BYTES).enumerate() {
            io.buf.bytes_mut()[..chunk.len()].copy_from_slice(chunk);
            self.transfer(IO_WRITE, lba + i as u64 * per, chunk.len(), &mut io)?;
        }
        Ok(())
    }
    fn flush(&self) -> BlockResult<()> {
        let mut io = self.io.lock();
        let mut cmd = [0u32; 16];
        cmd[0] = IO_FLUSH as u32;
        cmd[1] = 1;
        io.queue.run(cmd).map(|_| ())
    }
}

static COUNT: AtomicUsize = AtomicUsize::new(0);

fn r32(regs: u64, o: u64) -> u32 {
    unsafe { ((regs + o) as *const u32).read_volatile() }
}

fn w32(regs: u64, o: u64, v: u32) {
    unsafe { ((regs + o) as *mut u32).write_volatile(v) }
}

fn w64(regs: u64, o: u64, v: u64) {
    w32(regs, o, v as u32);
    w32(regs, o + 4, (v >> 32) as u32);
}

/// Disables the controller, points it at the admin queues and enables it.
fn enable(regs: u64, admin: &QueuePair, timeout: u64) -> bool {
    w32(regs, REG_CC, r32(regs, REG_CC) & !1);
    if wait_for(timeout, || r32(regs, REG_CSTS) & 1 == 0).is_err() {
        log!("nvme", "controller did not disable");
        return false;
    }
    w32(regs, REG_AQA, ((QDEPTH as u32 - 1) << 16) | (QDEPTH as u32 - 1));
    w64(regs, REG_ASQ, admin.sq.phys);
    w64(regs, REG_ACQ, admin.cq.phys);
    // 4 KiB pages, NVM command set, 64-byte SQEs, 16-byte CQEs.
    w32(regs, REG_CC, (6 << 16) | (4 << 20) | 1);
    if wait_for(timeout, || r32(regs, REG_CSTS) & 1 == 1).is_err() || r32(regs, REG_CSTS) & 2 != 0 {
        log!("nvme", "controller did not become ready");
        return false;
    }
    true
}

/// I/O completion queue 1, then submission queue 1 bound to it.
fn create_io_queues(admin: &mut QueuePair, queue: &QueuePair, irq: bool) -> bool {
    let mut cmd = [0u32; 16];
    cmd[0] = ADMIN_CREATE_CQ as u32;
    cmd[6] = queue.cq.phys as u32;
    cmd[7] = (queue.cq.phys >> 32) as u32;
    cmd[10] = ((QDEPTH as u32 - 1) << 16) | 1;
    // Physically contiguous; interrupts on MSI-X vector 0 when we have it.
    cmd[11] = 1 | if irq { 1 << 1 } else { 0 };
    if admin.run(cmd).is_err() {
        return false;
    }
    let mut cmd = [0u32; 16];
    cmd[0] = ADMIN_CREATE_SQ as u32;
    cmd[6] = queue.sq.phys as u32;
    cmd[7] = (queue.sq.phys >> 32) as u32;
    cmd[10] = ((QDEPTH as u32 - 1) << 16) | 1;
    cmd[11] = (1 << 16) | 1; // CQ 1, physically contiguous
    admin.run(cmd).is_ok()
}

static DISKS: crate::sync::IrqMutex<alloc::vec::Vec<Arc<NvmeDisk>>> =
    crate::sync::IrqMutex::new(alloc::vec::Vec::new());

pub fn probe(dev: &pci::Device) {
    dev.enable();
    let Some(regs) = dev.map_bar(0) else { return };
    let cap = r32(regs, REG_CAP) as u64 | (r32(regs, REG_CAP + 4) as u64) << 32;
    let stride = 4u64 << ((cap >> 32) & 0xF);
    let timeout = ((cap >> 24) & 0xFF).max(1) * 500;
    let queue: &'static WaitQueue = Box::leak(Box::new(WaitQueue::new()));
    let irq = dev.enable_msi("nvme", interrupt, queue as *const WaitQueue as usize).map(|_| queue);
    let Some(mut admin) = QueuePair::new(regs, 0, stride, irq) else { return };
    if !enable(regs, &admin, timeout) {
        return;
    }

    let Some(ident) = Dma::new(4096) else { return };
    let identify = |admin: &mut QueuePair, cns: u32, nsid: u32| {
        let mut cmd = [0u32; 16];
        cmd[0] = ADMIN_IDENTIFY as u32;
        cmd[1] = nsid;
        cmd[6] = ident.phys as u32;
        cmd[7] = (ident.phys >> 32) as u32;
        cmd[10] = cns;
        admin.run(cmd)
    };
    if identify(&mut admin, 1, 0).is_err() {
        return;
    }
    let model: String = (24..64).map(|i| ident.read::<u8>(i) as char).collect::<String>().trim().into();
    if identify(&mut admin, 0, 1).is_err() {
        return;
    }
    let sectors = ident.read::<u64>(0);
    let flbas = ident.read::<u8>(26) & 0xF;
    let lbads = ident.read::<u32>(128 + flbas as usize * 4) >> 16 & 0xFF;
    let sector_size = 1u32 << lbads;
    if sectors == 0 || !(9..=12).contains(&lbads) {
        log!("nvme", "namespace 1 unusable (size {}, lbads {})", sectors, lbads);
        return;
    }
    let Some(queue) = QueuePair::new(regs, 1, stride, irq) else { return };
    if !create_io_queues(&mut admin, &queue, irq.is_some()) {
        return;
    }
    let (Some(buf), Some(prp_list)) = (Dma::new(MAX_BYTES), Dma::new(4096)) else { return };
    let index = COUNT.fetch_add(1, Ordering::Relaxed);
    if irq.is_some() {
        log!("nvme", "interrupts via MSI-X");
    }
    let disk = Arc::new(NvmeDisk {
        index,
        regs,
        timeout,
        irq,
        admin: Mutex::new(admin),
        io: Mutex::new(Io { queue, buf, prp_list }),
        sectors,
        sector_size,
        model,
    });
    DISKS.lock().push(disk.clone());
    register(disk);
}

/// After sleep: the controller was reset; set up its queues again.
pub fn resume() {
    let disks: alloc::vec::Vec<Arc<NvmeDisk>> = DISKS.lock().clone();
    for d in disks {
        let mut admin = d.admin.lock();
        let mut io = d.io.lock();
        admin.reset();
        io.queue.reset();
        if !enable(d.regs, &admin, d.timeout) || !create_io_queues(&mut admin, &io.queue, d.irq.is_some()) {
            log!("nvme", "nvme{}: controller did not come back after sleep", d.index);
        }
    }
}
