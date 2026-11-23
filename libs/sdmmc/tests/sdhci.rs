//! The driver against the simulated controller and cards (`aurora_sdmmc::mock`).
//!
//! Every test ends by checking that the simulation saw no programming
//! errors (the controller's and the cards' rules, see the mock's docs).

use aurora_sdmmc::card::Speed;
use aurora_sdmmc::host::{Timing, TransferMode};
use aurora_sdmmc::mock::{CardSpec, Fault, Mock, CAPS_FULL, CAPS_QEMU};
use aurora_sdmmc::regs::version;
use aurora_sdmmc::{CardKind, DmaBuf, EraseKind, Error, Host, Limits, Platform, Slot};

/// Simulated physical memory: each buffer gets an address below 4 GiB
/// (so SDMA and 32-bit ADMA work) that starts 256 KiB past a 512 KiB
/// boundary (so SDMA crosses boundaries), mapped to its host memory.
static MAP: std::sync::Mutex<Vec<(u64, usize, usize)>> = std::sync::Mutex::new(Vec::new());
static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0x10_0000);

fn translate(addr: u64) -> *mut u8 {
    let map = MAP.lock().unwrap();
    let &(base, ptr, len) = map
        .iter()
        .find(|&&(base, _, len)| addr >= base && addr < base + len as u64)
        .unwrap_or_else(|| panic!("DMA to unmapped address {addr:#x}"));
    let _ = len;
    (ptr + (addr - base) as usize) as *mut u8
}

struct Buf {
    mem: Vec<u8>,
    phys: u64,
}

impl Buf {
    fn boxed(len: usize) -> Box<dyn DmaBuf> {
        let mem = vec![0u8; len];
        let span = (len as u64 + 0x10_0000).next_multiple_of(0x10_0000);
        let phys = NEXT.fetch_add(span, std::sync::atomic::Ordering::Relaxed) + 0x4_0000;
        MAP.lock().unwrap().push((phys, mem.as_ptr() as usize, len));
        Box::new(Buf { mem, phys })
    }
}

impl DmaBuf for Buf {
    fn phys(&self) -> u64 {
        self.phys
    }
    fn bytes(&self) -> &[u8] {
        &self.mem
    }
    fn bytes_mut(&mut self) -> &mut [u8] {
        &mut self.mem
    }
}

/// Simulated time: every look at the clock costs 5 µs, waits 100 µs.
#[derive(Default)]
struct Clock {
    now: u64,
    notes: Vec<String>,
}

impl Platform for Clock {
    fn now_us(&mut self) -> u64 {
        self.now += 5;
        self.now
    }
    fn wait(&mut self) {
        self.now += 100;
    }
    fn delay_us(&mut self, us: u64) {
        self.now += us;
    }
    fn note(&mut self, msg: core::fmt::Arguments) {
        self.notes.push(msg.to_string());
    }
}

fn slot(mock: &Mock, limits: Limits) -> Slot<Mock, Clock> {
    slot_with(mock, limits, 256 * 1024)
}

fn slot_with(mock: &Mock, limits: Limits, buffer: usize) -> Slot<Mock, Clock> {
    let host = Host::new(mock.clone(), Clock::default(), Buf::boxed(buffer), Buf::boxed(4096)).expect("controller");
    Slot::new(host, limits)
}

fn pattern(seed: u32, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2654435761) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

fn clean(mock: &Mock) {
    let v = mock.violations();
    assert!(v.is_empty(), "the driver broke the rules: {v:#?}");
}

/// Writes and reads back runs that start and end on odd sectors and cross
/// the bounce buffer's size (several commands each).
fn round_trips(s: &mut Slot<Mock, Clock>, mock: &Mock, seed: u32) {
    let max = s.max_sectors() as u64;
    for (i, &(lba, n)) in [(0u64, 1usize), (7, 3), (63, 130), (1000, max as usize + 9), (2047, 1)].iter().enumerate() {
        let data = pattern(seed + i as u32, n * 512);
        s.write_from(lba, &data).expect("write");
        let mut back = vec![0u8; n * 512];
        s.read_into(lba, &mut back).expect("read");
        assert!(back == data, "read back differs at {lba}+{n}");
        // And the card holds it where it should.
        let on_card = mock.card(|c| c.parts[0][lba as usize * 512..(lba as usize + n) * 512].to_vec()).unwrap();
        assert!(on_card == data, "card contents differ at {lba}+{n}");
    }
}

#[test]
fn sdhc_on_a_full_v3_controller_every_transfer_mode() {
    let mock = Mock::new(CAPS_FULL, version::V3, translate);
    mock.insert(CardSpec::sdhc(64 * 2048));
    let mut s = slot(&mock, Limits::default());
    let info = s.init_card().expect("bring-up").clone();
    assert_eq!(info.kind, CardKind::Sdhc);
    assert_eq!(info.sectors, 64 * 2048);
    assert!(info.block_addressed);
    assert_eq!(info.width, 4);
    assert_eq!(info.timing, Timing::SdHs);
    assert_eq!(info.clock_hz, 50_000_000);
    assert!(!info.cmd23, "this card has no CMD23: Auto CMD12 ends transfers");
    assert_eq!(info.cid.name, "MOCK!");
    for (i, mode) in [TransferMode::Adma64, TransferMode::Adma32, TransferMode::Sdma, TransferMode::Pio].into_iter().enumerate() {
        assert_eq!(s.host.set_mode(mode), mode);
        round_trips(&mut s, &mock, 100 * i as u32);
    }
    // Multi-block reads were ended by Auto CMD12, never a separate CMD12.
    assert!(!mock.commands().iter().any(|c| c.0 == 12 || c.0 == 23));
    clean(&mock);
}

#[test]
fn sdma_crosses_buffer_boundaries() {
    // A 1 MiB buffer: transfers cross the 512 KiB SDMA boundary.
    let mock = Mock::new(CAPS_FULL, version::V3, translate);
    mock.insert(CardSpec::sdhc(16 * 2048));
    let mut s = slot_with(&mock, Limits { mode: Some(TransferMode::Sdma), ..Limits::default() }, 1024 * 1024);
    s.init_card().unwrap();
    assert_eq!(s.host.mode, TransferMode::Sdma);
    let data = pattern(7, 2048 * 512);
    s.write_from(100, &data).unwrap();
    let mut back = vec![0; data.len()];
    s.read_into(100, &mut back).unwrap();
    assert!(back == data);
    clean(&mock);
}

#[test]
fn sdsc_card_on_a_qemu_like_v2_controller() {
    let mock = Mock::new(CAPS_QEMU, version::V2, translate);
    mock.insert(CardSpec::sdsc(128 * 2048));
    let mut s = slot(&mock, Limits::default());
    let info = s.init_card().expect("bring-up").clone();
    assert_eq!(info.kind, CardKind::Sdsc);
    assert!(!info.block_addressed);
    assert_eq!(info.sectors, 128 * 2048);
    // 52 MHz base, power-of-two divider: high speed runs at 26 MHz.
    assert_eq!(info.timing, Timing::SdHs);
    assert_eq!(info.clock_hz, 26_000_000);
    assert_eq!(s.host.mode, TransferMode::Adma32, "no 64-bit addressing in these capabilities");
    round_trips(&mut s, &mock, 1);
    // Byte addresses on the wire.
    let reads: Vec<u32> = mock.commands().iter().filter(|c| c.0 == 17).map(|c| c.1).collect();
    assert!(reads.contains(&(2047 * 512)), "{reads:?}");
    clean(&mock);
}

#[test]
fn uhs_sdr104_switches_to_1v8_and_tunes() {
    let mock = Mock::new(CAPS_FULL, version::V3, translate);
    mock.insert(CardSpec::uhs(32 * 2048));
    let mut s = slot(&mock, Limits::default());
    let info = s.init_card().expect("bring-up").clone();
    assert_eq!(info.timing, Timing::UhsSdr104);
    assert!(info.signal_1v8);
    assert_eq!(info.clock_hz, 200_000_000);
    assert!(info.cmd23);
    let cmds = mock.commands();
    assert!(cmds.iter().any(|c| c.0 == 11), "CMD11 voltage switch");
    assert!(cmds.iter().filter(|c| c.0 == 19).count() >= 6, "tuning blocks");
    round_trips(&mut s, &mock, 3);
    // CMD23 bounds multi-block transfers.
    assert!(mock.commands().iter().any(|c| c.0 == 23));
    assert!(!mock.commands().iter().any(|c| c.0 == 12));
    clean(&mock);
}

#[test]
fn uhs_tuning_failure_falls_back_to_3v3_high_speed() {
    let mock = Mock::new(CAPS_FULL, version::V3, translate);
    mock.insert(CardSpec::uhs(32 * 2048));
    mock.inject(None, Fault::TuningFails, 1);
    let mut s = slot(&mock, Limits::default());
    let info = s.init_card().expect("bring-up").clone();
    assert_eq!(info.timing, Timing::SdHs);
    assert!(!info.signal_1v8);
    mock.clear_faults();
    round_trips(&mut s, &mock, 4);
    clean(&mock);
}

#[test]
fn uhs_limited_by_boot_option() {
    let mock = Mock::new(CAPS_FULL, version::V3, translate);
    mock.insert(CardSpec::uhs(32 * 2048));
    let mut s = slot(&mock, Limits { speed: Speed::High, ..Limits::default() });
    let info = s.init_card().unwrap().clone();
    assert_eq!(info.timing, Timing::SdHs);
    assert!(!mock.commands().iter().any(|c| c.0 == 11));
    let mut s = slot(&mock, Limits { speed: Speed::Default, mode: Some(TransferMode::Pio), ..Limits::default() });
    let info = s.init_card().unwrap().clone();
    assert_eq!(info.timing, Timing::Legacy);
    assert_eq!(s.host.mode, TransferMode::Pio);
    round_trips(&mut s, &mock, 5);
    clean(&mock);
}

#[test]
fn emmc_hs200_eight_bit_boot_partitions_untouched() {
    let mock = Mock::new(CAPS_FULL, version::V3, translate);
    mock.insert(CardSpec::emmc(256 * 2048));
    let boot_before = mock.card(|c| (c.parts[1].clone(), c.parts[2].clone(), c.parts[3].clone())).unwrap();
    let mut s = slot(&mock, Limits::default());
    let info = s.init_card().expect("bring-up").clone();
    assert_eq!(info.kind, CardKind::MmcHc);
    assert_eq!(info.sectors, 256 * 2048);
    assert_eq!(info.width, 8);
    assert_eq!(info.timing, Timing::Hs200, "HS400 is off unless asked for");
    assert!(info.signal_1v8);
    assert!(info.cache && info.trim);
    assert_eq!(info.boot_bytes, 512 * 1024);
    assert_eq!(info.rpmb_bytes, 128 * 1024);
    assert!(mock.commands().iter().filter(|c| c.0 == 21).count() >= 6, "CMD21 tuning");
    round_trips(&mut s, &mock, 6);
    // The cache holds writes until flushed.
    assert!(mock.card(|c| c.dirty).unwrap());
    s.flush().unwrap();
    assert!(!mock.card(|c| c.dirty).unwrap());
    let boot_after = mock.card(|c| (c.parts[1].clone(), c.parts[2].clone(), c.parts[3].clone())).unwrap();
    assert!(boot_before == boot_after, "boot or RPMB partitions changed");
    assert_eq!(mock.card(|c| c.hidden_touches).unwrap(), 0);
    clean(&mock);
}

#[test]
fn emmc_hs400_when_allowed() {
    let mock = Mock::new(CAPS_FULL, version::V3, translate);
    mock.insert(CardSpec::emmc(64 * 2048));
    let host = Host::new(mock.clone(), Clock::default(), Buf::boxed(256 * 1024), Buf::boxed(4096)).unwrap();
    let mut s = Slot::new(host, Limits { hs400: true, ..Limits::default() });
    s.host.caps.hs400 = true;
    let info = s.init_card().expect("bring-up").clone();
    assert_eq!(info.timing, Timing::Hs400);
    assert_eq!(info.width, 8);
    assert_eq!(mock.card(|c| c.ext[183]).unwrap(), 6, "8-bit DDR");
    round_trips(&mut s, &mock, 7);
    clean(&mock);
}

#[test]
fn emmc_left_in_a_boot_partition_is_moved_to_the_user_area() {
    let mock = Mock::new(CAPS_FULL, version::V3, translate);
    mock.insert(CardSpec { start_partition: 0x48 | 1, ..CardSpec::emmc(64 * 2048) });
    let mut s = slot(&mock, Limits::default());
    s.init_card().unwrap();
    // Boot enable bits kept, access back to the user area.
    assert_eq!(mock.card(|c| c.ext[179]).unwrap(), 0x48);
    round_trips(&mut s, &mock, 8);
    let boot = mock.card(|c| c.parts[1].iter().all(|&b| b == 0xB1)).unwrap();
    assert!(boot, "boot partition 1 was written");
    clean(&mock);
}

#[test]
fn emmc_without_hs200_host_support_runs_high_speed() {
    // A version 2 controller (no SDR104, no 1.8 V switch): 52 MHz at 4 bits.
    let mock = Mock::new(CAPS_QEMU | 1 << 18, version::V2, translate);
    mock.insert(CardSpec::emmc(64 * 2048));
    let mut s = slot(&mock, Limits::default());
    let info = s.init_card().unwrap().clone();
    assert_eq!(info.timing, Timing::MmcHs);
    assert_eq!(info.width, 8);
    round_trips(&mut s, &mock, 9);
    clean(&mock);
}

#[test]
fn small_byte_addressed_emmc() {
    let mock = Mock::new(CAPS_FULL, version::V3, translate);
    mock.insert(CardSpec { high_capacity: false, hs200: false, hs400: false, ..CardSpec::emmc(64 * 2048) });
    let mut s = slot(&mock, Limits { bus8: false, ..Limits::default() });
    let info = s.init_card().unwrap().clone();
    assert_eq!(info.kind, CardKind::Mmc);
    assert!(!info.block_addressed);
    assert_eq!(info.sectors, 64 * 2048);
    assert_eq!(info.width, 4);
    round_trips(&mut s, &mock, 10);
    clean(&mock);
}

#[test]
fn trim_and_erase() {
    let mock = Mock::new(CAPS_FULL, version::V3, translate);
    mock.insert(CardSpec::emmc(64 * 2048));
    let mut s = slot(&mock, Limits::default());
    s.init_card().unwrap();
    s.write_from(0, &vec![0x5A; 4096 * 512]).unwrap();
    // TRIM: any range.
    assert_eq!(s.erase(3, 5, EraseKind::Discard).unwrap(), 5);
    // Erase: whole 1024-sector groups inside 100..3100 only.
    assert_eq!(s.erase(100, 3000, EraseKind::Erase).unwrap(), 2048);
    assert_eq!(s.erase(10, 20, EraseKind::Erase).unwrap(), 0);
    let erased = mock.card(|c| c.erased.clone()).unwrap();
    assert_eq!(erased, vec![(3, 7, 1), (1024, 3071, 0)]);
    let mut back = vec![0u8; 4096 * 512];
    s.read_into(0, &mut back).unwrap();
    assert!(back[3 * 512..8 * 512].iter().all(|&b| b == 0));
    assert!(back[1024 * 512..3072 * 512].iter().all(|&b| b == 0));
    assert!(back[8 * 512..1024 * 512].iter().all(|&b| b == 0x5A));
    // SD erases any range.
    let mock = Mock::new(CAPS_FULL, version::V3, translate);
    mock.insert(CardSpec::sdhc(8 * 2048));
    let mut s = slot(&mock, Limits::default());
    s.init_card().unwrap();
    assert_eq!(s.erase(10, 20, EraseKind::Discard).unwrap(), 20);
    assert_eq!(mock.card(|c| c.erased.clone()).unwrap(), vec![(10, 29, 0)]);
    clean(&mock);
}

#[test]
fn errors_are_retried_and_recovered() {
    let mock = Mock::new(CAPS_FULL, version::V3, translate);
    mock.insert(CardSpec::sdhc(16 * 2048));
    let mut s = slot(&mock, Limits::default());
    s.init_card().unwrap();
    let data = pattern(11, 64 * 512);
    s.write_from(0, &data).unwrap();
    let mut back = vec![0u8; data.len()];
    for fault in [
        Fault::CmdCrc,
        Fault::CmdTimeout,
        Fault::DataCrc { after: 5 },
        Fault::DataTimeout { after: 0 },
        Fault::Adma,
    ] {
        mock.inject(Some(18), fault, 1);
        back.fill(0);
        s.read_into(0, &mut back).unwrap_or_else(|e| panic!("{fault:?}: {e:?}"));
        assert!(back == data, "{fault:?}: data differs after recovery");
        mock.inject(Some(25), fault, 1);
        s.write_from(0, &data).unwrap_or_else(|e| panic!("write {fault:?}: {e:?}"));
    }
    s.read_into(0, &mut back).unwrap();
    assert!(back == data);
    assert!(s.retries >= 10);
    assert_eq!(s.fallbacks, 0, "single errors don't slow the bus down");
    clean(&mock);
}

#[test]
fn persistent_errors_slow_the_bus_down() {
    let mock = Mock::new(CAPS_FULL, version::V3, translate);
    mock.insert(CardSpec::sdhc(16 * 2048));
    let mut s = slot(&mock, Limits::default());
    s.init_card().unwrap();
    assert_eq!(s.card.as_ref().unwrap().timing, Timing::SdHs);
    mock.inject(Some(18), Fault::DataCrc { after: 1 }, 3);
    let mut back = vec![0u8; 8 * 512];
    s.read_into(0, &mut back).expect("succeeds after falling back");
    assert_eq!(s.fallbacks, 1);
    assert_eq!(s.card.as_ref().unwrap().timing, Timing::Legacy);
    // A card that never answers gives up.
    mock.inject(Some(17), Fault::CmdTimeout, 100);
    assert!(s.read_into(0, &mut back[..512]).is_err());
    mock.clear_faults();
    clean(&mock);
}

#[test]
fn removal_and_write_protect() {
    let mock = Mock::new(CAPS_FULL, version::V3, translate);
    mock.insert(CardSpec::sdhc(16 * 2048));
    let mut s = slot(&mock, Limits::default());
    s.init_card().unwrap();
    mock.eject();
    assert!(!s.host.card_present());
    assert_eq!(s.host.take_card_events(), 1 << 7);
    assert_eq!(s.read(0, 1), Err(Error::NoCard));
    assert!(s.card.is_none());
    // A locked card: reads work, writes are refused before reaching it.
    mock.set_write_protect(true);
    mock.insert(CardSpec::sdhc(16 * 2048));
    let info = s.init_card().unwrap().clone();
    assert!(info.read_only);
    s.read(0, 4).unwrap();
    assert_eq!(s.write(0, 1), Err(Error::ReadOnly));
    assert_eq!(s.erase(0, 1, EraseKind::Discard), Err(Error::ReadOnly));
    clean(&mock);
}

#[test]
fn empty_slot_and_out_of_range() {
    let mock = Mock::new(CAPS_FULL, version::V3, translate);
    let mut s = slot(&mock, Limits::default());
    assert_eq!(s.init_card().err(), Some(Error::NoCard));
    mock.insert(CardSpec::sdhc(4 * 2048));
    s.init_card().unwrap();
    assert_eq!(s.read(4 * 2048, 1), Err(Error::OutOfRange));
    assert_eq!(s.read(4 * 2048 - 1, 2), Err(Error::OutOfRange));
    s.read(4 * 2048 - 1, 1).unwrap();
    clean(&mock);
}

#[test]
fn interrupt_line_follows_enabled_statuses() {
    let mock = Mock::new(CAPS_FULL, version::V3, translate);
    let mut s = slot(&mock, Limits::default());
    assert!(!mock.irq());
    mock.insert(CardSpec::sdhc(4 * 2048));
    assert!(mock.irq(), "card insertion interrupts");
    s.host.take_card_events();
    assert!(!mock.irq());
    s.init_card().unwrap();
    assert!(!mock.irq(), "every completion was acknowledged");
    clean(&mock);
}

