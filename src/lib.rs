mod budget;
mod cache;
mod cli;
mod config;
mod fake;
mod gpu;
mod hour;
mod knobs;
mod outside;
mod pace;
mod paths;
mod procs;
mod queue;
mod report;
mod run;
mod seats;
mod spawn;
mod turn;
pub mod usage;
mod wake;
mod wrapper;

pub fn main() -> std::process::ExitCode {
    cli::main()
}
