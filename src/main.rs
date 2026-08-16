mod app;
mod bench;
mod collect;
mod smart;
mod ui;

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event};
use ratatui::crossterm::execute;

use app::{App, AppEvent};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("bench") {
        return bench::cli::parse_args(&args[1..])
            .and_then(|cfg| bench::cli::run_headless(&cfg))
            .map_err(|e| anyhow::anyhow!(e));
    }

    let (tx, rx) = mpsc::channel();

    let collector_tx = tx.clone();
    let tx_bench = tx.clone();
    thread::spawn(move || {
        let mut collector = collect::new_collector();
        loop {
            let ev = match collector.collect() {
                Ok(s) => AppEvent::Snapshot(Box::new(s)),
                Err(e) => AppEvent::CollectError(e.to_string()),
            };
            if collector_tx.send(ev).is_err() {
                return; // app is gone
            }
            thread::sleep(Duration::from_secs(1));
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
                Err(_) => return,
            }
        }
    });

    let mut terminal = ratatui::init();
    // wheel and click go to us, not the terminal scrollback
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let res = run(&mut terminal, rx, tx_bench);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    res
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    rx: mpsc::Receiver<AppEvent>,
    tx_bench: mpsc::Sender<AppEvent>,
) -> Result<()> {
    let mut app = App::default();
    while !app.quit {
        terminal.draw(|f| ui::draw(f, &app))?;
        match rx.recv() {
            Ok(AppEvent::Mouse(m)) => {
                let size = terminal.size()?;
                let frame = ratatui::layout::Rect::new(0, 0, size.width, size.height);
                ui::handle_mouse(&mut app, m, frame);
            }
            Ok(ev) => app.on_event(ev),
            Err(_) => break,
        }
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
