#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| {
    if data.len() > 16384 { return; }
    let _ = opsssh_net_watch::parse_vpn_line(data);
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = opsssh_ssh_config::parse_command(text);
        let _ = opsssh_drop::quote(text, opsssh_drop::Shell::Posix);
        let _ = opsssh_drop::quote(text, opsssh_drop::Shell::PowerShell);
        let _ = opsssh_drop::join_remote("/fixture", text);
    }
});
