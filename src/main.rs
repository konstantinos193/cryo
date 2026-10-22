#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    // `--incognito` flips the profile; a bare arg is an address to open
    // (spawned windows get one via Ctrl+Shift+Enter).
    let mut incognito = false;
    let mut open_url = None;
    for arg in std::env::args().skip(1) {
        if arg == "--incognito" {
            incognito = true;
        } else {
            open_url = Some(arg);
        }
    }
    cryo::run(incognito, open_url)
}
