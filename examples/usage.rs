use std::env;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use pgpu::usage::{Meter, card, sample};

fn main() -> ExitCode {
    let pids: Option<Vec<u32>> = env::args().skip(1).map(|arg| arg.parse().ok()).collect();
    let Some(pids) = pids.filter(|pids| !pids.is_empty()) else {
        eprintln!("usage: usage <pid>...");
        return ExitCode::from(2);
    };
    let mut meter = Meter::new();
    meter.push(sample(&pids), Instant::now());
    thread::sleep(Duration::from_secs(1));
    let again = sample(&pids);
    let vram = again.vram_kib();
    let Some(rate) = meter.push(again, Instant::now()) else {
        eprintln!("usage: no rate");
        return ExitCode::from(1);
    };
    println!("processes {}", pids.len());
    for (name, frac) in &rate.engines {
        println!("{name} {:.1}%", frac * 100.0);
    }
    println!("combined {:.1}%", rate.combined * 100.0);
    println!("vram {vram} KiB");
    match card() {
        Some(mem) => println!("card {} / {} bytes", mem.used, mem.total),
        None => println!("card unknown"),
    }
    ExitCode::SUCCESS
}
