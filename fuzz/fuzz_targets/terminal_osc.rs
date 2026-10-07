#![no_main]
use libfuzzer_sys::fuzz_target;
use opsssh_term_core::{AlacrittyTerminal, TerminalBackend, TerminalSize};
fuzz_target!(|data: &[u8]| {
    if data.len() > 65536 { return; }
    let mut terminal = AlacrittyTerminal::with_scrollback(TerminalSize::new(80,24).unwrap(), 100);
    for chunk in data.chunks(17) { let _ = terminal.ingest(chunk); }
    let _ = terminal.snapshot();
});
