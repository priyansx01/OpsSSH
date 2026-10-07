//! GPU workspace and headless development tools for the local renderer spike.

mod connections;
#[cfg(feature = "gui")]
mod files;
#[cfg(feature = "gui")]
mod gui;
mod localization;

use std::hint::black_box;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use opsssh_term_core::{
    AlacrittyTerminal, InputModes, Key, KeyEventKind, KittyMode, Modifiers, SessionState,
    TerminalBackend, TerminalSize, encode_key,
};
use opsssh_term_view::{FrameDamage, FrameScheduler};

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("opsssh: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    match arguments.as_slice() {
        [] => {
            launch(false);
        }
        [command] if command == "local" => launch(true),
        #[cfg(feature = "capture")]
        [command, path] if command == "snapshot" || command == "snapshot-local" => {
            gui::capture(command == "snapshot-local", path.into())
        }
        [command] if command == "--help" || command == "-h" => help(),
        [command] if command == "diagnostics" => diagnostics(),
        [command] if command == "bench" => benchmark(100_000)?,
        [command] if command == "bench-parser" => benchmark_parser(64)?,
        [command, count] if command == "bench-parser" => {
            let mebibytes: u32 = count.parse()?;
            if !(1..=1024).contains(&mebibytes) {
                return Err("parser workload must be between 1 and 1024 MiB".into());
            }
            benchmark_parser(mebibytes)?;
        }
        [command, count] if command == "bench" => {
            let iterations: u32 = count.parse()?;
            if !(1..=10_000_000).contains(&iterations) {
                return Err("iterations must be between 1 and 10000000".into());
            }
            benchmark(iterations)?;
        }
        _ => return Err("unknown command; use --help".into()),
    }
    Ok(())
}

fn help() {
    println!(
        "Usage: opsssh [local | diagnostics | bench [iterations] | bench-parser [MiB] | --help]"
    );
    println!("bench emits JSON for input encoding and frame scheduling, not GPU/VT performance.");
}

fn benchmark_parser(mebibytes: u32) -> Result<(), Box<dyn std::error::Error>> {
    let mut terminal = AlacrittyTerminal::with_scrollback(TerminalSize::new(120, 40)?, 0);
    let line = b"\x1b[38;2;136;208;221mOpsSSH parser replay\x1b[0m: abcdefghijklmnopqrstuvwxyz 0123456789\r\n";
    let chunk = line.repeat((1024 * 1024) / line.len());
    let started = Instant::now();
    for _ in 0..mebibytes {
        black_box(terminal.ingest(&chunk)?);
    }
    let elapsed = started.elapsed();
    let bytes = chunk.len() as u64 * u64::from(mebibytes);
    let platform = opsssh_platform::info();
    println!(
        "{{\"schema\":1,\"workload\":\"ansi-truecolor-lines-v1\",\"os\":\"{}\",\"architecture\":\"{}\",\"bytes\":{},\"elapsed_ns\":{},\"mib_per_second\":{:.2},\"scrollback_lines\":0,\"gpu_measured\":false,\"vt_parser_measured\":true}}",
        platform.os,
        platform.architecture,
        bytes,
        elapsed.as_nanos(),
        bytes as f64 / (1024. * 1024.) / elapsed.as_secs_f64()
    );
    Ok(())
}

fn diagnostics() {
    let platform = opsssh_platform::info();
    // No environment variables, local paths, server names, or credentials.
    println!(
        "{{\"app\":\"OpsSSH\",\"version\":\"{}\",\"os\":\"{}\",\"architecture\":\"{}\",\"stage\":\"local-renderer-spike\",\"renderer_compiled\":{},\"renderer_accepted\":false,\"ssh_ready\":false}}",
        env!("CARGO_PKG_VERSION"),
        platform.os,
        platform.architecture,
        cfg!(feature = "gui")
    );
}

fn launch(local: bool) {
    #[cfg(feature = "gui")]
    gui::run(local);
    #[cfg(not(feature = "gui"))]
    {
        let _ = local;
        eprintln!("GUI disabled; build with the default features to open a window.");
    }
}

fn benchmark(iterations: u32) -> Result<(), Box<dyn std::error::Error>> {
    let modes = InputModes {
        kitty: KittyMode {
            disambiguate: true,
            ..KittyMode::default()
        },
        ..InputModes::default()
    };
    let modifiers = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let started = Instant::now();
    let mut bytes = 0;
    for _ in 0..iterations {
        bytes += black_box(encode_key(
            SessionState::Connected,
            modes,
            Key::Enter,
            modifiers,
            KeyEventKind::Press,
        )?)
        .len();
    }
    let input_elapsed = started.elapsed();
    let started = Instant::now();
    let mut scheduler = FrameScheduler::new(120)?;
    for index in 0..iterations {
        scheduler.request(FrameDamage {
            rows: [index as usize % 24].into(),
            ..FrameDamage::default()
        });
        black_box(scheduler.take_frame(Duration::from_micros(u64::from(index) * 100)));
    }
    let frame_elapsed = started.elapsed();
    let platform = opsssh_platform::info();
    println!(
        "{{\"schema\":1,\"workload\":\"input-and-frame-scheduler\",\"os\":\"{}\",\"architecture\":\"{}\",\"iterations\":{},\"encoded_bytes\":{},\"input_ns\":{},\"scheduler_ns\":{},\"scheduled_frames\":{},\"gpu_measured\":false,\"vt_parser_measured\":false}}",
        platform.os,
        platform.architecture,
        iterations,
        bytes,
        input_elapsed.as_nanos(),
        frame_elapsed.as_nanos(),
        scheduler.stats().presented
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_benchmark_workloads_fail_without_work() {
        assert!(run(vec!["bench".into(), "0".into()]).is_err());
        assert!(run(vec!["bench".into(), "10000001".into()]).is_err());
        assert!(run(vec!["bench".into(), "oops".into()]).is_err());
        assert!(run(vec!["connect".into()]).is_err());
        assert!(run(vec!["bench-parser".into(), "0".into()]).is_err());
        assert!(run(vec!["bench-parser".into(), "1025".into()]).is_err());
    }
}
