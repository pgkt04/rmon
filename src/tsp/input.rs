//! raw pty input while a surface is live: TSP replies and events arrive as
//! APC strings (OSC 877 through a Windows ConPTY) mixed in with the keys.
//! crossterm would read `ESC _ tsp;e;{…}` as alt+_ and a burst of key
//! presses, so this splits them first and decodes legacy xterm keys itself.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Key(KeyEvent),
    /// a terminal -> program TSP string, `tsp;` onwards, terminator stripped
    Tsp(Vec<u8>),
    /// a DA1 answer (`CSI ? … c`)
    Da1,
}

/// a lone ESC this long with nothing after it is the Esc key, not a sequence start
pub const ESC_WAIT_MS: i32 = 25;
/// a string that never terminates is garbage; drop it instead of buffering forever
const MAX_PENDING: usize = 32 << 20;

#[derive(Default)]
pub struct Parser {
    buf: Vec<u8>,
}

enum Step {
    /// consumed n bytes, maybe producing an input
    Took(usize, Option<Input>),
    /// the bytes so far are a prefix of something longer
    Incomplete,
}

impl Parser {
    pub fn feed(&mut self, bytes: &[u8], out: &mut Vec<Input>) {
        self.buf.extend_from_slice(bytes);
        let mut i = 0;
        while i < self.buf.len() {
            match step(&self.buf[i..]) {
                Step::Took(n, input) => {
                    out.extend(input);
                    i += n;
                }
                Step::Incomplete => break,
            }
        }
        self.buf.drain(..i);
        if self.buf.len() > MAX_PENDING {
            self.buf.clear();
        }
    }

    /// true when only a bare ESC is buffered: the caller waits ESC_WAIT_MS for
    /// more input, then calls `flush_esc`
    pub fn lone_esc(&self) -> bool {
        self.buf == [0x1b]
    }

    pub fn flush_esc(&mut self, out: &mut Vec<Input>) {
        if self.lone_esc() {
            self.buf.clear();
            out.push(key(KeyCode::Esc, KeyModifiers::NONE));
        }
    }
}

/// wait up to `timeout_ms` for stdin to have bytes (or hang up)
pub fn readable(timeout_ms: i32) -> std::io::Result<bool> {
    let mut fd = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: one valid pollfd, count 1
        let r = unsafe { libc::poll(&mut fd, 1, timeout_ms) };
        if r >= 0 {
            return Ok(r > 0);
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// raw read(2) on stdin; Ok(0) is end of input
pub fn read_stdin(buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        // SAFETY: buf is valid for buf.len() writable bytes
        let r = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        if r >= 0 {
            return Ok(r as usize);
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

fn key(code: KeyCode, mods: KeyModifiers) -> Input {
    Input::Key(KeyEvent::new(code, mods))
}

fn step(b: &[u8]) -> Step {
    match b[0] {
        0x1b => escape(b),
        b'\r' | b'\n' => Step::Took(1, Some(key(KeyCode::Enter, KeyModifiers::NONE))),
        b'\t' => Step::Took(1, Some(key(KeyCode::Tab, KeyModifiers::NONE))),
        0x7f | 0x08 => Step::Took(1, Some(key(KeyCode::Backspace, KeyModifiers::NONE))),
        c @ 0x01..=0x1a => Step::Took(
            1,
            Some(key(
                KeyCode::Char((b'a' + c - 1) as char),
                KeyModifiers::CONTROL,
            )),
        ),
        0x00 | 0x1c..=0x1f => Step::Took(1, None),
        _ => match utf8_char(b) {
            Some(Ok((c, n))) => Step::Took(n, Some(key(KeyCode::Char(c), KeyModifiers::NONE))),
            Some(Err(())) => Step::Took(1, None),
            None => Step::Incomplete,
        },
    }
}

/// Some(Ok) a char and its length, Some(Err) invalid, None truncated
fn utf8_char(b: &[u8]) -> Option<Result<(char, usize), ()>> {
    let n = match b[0] {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return Some(Err(())),
    };
    if b.len() < n {
        return None;
    }
    Some(
        std::str::from_utf8(&b[..n])
            .ok()
            .and_then(|s| s.chars().next())
            .map(|c| (c, n))
            .ok_or(()),
    )
}

/// end of a string started at `from`: (index of the terminator, its length).
/// ST always ends it; BEL too when `bel` (OSC)
fn string_end(b: &[u8], from: usize, bel: bool) -> Option<(usize, usize)> {
    let mut i = from;
    while i < b.len() {
        match b[i] {
            0x07 if bel => return Some((i, 1)),
            0x1b if b.get(i + 1) == Some(&b'\\') => return Some((i, 2)),
            _ => i += 1,
        }
    }
    None
}

fn escape(b: &[u8]) -> Step {
    let Some(&next) = b.get(1) else {
        return Step::Incomplete;
    };
    match next {
        b'_' => match string_end(b, 2, false) {
            Some((end, t)) => {
                let inner = &b[2..end];
                let tsp = inner
                    .starts_with(b"tsp;")
                    .then(|| Input::Tsp(inner.to_vec()));
                Step::Took(end + t, tsp)
            }
            None => Step::Incomplete,
        },
        b']' => match string_end(b, 2, true) {
            Some((end, t)) => {
                let tsp = b[2..end]
                    .strip_prefix(b"877;")
                    .filter(|s| s.starts_with(b"tsp;"))
                    .map(|s| Input::Tsp(s.to_vec()));
                Step::Took(end + t, tsp)
            }
            None => Step::Incomplete,
        },
        b'P' | b'X' | b'^' => match string_end(b, 2, false) {
            Some((end, t)) => Step::Took(end + t, None),
            None => Step::Incomplete,
        },
        b'[' => csi(b),
        b'O' => match b.get(2) {
            None => Step::Incomplete,
            Some(&f) => Step::Took(3, final_key(f, KeyModifiers::NONE)),
        },
        0x1b => Step::Took(1, Some(key(KeyCode::Esc, KeyModifiers::NONE))),
        _ => match utf8_char(&b[1..]) {
            Some(Ok((c, n))) => Step::Took(1 + n, Some(key(KeyCode::Char(c), KeyModifiers::ALT))),
            Some(Err(())) => Step::Took(2, None),
            None => Step::Incomplete,
        },
    }
}

fn csi(b: &[u8]) -> Step {
    // params 0x30-0x3f, intermediates 0x20-0x2f, final 0x40-0x7e
    let mut i = 2;
    while i < b.len() && (0x20..=0x3f).contains(&b[i]) {
        i += 1;
    }
    let Some(&fin) = b.get(i) else {
        return Step::Incomplete;
    };
    let params = &b[2..i];
    let n = i + 1;
    if !(0x40..=0x7e).contains(&fin) {
        return Step::Took(n, None);
    }
    if params.first() == Some(&b'?') {
        return Step::Took(n, (fin == b'c').then_some(Input::Da1));
    }
    let nums: Vec<u32> = std::str::from_utf8(params)
        .unwrap_or("")
        .split(';')
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    // xterm modifier param: 1 + (shift 1 | alt 2 | ctrl 4)
    let m = nums.get(1).copied().unwrap_or(1).saturating_sub(1);
    let mut mods = KeyModifiers::NONE;
    if m & 1 != 0 {
        mods |= KeyModifiers::SHIFT;
    }
    if m & 2 != 0 {
        mods |= KeyModifiers::ALT;
    }
    if m & 4 != 0 {
        mods |= KeyModifiers::CONTROL;
    }
    let input = if fin == b'~' {
        let code = match nums.first().copied().unwrap_or(0) {
            1 | 7 => Some(KeyCode::Home),
            2 => Some(KeyCode::Insert),
            3 => Some(KeyCode::Delete),
            4 | 8 => Some(KeyCode::End),
            5 => Some(KeyCode::PageUp),
            6 => Some(KeyCode::PageDown),
            _ => None,
        };
        code.map(|c| key(c, mods))
    } else {
        final_key(fin, mods)
    };
    Step::Took(n, input)
}

fn final_key(fin: u8, mods: KeyModifiers) -> Option<Input> {
    let code = match fin {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'Z' => KeyCode::BackTab,
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'R' => KeyCode::F(3),
        b'S' => KeyCode::F(4),
        _ => return None,
    };
    Some(key(code, mods))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(chunks: &[&[u8]]) -> Vec<Input> {
        let mut p = Parser::default();
        let mut out = Vec::new();
        for c in chunks {
            p.feed(c, &mut out);
        }
        p.flush_esc(&mut out);
        out
    }

    fn k(code: KeyCode) -> Input {
        key(code, KeyModifiers::NONE)
    }

    #[test]
    fn tsp_event_never_leaks_as_keys() {
        // the json holds q, k, s: rmon hotkeys that must not fire
        let ev = br#"tsp;e;{"ev":"ack","sf":"rmon","s":3,"q":"ks"}"#;
        let mut wire = b"q\x1b_".to_vec();
        wire.extend_from_slice(ev);
        wire.extend_from_slice(b"\x1b\\j");
        assert_eq!(
            parse(&[&wire]),
            vec![
                k(KeyCode::Char('q')),
                Input::Tsp(ev.to_vec()),
                k(KeyCode::Char('j'))
            ]
        );
    }

    #[test]
    fn tsp_string_split_across_reads() {
        let out = parse(&[b"\x1b_tsp;e;{\"ev\":", b"\"ack\",\"s\":1}\x1b", b"\\"]);
        assert_eq!(
            out,
            vec![Input::Tsp(br#"tsp;e;{"ev":"ack","s":1}"#.to_vec())]
        );
    }

    #[test]
    fn osc_877_with_st_or_bel() {
        let out = parse(&[b"\x1b]877;tsp;e;{}\x07\x1b]877;tsp;r;{}\x1b\\"]);
        assert_eq!(
            out,
            vec![
                Input::Tsp(b"tsp;e;{}".to_vec()),
                Input::Tsp(b"tsp;r;{}".to_vec())
            ]
        );
    }

    #[test]
    fn other_strings_and_reports_are_swallowed() {
        let out = parse(&[b"\x1b]11;rgb:0000/0000/0000\x07\x1b_Gok\x1b\\\x1b[?62;22c"]);
        assert_eq!(out, vec![Input::Da1]);
    }

    #[test]
    fn arrows_and_modified_keys() {
        let out = parse(&[b"\x1b[A\x1bOB\x1b[1;5A\x1b[5~\x1b[3~"]);
        assert_eq!(
            out,
            vec![
                k(KeyCode::Up),
                k(KeyCode::Down),
                key(KeyCode::Up, KeyModifiers::CONTROL),
                k(KeyCode::PageUp),
                k(KeyCode::Delete),
            ]
        );
    }

    #[test]
    fn lone_esc_waits_for_quiet() {
        let mut p = Parser::default();
        let mut out = Vec::new();
        p.feed(b"\x1b", &mut out);
        assert!(out.is_empty() && p.lone_esc());
        // the rest of an arrow arrives before the wait expires: no Esc
        p.feed(b"[B", &mut out);
        assert_eq!(out, vec![k(KeyCode::Down)]);
        p.feed(b"\x1b", &mut out);
        p.flush_esc(&mut out);
        assert_eq!(out.last(), Some(&k(KeyCode::Esc)));
    }

    #[test]
    fn control_and_text_keys() {
        let out = parse(&[b"\x03\r\x7fa\xc3\xa9", b"\xe2\x9c", b"\x93"]);
        assert_eq!(
            out,
            vec![
                key(KeyCode::Char('c'), KeyModifiers::CONTROL),
                k(KeyCode::Enter),
                k(KeyCode::Backspace),
                k(KeyCode::Char('a')),
                k(KeyCode::Char('é')),
                k(KeyCode::Char('✓')),
            ]
        );
    }
}
