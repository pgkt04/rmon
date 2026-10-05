//! the native backend: one `screen` surface (rmon is full screen, like the
//! alternate screen) built from the same App the ratatui backend draws.
//! spec: https://docs.stencil.so/tern/protocol/surfaces.html

mod view;

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicI64, AtomicU64};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::{cursor, execute, terminal};
use serde_json::{Value, json};

use crate::app::{App, AppEvent};
use crate::tsp::doc::{self, Node};
use crate::tsp::input::{self, Input, Parser};
use crate::tsp::probe::Probe;
use crate::tsp::wire::{self, Event, Hello, Incoming};
use view::SF;

/// cosmetic only, for terminals that take stylesheets: the capped net and
/// disk lists scroll instead of clipping, and a proc row's numbers keep their
/// width while the name truncates, like the ratatui table. the layout itself
/// never depends on it (view::plan)
const CSS: &str = "[data-role='rmon.scroll']{overflow-y:auto}\
    [data-id='proc'] .sf-item-value{max-width:none;flex-shrink:0}";
/// the net and dsk series colors of the ratatui theme
const PALETTE_DARK: [(&str, &str); 3] = [("rx", "#78c8ff"), ("tx", "#ffaa6e"), ("io", "#be8cff")];
const PALETTE_LIGHT: [(&str, &str); 3] = [("rx", "#1f6fb2"), ("tx", "#b8581a"), ("io", "#7a3fc4")];
/// how long leaving waits for the DA1 that follows the close
const DRAIN: Duration = Duration::from_millis(500);

pub fn run(
    probe: Probe,
    rx: Receiver<AppEvent>,
    tx: Sender<AppEvent>,
    thread_pid: &AtomicI64,
    update_ms: &AtomicU64,
) -> Result<()> {
    let Probe {
        hello,
        parser,
        early,
    } = probe;
    let mut cx = view::Ctx {
        cols: 80,
        rows: 24,
        palette: hello.has_feature("program-palette"),
    };
    execute!(
        std::io::stdout(),
        terminal::EnterAlternateScreen,
        cursor::Hide
    )?;
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut close = Vec::new();
        wire::encode(
            &mut close,
            b'x',
            &close_body().to_string(),
            usize::MAX,
            &mut 0,
        );
        let _ = std::io::stdout().write_all(&close);
        leave_terminal();
        hook(info);
    }));
    spawn_input(parser, early, tx.clone())?;
    let mut surface = Surface::new(hello);
    let res = session(&mut surface, &mut cx, &rx, &tx, thread_pid, update_ms);
    leave_terminal();
    res
}

fn leave_terminal() {
    let _ = execute!(
        std::io::stdout(),
        cursor::Show,
        terminal::LeaveAlternateScreen
    );
    let _ = terminal::disable_raw_mode();
}

fn close_body() -> Value {
    json!({"id": SF, "keep": false})
}

fn session(
    surface: &mut Surface,
    cx: &mut view::Ctx,
    rx: &Receiver<AppEvent>,
    tx: &Sender<AppEvent>,
    thread_pid: &AtomicI64,
    update_ms: &AtomicU64,
) -> Result<()> {
    surface.open()?;
    let mut app = App::default();
    let mut dirty = true;
    let mut tty_gone = false;
    loop {
        // credits pace the frames: while the window has not drawn the last
        // ones, changes pile up in App and go out as one diff after the ack
        if dirty && surface.ready() {
            // the pane's grid size; Tern keeps the pty at it under the surface
            if let Ok((cols, rows)) = terminal::size() {
                cx.cols = cols as usize;
                cx.rows = rows as usize;
            }
            let (doc, focus) = view::build(&app, cx);
            surface.frame(doc, focus)?;
            dirty = false;
        }
        let Ok(first) = rx.recv() else { break };
        let mut next = Some(first);
        while let Some(ev) = next {
            tty_gone |= matches!(ev, AppEvent::Quit);
            dirty |= handle(&mut app, surface, ev)?;
            next = rx.try_recv().ok();
        }
        crate::sync_workers(&mut app, thread_pid, update_ms, tx);
        if app.quit {
            break;
        }
    }
    if !tty_gone {
        leave(surface, rx)?;
    }
    Ok(())
}

/// true when the App changed and the document needs a new diff
fn handle(app: &mut App, surface: &mut Surface, ev: AppEvent) -> std::io::Result<bool> {
    let AppEvent::Tsp(ev) = ev else {
        app.on_event(ev);
        return Ok(true);
    };
    surface.record("in", None, || json!(format!("{ev:?}")));
    Ok(match ev {
        Event::Ack(s) => {
            surface.ack(s);
            false
        }
        // the size is read off the pty at the next build
        Event::Resize(_) => true,
        Event::Da1 => false,
        // the surface itself went (its screen was cleared): start over
        Event::Gone(ids) if ids.iter().any(|i| i == SF) => {
            surface.open()?;
            true
        }
        Event::Gone(_) => false,
        Event::Error(msg) => {
            app.status = Some(format!("tsp: {msg}"));
            true
        }
        ev => {
            view::on_event(app, ev);
            true
        }
    })
}

/// close first, then read until the DA1 written after the close comes back,
/// so no reply in flight reaches the shell as typed text
fn leave(surface: &mut Surface, rx: &Receiver<AppEvent>) -> std::io::Result<()> {
    surface.send(b'x', &close_body())?;
    let mut out = std::io::stdout().lock();
    out.write_all(b"\x1b[c")?;
    out.flush()?;
    let deadline = Instant::now() + DRAIN;
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(AppEvent::Tsp(Event::Da1)) | Ok(AppEvent::Quit) | Err(_) => return Ok(()),
            Ok(_) => {}
        }
    }
}

/// stdin belongs to this thread while native: keys become AppEvent::Key,
/// TSP strings become AppEvent::Tsp, and the tty going away quits
fn spawn_input(mut parser: Parser, early: Vec<Input>, tx: Sender<AppEvent>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("tsp-input".into())
        .spawn(move || {
            let mut pending = early;
            let mut buf = [0u8; 16 << 10];
            loop {
                for i in pending.drain(..) {
                    let ev = match i {
                        Input::Key(k) => AppEvent::Key(k),
                        Input::Da1 => AppEvent::Tsp(Event::Da1),
                        Input::Tsp(inner) => match wire::decode(&inner) {
                            Some(Incoming::Event(v)) => match Event::from_value(&v) {
                                Some(e) => AppEvent::Tsp(e),
                                None => continue,
                            },
                            _ => continue,
                        },
                    };
                    if tx.send(ev).is_err() {
                        return;
                    }
                }
                if parser.lone_esc() && !input::readable(input::ESC_WAIT_MS).unwrap_or(false) {
                    parser.flush_esc(&mut pending);
                    continue;
                }
                match input::read_stdin(&mut buf) {
                    Ok(n) if n > 0 => parser.feed(&buf[..n], &mut pending),
                    _ => {
                        let _ = tx.send(AppEvent::Quit);
                        return;
                    }
                }
            }
        })
        .map(|_| ())
}

/// what rmon has told the terminal: the document as last sent, and the
/// frames still waiting for an ack
struct Surface {
    hello: Hello,
    seq: u64,
    acked: u64,
    sent: Node,
    focus: Option<String>,
    next_chunk: u64,
    /// `RMON_TSP_LOG=<file>`: every message as JSONL, the recording format
    /// Tern's `surface-play` replays
    log: Option<File>,
    start: Instant,
}

impl Surface {
    fn new(hello: Hello) -> Surface {
        Surface {
            hello,
            seq: 0,
            acked: 0,
            sent: Node::new(SF, "col"),
            focus: None,
            next_chunk: 0,
            log: std::env::var_os("RMON_TSP_LOG").and_then(|p| File::create(p).ok()),
            start: Instant::now(),
        }
    }

    /// a new `o` replaces any screen surface of ours, so this also resets
    fn open(&mut self) -> std::io::Result<()> {
        self.seq = 0;
        self.acked = 0;
        self.focus = None;
        self.sent = Node::new(SF, "col");
        self.send(
            b'o',
            &json!({"id": SF, "mode": "screen", "title": "rmon", "role": "rmon.monitor"}),
        )?;
        if self.hello.has_feature("program-palette") {
            let variant = |p: &[(&str, &str)]| -> Value {
                let mut v: serde_json::Map<String, Value> =
                    p.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
                // the ratatui load gradient, one stop per 10%
                for (i, tok) in view::GRADIENT.iter().enumerate() {
                    if let ratatui::style::Color::Rgb(r, g, b) =
                        crate::ui::theme::gradient(i as f64 * 10.0)
                    {
                        v.insert(tok.to_string(), json!(format!("#{r:02x}{g:02x}{b:02x}")));
                    }
                }
                Value::Object(v)
            };
            self.send(
                b't',
                &json!({"sf": SF, "dark": variant(&PALETTE_DARK), "light": variant(&PALETTE_LIGHT)}),
            )?;
        }
        if self.hello.has_feature("styles") {
            self.send(b's', &json!({"sf": SF, "name": "rmon", "css": CSS}))?;
        }
        Ok(())
    }

    fn ready(&self) -> bool {
        self.seq - self.acked < self.hello.credits
    }

    fn ack(&mut self, s: u64) {
        self.acked = self.acked.max(s.min(self.seq));
    }

    fn frame(&mut self, doc: Node, focus: Option<String>) -> std::io::Result<()> {
        let mut ops = doc::diff(&self.sent, &doc);
        if focus != self.focus {
            ops.push(json!(["focus", focus]));
        }
        if ops.is_empty() {
            return Ok(());
        }
        self.seq += 1;
        self.send(b'f', &json!({"sf": SF, "s": self.seq, "ops": ops}))?;
        self.sent = doc;
        self.focus = focus;
        Ok(())
    }

    fn send(&mut self, verb: u8, body: &Value) -> std::io::Result<()> {
        let text = body.to_string();
        let mut out = Vec::with_capacity(text.len() + 32);
        wire::encode(&mut out, verb, &text, self.hello.apc, &mut self.next_chunk);
        let mut so = std::io::stdout().lock();
        so.write_all(&out)?;
        so.flush()?;
        self.record("out", Some(verb), || body.clone());
        Ok(())
    }

    fn record(&mut self, dir: &str, verb: Option<u8>, body: impl FnOnce() -> Value) {
        let Some(log) = &mut self.log else { return };
        let line = json!({
            "t": self.start.elapsed().as_millis() as u64,
            "dir": dir,
            "verb": verb.map(|v| (v as char).to_string()),
            "params": {},
            "body": body(),
        });
        let _ = writeln!(log, "{line}");
    }
}
