//! Opt-in live Windows freeze measurements using local, deterministic workloads.
//! Run with CRYO_FREEZE_CASE=light|heap|shared and cargo test live_freeze_memory -- --ignored --nocapture.
//! Private bytes are process commitments, not physical RAM or guaranteed reclaimable tab bytes.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use raw_window_handle::HasWindowHandle;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::platform::windows::EventLoopBuilderExtWindows;
use tao::window::WindowBuilder;

use crate::engine::accounting::MemorySnapshot;
use crate::engine::wry_impl::WryEngine;
use crate::engine::Engine;
use crate::tabs::{SystemClock, TabId, TabState};

use super::{App, AppEvent, Profile};

struct Probe {
    case: String,
    tabs: Vec<TabId>,
    phase: usize,
    due: Instant,
    pending: Option<Receiver<MemorySnapshot>>,
    /// Phase-0 private total — renderers are shared across views, so a
    /// tab's cost is only ever a process delta against this baseline.
    baseline_private: Option<u64>,
}

impl Probe {
    fn tick(&mut self, app: &mut App) -> anyhow::Result<bool> {
        let phases = [
            "before",
            "idle_control",
            "freeze_one_1s",
            "freeze_one_5s",
            "freeze_all_5s",
        ];
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(snapshot) => {
                    let processes: Vec<_> = snapshot
                        .processes()
                        .iter()
                        .map(|p| {
                            serde_json::json!({
                                "pid": p.id().0, "kind": format!("{:?}", p.kind()),
                                "private_bytes": p.stats().ok().map(|s| s.private_bytes()),
                                "working_set_bytes": p.stats().ok().map(|s| s.working_set_bytes()),
                                "error": p.stats().err(),
                            })
                        })
                        .collect();
                    anyhow::ensure!(
                        !processes.is_empty(),
                        "WebView2 enumeration returned no processes"
                    );
                    anyhow::ensure!(
                        snapshot.processes().iter().all(|p| p.stats().is_ok()),
                        "incomplete memory sample"
                    );
                    let expected_frozen = match self.phase {
                        0 | 1 => 0,
                        2 | 3 => 1,
                        _ => self.tabs.len(),
                    };
                    anyhow::ensure!(
                        self.tabs
                            .iter()
                            .filter(|id| app
                                .registry
                                .get(**id)
                                .is_some_and(|t| t.state() == TabState::Frozen))
                            .count()
                            == expected_frozen,
                        "measured tabs changed state in phase {}: {:?}",
                        phases[self.phase],
                        self.tabs
                            .iter()
                            .map(|id| app.registry.get(*id).map(|t| t.state()))
                            .collect::<Vec<_>>()
                    );
                    let private = snapshot
                        .processes()
                        .iter()
                        .map(|p| p.stats().expect("sample is complete").private_bytes())
                        .sum::<u64>();
                    let baseline = *self.baseline_private.get_or_insert(private);
                    crate::report!(
                        "FREEZE_PROBE {}",
                        serde_json::json!({
                            "case": self.case, "phase": phases[self.phase], "processes": processes,
                            "private_mib": private as f64 / 1048576.0,
                            "delta_from_baseline_mib": (private as i64 - baseline as i64) as f64 / 1048576.0,
                            "renderer_private_mib": snapshot.private_bytes(crate::engine::accounting::ProcessKind::Renderer).map(|n| n as f64 / 1048576.0),
                            "frozen_tabs": self.tabs.iter().filter(|id| app.registry.get(**id).is_some_and(|t| t.state() == TabState::Frozen)).count(),
                        })
                    );
                    self.pending = None;
                    self.phase += 1;
                    match self.phase {
                        1 => self.due = Instant::now() + Duration::from_secs(3),
                        2 => {
                            app.freeze_tab(self.tabs[0])?;
                            self.due = Instant::now() + Duration::from_secs(1);
                        }
                        3 => self.due = Instant::now() + Duration::from_secs(4),
                        4 => {
                            for id in self.tabs.iter().skip(1) {
                                app.freeze_tab(*id)?;
                            }
                            self.due = Instant::now() + Duration::from_secs(5);
                        }
                        _ => return Ok(true),
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(e) => return Err(e.into()),
            }
            return Ok(false);
        }
        if Instant::now() < self.due {
            return Ok(false);
        }
        if self.phase == 0
            && !self.tabs.iter().all(|id| {
                app.registry
                    .get(*id)
                    .is_some_and(|tab| tab.title() == "probe-ready" && !tab.is_loading())
            })
        {
            self.due = Instant::now() + Duration::from_secs(3);
            return Ok(false);
        }
        let (tx, rx) = channel();
        app.engine.measure_memory(Box::new(move |snapshot| {
            if tx.send(snapshot).is_err() {
                crate::report!("freeze probe receiver closed");
            }
        }))?;
        self.pending = Some(rx);
        Ok(false)
    }
}

#[test]
#[ignore = "opens a live WebView2 window and measures local workloads"]
fn live_freeze_memory() -> anyhow::Result<()> {
    let case = std::env::var("CRYO_FREEZE_CASE").unwrap_or_else(|_| "heap".into());
    anyhow::ensure!(
        ["light", "heap", "shared"].contains(&case.as_str()),
        "unknown probe case"
    );
    let (mut event_loop, mut app, tabs) = fixture_app(&case)?;
    let mut probe = Probe {
        case,
        tabs,
        phase: 0,
        due: Instant::now() + Duration::from_secs(3),
        pending: None,
        baseline_private: None,
    };
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut result = Ok(());
    event_loop.run_return(|event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(50));
        if matches!(
            event,
            Event::NewEvents(StartCause::ResumeTimeReached { .. })
        ) {
            match probe.tick(&mut app) {
                Ok(true) => {
                    *control_flow = ControlFlow::Exit;
                    return;
                }
                Err(e) => {
                    result = Err(e);
                    *control_flow = ControlFlow::Exit;
                    return;
                }
                _ => {}
            }
        }
        if Instant::now() >= deadline {
            result = Err(anyhow::anyhow!("freeze probe timed out"));
            *control_flow = ControlFlow::Exit;
            return;
        }
        app.handle_event(event, control_flow);
    });
    anyhow::ensure!(
        probe.phase == 5 || result.is_err(),
        "probe window closed before measurement completed"
    );
    result
}

fn fixture_app(
    case: &str,
) -> anyhow::Result<(tao::event_loop::EventLoop<AppEvent>, App, Vec<TabId>)> {
    let url = fixture(case)?;
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event()
        .with_any_thread(true)
        .build();
    let window = Rc::new(
        WindowBuilder::new()
            .with_title("cryo freeze probe")
            .build(&event_loop)?,
    );
    let engine: Rc<dyn Engine> = Rc::new(WryEngine::new(
        window.window_handle()?.as_raw(),
        crate::shell::pages::internal_page,
        true,
    ));
    let mut app = App::new(
        window,
        engine,
        Rc::new(SystemClock),
        event_loop.create_proxy(),
        None,
        Profile::session_only(),
    )?;
    let anchor = app.registry.active_id().expect("initial tab is active");
    let mut tabs = Vec::new();
    for _ in 0..if case == "shared" { 2 } else { 1 } {
        app.spawn_tab(&url, true)?;
        tabs.push(app.registry.active_id().expect("spawned tab is active"));
    }
    app.activate(anchor)?;
    Ok((event_loop, app, tabs))
}

#[test]
#[ignore = "opens a live WebView2 window and exercises automatic RAM enforcement"]
fn live_ram_budget() -> anyhow::Result<()> {
    let (mut event_loop, mut app, tabs) = fixture_app("shared")?;
    let anchor = app.registry.active_id().expect("anchor is active");
    pump_until(&mut event_loop, &mut app, |app| {
        Ok(tabs.iter().all(|id| {
            app.registry
                .get(*id)
                .is_some_and(|t| t.title() == "probe-ready" && !t.is_loading())
        }))
    })?;
    for id in &tabs {
        app.registry
            .get_mut(*id)
            .unwrap()
            .note_unfocused(Instant::now() - Duration::from_secs(121));
    }
    app.on_page_message(
        tabs[1],
        crate::engine::PageMessage {
            source: app.registry.get(tabs[1]).unwrap().url().to_string(),
            body: r#"{"kind":"edited"}"#.into(),
        },
    )?;
    anyhow::ensure!(
        app.registry
            .get(tabs[1])
            .unwrap()
            .resources()
            .automatic_freeze_blocked,
        "unsaved-edit report did not protect work"
    );
    app.set_memory_budget(crate::memory::ByteBudget::from_mib(128)?);
    pump_until(&mut event_loop, &mut app, |app| {
        anyhow::ensure!(
            app.registry.active_id() == Some(anchor),
            "foreground tab changed"
        );
        anyhow::ensure!(
            app.registry.get(tabs[1]).unwrap().state() == TabState::Active,
            "protected work was frozen"
        );
        Ok(app.registry.get(tabs[0]).unwrap().state() == TabState::Frozen)
    })?;
    crate::report!("RAM_BUDGET_PROBE automatic freeze preserved foreground and protected work");
    let frozen_at = Instant::now();
    pump_until(&mut event_loop, &mut app, |app| {
        anyhow::ensure!(
            app.registry.active_id() == Some(anchor),
            "foreground tab changed"
        );
        anyhow::ensure!(
            app.registry.get(tabs[0]).unwrap().state() == TabState::Frozen,
            "frozen tab unexpectedly restored"
        );
        anyhow::ensure!(
            app.registry.get(tabs[1]).unwrap().state() == TabState::Active,
            "protected work was frozen"
        );
        Ok(frozen_at.elapsed() >= Duration::from_secs(20))
    })?;
    crate::report!("RAM_BUDGET_PROBE protected work remained live under continuing pressure");
    // Restore through the real spawn path — wry's synchronous controller
    // creation pumps while a tao handler is on the stack, so this is also
    // the re-entrancy regression check (see `engine::wry_impl::pump`).
    app.activate(tabs[0])?;
    pump_until(&mut event_loop, &mut app, |app| {
        anyhow::ensure!(
            app.registry.get(tabs[1]).unwrap().state() == TabState::Active,
            "protected work was frozen"
        );
        let tab = app.registry.get(tabs[0]).unwrap();
        Ok(tab.state() == TabState::Active && tab.title() == "probe-ready" && !tab.is_loading())
    })?;
    crate::report!("RAM_BUDGET_PROBE live restore rehydrated the frozen tab");
    Ok(())
}

fn pump_until(
    event_loop: &mut tao::event_loop::EventLoop<AppEvent>,
    app: &mut App,
    mut predicate: impl FnMut(&mut App) -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut complete = false;
    let mut result = Ok(());
    event_loop.run_return(|event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(50));
        app.resource_tick();
        match predicate(app) {
            Ok(true) => {
                complete = true;
                *control_flow = ControlFlow::Exit;
            }
            Err(e) => {
                result = Err(e);
                *control_flow = ControlFlow::Exit;
            }
            Ok(false) if Instant::now() >= deadline => {
                result = Err(anyhow::anyhow!("automatic RAM enforcement timed out"));
                *control_flow = ControlFlow::Exit;
            }
            Ok(false) => app.handle_event(event, control_flow),
        }
    });
    result?;
    anyhow::ensure!(complete, "probe window closed before enforcement completed");
    Ok(())
}

fn fixture(case: &str) -> anyhow::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}/", listener.local_addr()?);
    let allocation = if case == "light" {
        ""
    } else {
        "window.retained = new Uint8Array(64 * 1024 * 1024); window.retained.fill(123);"
    };
    let page = format!("<!doctype html><title>loading</title><h1>Freeze memory probe</h1><script>{allocation}document.title='probe-ready';</script>");
    std::thread::Builder::new().name("freeze-fixture".into()).spawn(move || {
        for connection in listener.incoming() {
            let outcome = (|| -> std::io::Result<()> {
                let mut stream = connection?;
                stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                let mut request = [0; 4096];
                let mut used = 0;
                while used < request.len() && !request[..used].windows(4).any(|w| w == b"\r\n\r\n") {
                    let received = stream.read(&mut request[used..])?;
                    if received == 0 {
                        return Ok(());
                    }
                    used += received;
                }
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", page.len(), page)
            })();
            if let Err(e) = outcome {
                crate::report!("freeze fixture: {e}");
            }
        }
    })?;
    Ok(url)
}
