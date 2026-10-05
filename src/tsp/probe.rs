//! the `hello` handshake: a TSP terminal answers the query before the DA1
//! sentinel that every terminal answers.
//! spec: https://docs.stencil.so/tern/protocol/handshake.html

use std::io::Write;
use std::time::{Duration, Instant};

use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};

use super::input::{self, Input, Parser};
use super::wire::{self, Hello, Incoming};

/// every kind the native view is built from; a terminal missing one keeps the
/// ratatui renderer
pub const KINDS: &[&str] = &[
    "col", "row", "card", "text", "ansi", "kv", "table", "list", "item", "meter", "chart",
    "progress", "input", "status", "seg", "overlay",
];

const TIMEOUT: Duration = Duration::from_millis(1000);

/// a terminal that speaks TSP, plus what it sent around the hello: keys typed
/// meanwhile and the start of a sequence still in flight
pub struct Probe {
    pub hello: Hello,
    pub parser: Parser,
    pub early: Vec<Input>,
}

/// multiplexers swallow APC, so the reply could never come back;
/// `RMON_TSP=0` keeps the ratatui renderer
fn wanted(env: impl Fn(&str) -> Option<String>) -> bool {
    if env("RMON_TSP").as_deref() == Some("0") {
        return false;
    }
    if ["TMUX", "STY", "ZELLIJ"]
        .iter()
        .any(|v| env(v).is_some_and(|s| !s.is_empty()))
    {
        return false;
    }
    let term = env("TERM").unwrap_or_default().to_ascii_lowercase();
    !(term.starts_with("tmux") || term.starts_with("screen") || term == "dumb" || term == "linux")
}

/// probe the tty; Some leaves it in raw mode for the native backend, None
/// leaves it as it was
pub fn detect() -> Option<Probe> {
    // SAFETY: isatty only inspects the descriptor
    let ttys = unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 };
    if !ttys || !wanted(|k| std::env::var(k).ok()) {
        return None;
    }
    // raw before the query, or a cooked tty echoes the reply onto the screen
    enable_raw_mode().ok()?;
    match query() {
        Ok((Some(hello), parser, early)) if hello.has_kinds(KINDS) => Some(Probe {
            hello,
            parser,
            early,
        }),
        _ => {
            let _ = disable_raw_mode();
            None
        }
    }
}

fn query() -> std::io::Result<(Option<Hello>, Parser, Vec<Input>)> {
    let mut msg = Vec::new();
    wire::encode(&mut msg, b'q', &wire::hello_query(), usize::MAX, &mut 0);
    msg.extend_from_slice(b"\x1b[c");
    let mut out = std::io::stdout().lock();
    out.write_all(&msg)?;
    out.flush()?;

    let deadline = Instant::now() + TIMEOUT;
    let mut parser = Parser::default();
    let mut early = Vec::new();
    let mut hello = None;
    let mut buf = [0u8; 4096];
    let mut got = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || !input::readable(left.as_millis() as i32)? {
            break;
        }
        let n = input::read_stdin(&mut buf)?;
        if n == 0 {
            break;
        }
        parser.feed(&buf[..n], &mut got);
        let mut answered = false;
        for i in got.drain(..) {
            match i {
                Input::Da1 => answered = true,
                Input::Tsp(inner) => {
                    if let Some(Incoming::Reply(v)) = wire::decode(&inner)
                        && let Some(h) = Hello::from_reply(&v)
                    {
                        hello = Some(h);
                    }
                }
                key @ Input::Key(_) => early.push(key),
            }
        }
        if answered {
            break;
        }
    }
    Ok((hello, parser, early))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn probes_a_plain_terminal_even_without_tern_env() {
        // ssh drops TERM_PROGRAM; the query still reaches Tern
        assert!(wanted(env(&[("TERM", "xterm-256color")])));
    }

    #[test]
    fn never_probes_inside_a_multiplexer() {
        assert!(!wanted(env(&[("TMUX", "/tmp/tmux-501/default,1,0")])));
        assert!(!wanted(env(&[("TERM", "screen-256color")])));
        assert!(!wanted(env(&[("ZELLIJ", "0")])));
    }

    #[test]
    fn env_opt_out_wins() {
        assert!(!wanted(env(&[("RMON_TSP", "0"), ("TERM_PROGRAM", "tern")])));
    }
}
