mod app;
mod bench;
mod collect;
mod fetch;
mod smart;
mod ui;
mod update;

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use ratatui::crossterm::execute;

use app::{App, AppEvent};

const USAGE: &str = "\
rmon - terminal system monitor with disk benchmarking

usage:
    rmon                            run the monitor
    rmon bench [OPTIONS]            headless disk benchmark
    rmon update                     update to the latest release
    rmon --version                  print version
    rmon --help                     print this help

bench options:
    --path DIR                      benchmark a filesystem path
    --device DEV                    read-only raw device test (root)
    --size-mb N                     size of the test file
    --secs N                        seconds per random test

keys:
    q           quit
    c/m/i/n     sort procs
    f or /      filter procs
    t           threads of selected process
    e           process tree
    h           show idle interfaces/disks
    k           kill selected process
    s           system info
    + / -       refresh speed
    b           disk benchmark";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("bench") => {
            return bench::cli::parse_args(&args[1..])
                .and_then(|cfg| bench::cli::run_headless(&cfg))
                .map_err(|e| anyhow::anyhow!(e));
        }
        Some("update") => {
            return update::run().map_err(|e| anyhow::anyhow!(e));
        }
        Some("-V" | "--version" | "version") => {
            println!("rmon {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            return Ok(());
        }
        // an unknown arg used to silently open the tui; say so instead
        Some(other) => {
            anyhow::bail!("unknown argument `{other}`\n\n{USAGE}");
        }
        None => {}
    }

    let (tx, rx) = mpsc::channel();

    let collector_tx = tx.clone();
    let tx_bench = tx.clone();
    // the collector thread only pays for per-thread stats for the one pid
    // the ui has selected; -1 = t toggle off. ui stores it after every event
    let thread_pid = Arc::new(AtomicI64::new(-1));
    let collect_pid = thread_pid.clone();
    // refresh cadence in ms; +/- in the ui writes it, the collector reads
    // it fresh each tick so changes land without waking anything
    let update_ms = Arc::new(AtomicU64::new(1000));
    let collect_ms = update_ms.clone();
    thread::spawn(move || {
        let mut collector = collect::new_collector();
        loop {
            let v = collect_pid.load(Relaxed);
            let ev = match collector.collect((v >= 0).then_some(v as i32)) {
                Ok(s) => AppEvent::Snapshot(Box::new(s)),
                Err(e) => AppEvent::CollectError(e.to_string()),
            };
            if collector_tx.send(ev).is_err() {
                return; // app is gone
            }
            thread::sleep(Duration::from_millis(collect_ms.load(Relaxed)));
        }
    });

    // SMART data is slow (shells out to smartctl) and near-static: refresh every
    // 60s on its own thread. No smartctl on this box -> no thread, empty panel.
    if let Some(smartctl) = smart::find_smartctl() {
        let smart_tx = tx.clone();
        thread::Builder::new()
            .name("smart".into())
            .spawn(move || {
                loop {
                    let v = smart::collect(&smartctl);
                    if smart_tx.send(AppEvent::Smart(v)).is_err() {
                        return; // app is gone
                    }
                    thread::sleep(Duration::from_secs(60));
                }
            })
            .ok(); // spawn failure just leaves the panel empty
    }

    thread::spawn(move || {
        loop {
            match event::read() {
                Ok(Event::Key(k)) => {
                    if tx.send(AppEvent::Key(k)).is_err() {
                        return;
                    }
                }
                Ok(Event::Mouse(m)) => {
                    if tx.send(AppEvent::Mouse(m)).is_err() {
                        return;
                    }
                }
                Ok(_) => {} // resize redraws on next event
                Err(_) => {
                    // the tty is gone (ssh drop, tmux kill); tell main to exit
                    // instead of drawing to a dead pty forever
                    let _ = tx.send(AppEvent::Quit);
                    return;
                }
            }
        }
    });

    let mut terminal = ratatui::init();
    // wheel and click go to us, not the terminal scrollback
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    // ratatui's panic hook restores raw mode and the alt screen but knows
    // nothing about mouse capture; chain our own disable in front of it
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        hook(info);
    }));
    let res = run(&mut terminal, rx, tx_bench, &thread_pid, &update_ms);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    res
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    rx: mpsc::Receiver<AppEvent>,
    tx_bench: mpsc::Sender<AppEvent>,
    thread_pid: &AtomicI64,
    update_ms: &AtomicU64,
) -> Result<()> {
    let mut app = App::default();
    while !app.quit {
        terminal.draw(|f| ui::draw(f, &mut app))?;
        match rx.recv() {
            Ok(AppEvent::Mouse(m)) => {
                let size = terminal.size()?;
                let frame = ratatui::layout::Rect::new(0, 0, size.width, size.height);
                ui::handle_mouse(&mut app, m, frame);
            }
            Ok(ev) => {
                app.on_event(ev);
                update_ms.store(app.refresh_ms(), Relaxed);
            }
            Err(_) => break,
        }
        // clicks move the selection too, so this runs for every event kind:
        // tell the collector which pid (if any) should pay for thread detail
        thread_pid.store(
            if app.show_threads {
                app.selected_id.map(|(p, _)| p as i64).unwrap_or(-1)
            } else {
                -1
            },
            Relaxed,
        );
        // the picker (b key) chose a target; unwritable dirs surface as a
        // clean bench error in the panel, so no probe needed here
        if let Some(target) = app.bench_target.take() {
            let bench_tx = tx_bench.clone();
            thread::spawn(move || {
                let cfg = bench::BenchConfig {
                    target_dir: target,
                    ..Default::default()
                };
                bench::run(&cfg, &mut |ev| {
                    let _ = bench_tx.send(AppEvent::Bench(ev));
                });
            });
        }
    }
    Ok(())
}
