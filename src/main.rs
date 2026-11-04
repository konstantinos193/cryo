//! Binary entry point — a shell, not a program (AGENTS §1): parse the
//! CLI flags and hand the thread to `cryo::run()`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    // `--incognito` flips the profile; `--new-window` marks a window a
    // running cryo spawned on purpose — without it a bare address arg is
    // handed to the existing window instead of opening one here. A bare
    // arg is an address to open (spawned windows get one via
    // Ctrl+Shift+Enter, external links via the OS). `--warm [pid]` is not
    // a window at all: the keeper a closing window leaves behind so the
    // next launch finds the browser process already running, told which
    // old browser process to outwait first.
    let mut incognito = false;
    let mut new_window = false;
    let mut open_url = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--warm" => return cryo::run_warm(args.next().and_then(|pid| pid.parse().ok())),
            "--incognito" => incognito = true,
            "--new-window" => new_window = true,
            _ => open_url = Some(arg),
        }
    }
    cryo::run(incognito, new_window, open_url)
}
