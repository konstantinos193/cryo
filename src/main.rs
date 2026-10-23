//! Binary entry point — a shell, not a program (AGENTS §1): parse the
//! CLI flags and hand the thread to `cryo::run()`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    // `--incognito` flips the profile; `--new-window` marks a window a
    // running cryo spawned on purpose — without it a bare address arg is
    // handed to the existing window instead of opening one here. A bare
    // arg is an address to open (spawned windows get one via
    // Ctrl+Shift+Enter, external links via the OS).
    let mut incognito = false;
    let mut new_window = false;
    let mut open_url = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--incognito" => incognito = true,
            "--new-window" => new_window = true,
            _ => open_url = Some(arg),
        }
    }
    cryo::run(incognito, new_window, open_url)
}
