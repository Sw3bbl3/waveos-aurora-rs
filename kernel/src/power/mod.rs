//! Shutdown, restart, sleep ([`s3`]), CPU idle states ([`idle`]),
//! frequency and the energy mode ([`cpufreq`]), and the QEMU test-exit device.

pub mod cpufreq;
pub mod idle;
pub mod s3;

use crate::acpi::AcpiInfo;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Once;
use x86_64::instructions::port::Port;

static ACPI: Once<AcpiInfo> = Once::new();

pub fn init(info: AcpiInfo) {
    ACPI.call_once(|| info);
    idle::init();
    cpufreq::init();
}

/// Bumped when every CPU must change its own registers (frequency policy,
/// timer mode); each applies it in its idle loop.
static GENERATION: AtomicU64 = AtomicU64::new(1);

/// Asks every CPU to apply the current power settings to itself, and wakes
/// the idle ones so they do it now (busy ones do it when they next idle).
pub fn request_local_update() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    let me = crate::arch::percpu::cpu_id();
    for c in crate::arch::percpu::online().filter(|&c| c != me) {
        if crate::arch::percpu::CPUS[c].idle.load(Ordering::Relaxed) {
            idle::set_need_resched(c);
            crate::arch::apic::send_ipi(
                crate::arch::percpu::CPUS[c].lapic_id.load(Ordering::Relaxed),
                crate::arch::idt::RESCHEDULE_VECTOR,
            );
        }
    }
}

/// From the idle loop (interrupts off): this CPU's frequency governor, and
/// settings it has not applied yet.
pub(crate) fn apply_local_updates(cpu: usize) {
    cpufreq::on_idle(cpu);
    let g = GENERATION.load(Ordering::Acquire);
    let c = &idle::CPUS[cpu];
    if c.applied.swap(g, Ordering::AcqRel) != g {
        if crate::arch::timer::local_mode() != crate::arch::timer::mode() {
            crate::arch::timer::start_local();
            crate::timer::reprogram();
        }
        cpufreq::apply_local();
    }
}

/// A CPU (re)started (boot, or after sleep): it applies the settings anew,
/// since its registers were reset.
pub fn cpu_started(cpu: usize) {
    if let Some(c) = idle::CPUS.get(cpu) {
        c.applied.store(0, Ordering::Release);
    }
}

/// After the file systems and the ACPI task are up: boot options, and what
/// the firmware says about the processors (`_CST`, `_PSS`), on a short-lived
/// task (AML can take a while).
pub fn start() {
    crate::sched::spawn("power-setup", || {
        if let Some(timer) = crate::gui::prefs::boot_option("timer") {
            crate::arch::timer::choose(Some(&timer));
            log!("power", "timer: {} (timer={} in boot.conf)", crate::arch::timer::describe(), timer);
            request_local_update();
        }
        if let Some(v) = crate::gui::prefs::boot_option("idle") {
            idle::configure(Some(&v));
        }
        crate::acpi::processor::configure();
        if let Some(t) = idle::table() {
            let names: alloc::vec::Vec<&str> = t.states.iter().map(|s| s.name.as_str()).collect();
            log!("power", "idle states ({}): {}", t.source, names.join(", "));
        }
        log!("power", "frequency: {}; energy mode {}", cpufreq::describe(), cpufreq::mode().name());
    });
}

/// The `energy_mode` preference changed (or was read at start).
pub fn set_energy_mode(value: &str) {
    if let Some(m) = cpufreq::Mode::parse(value) {
        cpufreq::set_mode(m);
    }
}

/// From the ACPI task: the battery and adapter state.
pub fn power_source(info: &aurora_abi::PowerInfo) {
    use aurora_abi::power::*;
    let f = info.flags;
    let unplugged = if f & AC_PRESENT != 0 { f & AC_ONLINE == 0 } else { f & DISCHARGING != 0 };
    cpufreq::set_on_battery(f & BATTERY != 0 && unplugged);
}

/// The power section of `cpuinfo`.
pub fn report(s: &mut alloc::string::String) {
    use core::fmt::Write;
    let _ = writeln!(s, "Timer:      {}", crate::arch::timer::describe());
    let arat = if crate::arch::timer::always_running() {
        "keeps running in deep C-states (ARAT)"
    } else {
        "may stop in deep C-states (no ARAT)"
    };
    let _ = writeln!(s, "            the LAPIC timer {arat}");
    if let Some(t) = idle::table() {
        let names: alloc::vec::Vec<&str> = t.states.iter().map(|s| s.name.as_str()).collect();
        let _ = writeln!(s, "Idle:       {} ({})", names.join(", "), t.source);
    }
    let _ = writeln!(s, "Frequency:  {}", cpufreq::describe());
    let _ = writeln!(
        s,
        "Energy:     {}{}",
        cpufreq::mode().name(),
        if cpufreq::on_battery() { ", on battery" } else { "" }
    );
    for cpu in crate::arch::percpu::online() {
        let st = &crate::timer::STATS[cpu];
        let (ipis, avoided) = idle::ipi_counts(cpu);
        let _ = write!(
            s,
            "CPU {:<2}      {} timer interrupts ({} slice ends, {} empty; timer set {} times, {} pending), {} wakeup IPIs ({} avoided)",
            cpu,
            st.interrupts.load(Ordering::Relaxed),
            st.slices.load(Ordering::Relaxed),
            st.empty.load(Ordering::Relaxed),
            crate::arch::timer::writes(cpu),
            crate::timer::pending(cpu),
            ipis,
            avoided
        );
        if let Some(mhz) = cpufreq::current_mhz(cpu).or(cpufreq::pstate_mhz(cpu)) {
            let _ = write!(s, ", ~{mhz} MHz");
        }
        let _ = writeln!(s);
        let res = idle::residency(cpu);
        let total: u64 = res.iter().map(|r| r.ns).sum::<u64>().max(1);
        let _ = write!(s, "            idle:");
        for r in res.iter().filter(|r| r.entries > 0) {
            let _ = write!(s, " {} {}% ({}×)", r.name, r.ns * 100 / total, r.entries);
        }
        let _ = writeln!(s);
    }
    let wakers = crate::sched::timer_wakeups();
    if !wakers.is_empty() {
        let _ = write!(s, "Wakers:     ");
        for (i, (name, n)) in wakers.iter().take(8).enumerate() {
            let _ = write!(s, "{}{} {}", if i > 0 { ", " } else { "" }, name, n);
        }
        let _ = writeln!(s);
    }
}

/// The static ACPI facts found at boot.
pub fn info() -> Option<&'static AcpiInfo> {
    ACPI.get()
}

/// Puts the machine to sleep, from a task on the boot CPU (where the firmware
/// resumes); the desktop repaints when it wakes.
pub fn sleep() {
    if !s3::available() {
        crate::gui::notify::system("Sleep isn't available", "This computer's firmware doesn't offer sleep (S3).");
        return;
    }
    crate::sched::spawn_on_bsp("sleep", || match s3::suspend() {
        Ok(()) => crate::gui::server::command(crate::gui::server::Command::Resumed),
        Err(e) => {
            log!("power", "sleep failed: {}", e);
            crate::gui::notify::system("Couldn't sleep", e);
            crate::gui::server::command(crate::gui::server::Command::Resumed);
        }
    });
}

pub fn shutdown() -> ! {
    log!("power", "shutting down");
    x86_64::instructions::interrupts::disable();
    if let Some(a) = ACPI.get() {
        if a.pm1a_cnt != 0 {
            unsafe {
                let mut cnt = Port::<u16>::new(a.pm1a_cnt);
                // Switch to ACPI mode first if firmware left us in legacy mode.
                if cnt.read() & 1 == 0 && a.smi_cmd != 0 && a.acpi_enable != 0 {
                    Port::<u8>::new(a.smi_cmd).write(a.acpi_enable);
                    for _ in 0..1_000_000 {
                        if cnt.read() & 1 != 0 {
                            break;
                        }
                    }
                }
                // \_S5 from AML when the runtime is up, else from the DSDT scan.
                let (typa, typb) = crate::acpi::sleep_type(5).unwrap_or((a.slp_typa, a.slp_typb));
                cnt.write((typa << 10) | (1 << 13));
                if a.pm1b_cnt != 0 {
                    Port::<u16>::new(a.pm1b_cnt).write((typb << 10) | (1 << 13));
                }
            }
        }
    }
    unsafe {
        // Emulator fallbacks: QEMU (older), Bochs, VirtualBox.
        Port::<u16>::new(0x604).write(0x2000);
        Port::<u16>::new(0xB004).write(0x2000);
        Port::<u16>::new(0x4004).write(0x3400);
    }
    crate::arch::cpu::halt_forever();
}

pub fn reboot() -> ! {
    log!("power", "restarting");
    x86_64::instructions::interrupts::disable();
    unsafe {
        if let Some((port, value)) = ACPI.get().and_then(|a| a.reset_port) {
            Port::<u8>::new(port).write(value);
        }
        Port::<u8>::new(0xCF9).write(0x06);
        // Keyboard controller pulse reset line.
        for _ in 0..10_000 {
            if Port::<u8>::new(0x64).read() & 0x02 == 0 {
                break;
            }
        }
        Port::<u8>::new(0x64).write(0xFE);
        // Last resort: triple fault with an empty IDT.
        let idt = x86_64::structures::DescriptorTablePointer { limit: 0, base: x86_64::VirtAddr::new(0) };
        x86_64::instructions::tables::lidt(&idt);
        core::arch::asm!("int3");
    }
    crate::arch::cpu::halt_forever();
}

/// Exits QEMU via the isa-debug-exit device (status = (code << 1) | 1).
#[allow(dead_code)]
pub fn qemu_exit(code: u32) -> ! {
    unsafe { Port::<u32>::new(0xF4).write(code) };
    crate::arch::cpu::halt_forever();
}
