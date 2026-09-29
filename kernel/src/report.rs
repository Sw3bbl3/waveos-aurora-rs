//! Plain-text reports about the machine: `lspci`, `lsusb`, `cpuinfo`,
//! `dmesg`, and the boot log saved to the EFI partition — which is what to
//! send along when WaveOS misbehaves on real hardware.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::x86_64::__cpuid;
use core::fmt::Write;

fn class_name(class: u8, sub: u8, prog_if: u8) -> &'static str {
    match (class, sub, prog_if) {
        (0x01, 0x01, _) => "IDE controller",
        (0x01, 0x06, _) => "SATA controller (AHCI)",
        (0x01, 0x08, _) => "NVMe controller",
        (0x01, _, _) => "Storage controller",
        (0x02, _, _) => "Network controller",
        (0x03, _, _) => "Display controller",
        (0x04, 0x03, _) => "Audio device (HD Audio)",
        (0x04, _, _) => "Multimedia device",
        (0x06, 0x00, _) => "Host bridge",
        (0x06, 0x01, _) => "ISA bridge",
        (0x06, 0x04, _) => "PCI bridge",
        (0x06, _, _) => "Bridge",
        (0x0C, 0x03, 0x30) => "USB controller (xHCI)",
        (0x0C, 0x03, 0x20) => "USB controller (EHCI)",
        (0x0C, 0x03, _) => "USB controller",
        (0x0C, 0x05, _) => "SMBus controller",
        (0x0D, _, _) => "Wireless controller",
        _ => "Device",
    }
}

pub fn pci() -> String {
    let mut s = String::new();
    for d in crate::drivers::pci::devices() {
        let _ = writeln!(
            s,
            "{:02x}:{:02x}.{} [{:04x}:{:04x}] {} (class {:02x}{:02x}{:02x})",
            d.bus,
            d.dev,
            d.func,
            d.vendor,
            d.device,
            class_name(d.class, d.subclass, d.prog_if),
            d.class,
            d.subclass,
            d.prog_if
        );
    }
    s
}

pub fn usb() -> String {
    let mut s = String::new();
    for line in crate::drivers::usb::list() {
        let _ = writeln!(s, "{line}");
    }
    if s.is_empty() {
        s.push_str("No USB devices.\n");
    }
    s
}

pub fn cpu() -> String {
    let mut s = String::new();
    let leaf1 = __cpuid(1);
    let leaf7 = __cpuid(7);
    let ext = __cpuid(0x8000_0007);
    let flags: Vec<&str> = [
        (leaf1.ecx & (1 << 20) != 0, "sse4.2"),
        (leaf1.ecx & (1 << 28) != 0, "avx"),
        (leaf7.ebx & (1 << 5) != 0, "avx2"),
        (leaf1.ecx & (1 << 25) != 0, "aes"),
        (leaf1.ecx & (1 << 30) != 0, "rdrand"),
        (leaf1.ecx & (1 << 21) != 0, "x2apic"),
        (leaf1.ecx & (1 << 24) != 0, "tsc-deadline"),
        (ext.edx & (1 << 8) != 0, "invariant-tsc"),
        (leaf7.ebx & (1 << 7) != 0, "smep"),
        (leaf7.ebx & (1 << 20) != 0, "smap"),
    ]
    .iter()
    .filter(|f| f.0)
    .map(|f| f.1)
    .collect();
    let family = (leaf1.eax >> 8) & 0xF;
    let model = (leaf1.eax >> 4) & 0xF | ((leaf1.eax >> 16) & 0xF) << 4;
    let _ = writeln!(s, "Processor:  {}", crate::arch::cpu::brand());
    let _ = writeln!(
        s,
        "Vendor:     {} (family {:#x}, model {:#x}, stepping {})",
        crate::arch::cpu::vendor(),
        family,
        model,
        leaf1.eax & 0xF
    );
    let online = crate::arch::percpu::count();
    let ids: Vec<String> = crate::arch::percpu::online()
        .map(|c| format!("{}", crate::arch::percpu::CPUS[c].lapic_id.load(core::sync::atomic::Ordering::Relaxed)))
        .collect();
    let _ = writeln!(s, "CPUs:       {} online (APIC ids {})", online, ids.join(", "));
    let hz = crate::time::tsc_hz();
    if hz != 0 {
        let _ = writeln!(s, "TSC:        {}.{:03} GHz", hz / 1_000_000_000, hz / 1_000_000 % 1000);
    }
    let clock = match crate::time::source() {
        crate::time::Source::Tsc => "TSC",
        crate::time::Source::Hpet => "HPET",
        crate::time::Source::Tick => "timer ticks",
    };
    let hpet = if crate::drivers::hpet::present() { " (HPET present)" } else { "" };
    let _ = writeln!(s, "Clock:      {clock}{hpet}");
    let _ = writeln!(s, "Features:   {}", flags.join(" "));
    let rng = if crate::random::hardware() { "RDRAND/RDSEED + timer jitter" } else { "timer jitter only" };
    let _ = writeln!(s, "Randomness: {rng}");
    let times = crate::sched::cpu_times();
    for (i, (busy, idle)) in times.iter().enumerate() {
        let total = (busy + idle).max(1);
        let _ = writeln!(s, "CPU {:<2}      {:>3}% busy since boot", i, busy * 100 / total);
    }
    s
}

pub fn log() -> Vec<u8> {
    crate::drivers::serial::history()
}

/// Saves the kernel log to `/Boot/aurora/boot.log` (the EFI partition, so
/// it can be read on another computer).
pub fn save_boot_log() {
    let mut text = format!(
        "WaveOS Aurora {} boot log\n\n== CPU ==\n{}\n== PCI ==\n{}\n== USB ==\n{}\n== Log ==\n",
        crate::VERSION,
        cpu(),
        pci(),
        usb()
    );
    text.push_str(&String::from_utf8_lossy(&log()));
    match crate::fs::write_all("/Boot/aurora/boot.log", text.as_bytes()) {
        Ok(()) => log!("boot", "boot log saved to /Boot/aurora/boot.log"),
        Err(e) => log!("boot", "could not save the boot log: {}", aurora_abi::err::name(e)),
    }
}
