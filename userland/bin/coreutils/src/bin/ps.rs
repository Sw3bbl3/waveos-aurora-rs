//! ps — list processes and kernel tasks.
#![no_std]
#![no_main]
extern crate alloc;
use aurora::abi;
use aurora::println;
aurora::entry!(main);

fn main(_: aurora::Args) -> i32 {
    println!("  PID  PPID  STATE     CPU(ms)    MEM  NAME");
    let mut rows = aurora::process::list();
    rows.sort_by_key(|p| (p.user, p.pid));
    for p in rows.iter().filter(|p| p.state != abi::PROC_EXITED) {
        let state = match p.state {
            abi::PROC_RUNNING => "running",
            abi::PROC_READY => "ready",
            abi::PROC_SLEEPING => "sleeping",
            _ => "exited",
        };
        let mem = if p.user == 1 { alloc::format!("{}K", p.mem_kib) } else { alloc::string::String::from("-") };
        let name = if p.user == 1 { alloc::string::String::from(p.name()) } else { alloc::format!("[{}]", p.name()) };
        let pid = if p.user == 1 { alloc::format!("{}", p.pid) } else { alloc::string::String::from("k") };
        println!("{:>5} {:>5}  {:<9} {:>7} {:>6}  {}", pid, p.parent, state, p.cpu_ms, mem, name);
    }
    0
}
