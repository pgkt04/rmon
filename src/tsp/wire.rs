//! TSP framing: `ESC _ tsp ; <verb> [; key=value]* ; <body> ESC \`.
//! spec: https://docs.stencil.so/tern/protocol/transport.html

use serde_json::{Value, json};

pub const VERSION: u64 = 1;
const PREFIX: &[u8] = b"\x1b_tsp;";
const ST: &[u8] = b"\x1b\\";

/// the terminal's `hello` reply, the parts rmon reads
#[derive(Debug, Clone, PartialEq)]
pub struct Hello {
    /// largest body per message; longer ones go out in chunks
    pub apc: usize,
    /// frames that may wait for an ack
    pub credits: u64,
    pub kinds: Vec<String>,
    pub features: Vec<String>,
    /// pane width in cells; `resize` events update it
    pub cols: Option<u64>,
}

impl Hello {
    pub fn from_reply(v: &Value) -> Option<Hello> {
        if v["r"] != "hello" || v["v"] != VERSION {
            return None;
        }
        let strings = |key: &str| -> Vec<String> {
            v[key]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default()
        };
        Some(Hello {
            apc: v["apc"].as_u64().map_or(65_536, |n| n.max(64) as usize),
            credits: v["credits"].as_u64().unwrap_or(2).max(1),
            kinds: strings("kinds"),
            features: strings("features"),
            cols: v["cols"].as_u64(),
        })
    }

    pub fn has_feature(&self, name: &str) -> bool {
        self.features.iter().any(|f| f == name)
    }

    pub fn has_kinds(&self, kinds: &[&str]) -> bool {
        kinds
            .iter()
            .all(|k| self.kinds.iter().any(|have| have == k))
    }
}

/// the terminal -> program messages rmon acts on; the rest are dropped
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// highest frame the window drew
    Ack(u64),
    /// the surface's width in cells changed
    Resize(u64),
    /// click on a list item (`id` the list) or a standalone node
    Select { id: String, item: String },
    /// double-click
    Activate { id: String, item: String },
    /// a custom pointer action, `name=value` already split
    Action { act: String, value: Option<String> },
    /// retention or a cleared screen dropped these nodes or surfaces
    Gone(Vec<String>),
    /// the terminal rejected something rmon sent
    Error(String),
    /// the DA1 answer: rmon writes DA1 after its last TSP message so the
    /// exit drain knows every reply in flight has been read
    Da1,
}

impl Event {
    pub fn from_value(v: &Value) -> Option<Event> {
        let s = |key: &str| v[key].as_str().map(String::from);
        Some(match v["ev"].as_str()? {
            "ack" => Event::Ack(v["s"].as_u64()?),
            "resize" => Event::Resize(v["cols"].as_u64()?),
            "select" => Event::Select {
                id: s("id")?,
                item: s("item")?,
            },
            "activate" => Event::Activate {
                id: s("id")?,
                item: s("item")?,
            },
            "action" => Event::Action {
                act: s("act")?,
                value: s("value"),
            },
            "gone" => Event::Gone(
                v["ids"]
                    .as_array()?
                    .iter()
                    .filter_map(|i| i.as_str().map(String::from))
                    .collect(),
            ),
            "error" => Event::Error(match (v["s"].as_u64(), v["op"].as_u64()) {
                (Some(s), Some(op)) => format!("frame {s} op {op}: {}", v["msg"]),
                _ => v["msg"].to_string(),
            }),
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Reply(Value),
    Event(Value),
}

/// `inner` is the string between `ESC _` (or `ESC ] 877;`) and its terminator
pub fn decode(inner: &[u8]) -> Option<Incoming> {
    let rest = inner.strip_prefix(b"tsp;")?;
    let semi = rest.iter().position(|&b| b == b';')?;
    let verb = &rest[..semi];
    let mut body = &rest[semi + 1..];
    // the grammar allows params before the body; Tern sends none, skip them anyway
    while let Some(i) = body.iter().position(|&b| b == b';') {
        if !is_param(&body[..i]) {
            break;
        }
        body = &body[i + 1..];
    }
    let v: Value = serde_json::from_slice(body).ok()?;
    match verb {
        b"r" => Some(Incoming::Reply(v)),
        b"e" => Some(Incoming::Event(v)),
        _ => None,
    }
}

fn is_param(seg: &[u8]) -> bool {
    let Some(eq) = seg.iter().position(|&b| b == b'=') else {
        return false;
    };
    eq > 0
        && seg[..eq]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
        && seg[eq + 1..]
            .iter()
            .all(|b| (0x21..=0x7e).contains(b) && *b != b';')
}

pub fn hello_query() -> String {
    json!({"q": "hello", "v": [VERSION], "app": "rmon", "ver": env!("CARGO_PKG_VERSION")})
        .to_string()
}

/// one program -> terminal message, split into `c=`/`m=1` chunks when the body
/// passes `limit` bytes
pub fn encode(out: &mut Vec<u8>, verb: u8, body: &str, limit: usize, next_chunk: &mut u64) {
    if body.len() <= limit {
        frame(out, verb, "", body.as_bytes());
        return;
    }
    *next_chunk += 1;
    let id = *next_chunk;
    let mut start = 0;
    while start < body.len() {
        let end = if body.len() - start <= limit {
            body.len()
        } else {
            split_point(body, start, start + limit)
        };
        let params = if end < body.len() {
            format!("c={id:x};m=1;")
        } else {
            format!("c={id:x};")
        };
        frame(out, verb, &params, &body.as_bytes()[start..end]);
        start = end;
    }
}

/// a chunk must end on a char boundary, and the next one must not open with a
/// `key=value;`-shaped run: Tern would read it as a parameter. splitting right
/// before any byte outside `[A-Za-z0-9_-]` is always safe
fn split_point(body: &str, start: usize, max: usize) -> usize {
    let b = body.as_bytes();
    let safe = |i: usize| {
        body.is_char_boundary(i) && !(b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'-')
    };
    (start + 1..=max)
        .rev()
        .find(|&i| safe(i))
        .or_else(|| (start + 1..=max).rev().find(|&i| body.is_char_boundary(i)))
        .unwrap_or(max)
}

fn frame(out: &mut Vec<u8>, verb: u8, params: &str, body: &[u8]) {
    out.extend_from_slice(PREFIX);
    out.push(verb);
    out.push(b';');
    out.extend_from_slice(params.as_bytes());
    out.extend_from_slice(body);
    out.extend_from_slice(ST);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// what Tern does with our bytes: split messages, take params, join chunks
    fn join(wire: &[u8]) -> Vec<(u8, String)> {
        let mut msgs = Vec::new();
        let mut pending: Option<(u8, String, Vec<u8>)> = None;
        for raw in wire
            .split(|&b| b == 0x1b)
            .filter(|s| s.starts_with(b"_tsp;"))
        {
            let inner = &raw[5..];
            let verb = inner[0];
            let mut rest = &inner[2..];
            let mut chunk = None;
            let mut more = false;
            while let Some(i) = rest.iter().position(|&b| b == b';') {
                if !is_param(&rest[..i]) {
                    break;
                }
                let seg = std::str::from_utf8(&rest[..i]).unwrap();
                match seg.split_once('=').unwrap() {
                    ("c", v) => chunk = Some(v.to_string()),
                    ("m", "1") => more = true,
                    _ => {}
                }
                rest = &rest[i + 1..];
            }
            match (chunk, &mut pending) {
                (Some(c), Some((_, pc, buf))) if *pc == c => buf.extend_from_slice(rest),
                (Some(c), _) => pending = Some((verb, c, rest.to_vec())),
                (None, _) => msgs.push((verb, String::from_utf8(rest.to_vec()).unwrap())),
            }
            if !more && let Some((v, _, buf)) = pending.take() {
                msgs.push((v, String::from_utf8(buf).unwrap()));
            }
        }
        msgs
    }

    #[test]
    fn small_body_is_one_message() {
        let mut out = Vec::new();
        encode(
            &mut out,
            b'f',
            r#"{"sf":"rmon","s":1,"ops":[]}"#,
            64,
            &mut 0,
        );
        assert_eq!(
            out,
            b"\x1b_tsp;f;{\"sf\":\"rmon\",\"s\":1,\"ops\":[]}\x1b\\"
        );
    }

    #[test]
    fn chunks_rejoin_to_the_body() {
        // multibyte chars and key=value; runs that a naive split would expose
        let body = json!({"t": "x=1;y=2;".repeat(40), "u": "µs°C ⣿⣀ ".repeat(30)}).to_string();
        let mut out = Vec::new();
        encode(&mut out, b'f', &body, 64, &mut 0);
        let msgs = join(&out);
        assert_eq!(msgs, vec![(b'f', body)]);
    }

    #[test]
    fn every_chunk_fits_the_limit() {
        let body = "a".repeat(10) + &"é;k=v".repeat(100);
        let mut out = Vec::new();
        encode(&mut out, b'f', &body, 50, &mut 7);
        for raw in out
            .split(|&b| b == 0x1b)
            .filter(|s| s.starts_with(b"_tsp;"))
        {
            // `_tsp;f;c=8;m=1;` adds at most 16 bytes on top of the body
            assert!(raw.len() <= 50 + 16, "{} bytes", raw.len());
        }
        assert!(String::from_utf8(out).unwrap().contains("c=8;"));
    }

    #[test]
    fn decodes_hello_reply() {
        let inner = br#"tsp;r;{"r":"hello","v":1,"kinds":["col","text"],"features":["styles"],"apc":4096,"credits":3}"#;
        let Some(Incoming::Reply(v)) = decode(inner) else {
            panic!("not a reply");
        };
        let h = Hello::from_reply(&v).unwrap();
        assert_eq!(h.apc, 4096);
        assert_eq!(h.credits, 3);
        assert!(h.has_kinds(&["col", "text"]));
        assert!(!h.has_kinds(&["col", "meter"]));
        assert!(h.has_feature("styles"));
    }

    #[test]
    fn hello_of_another_version_is_refused() {
        assert_eq!(Hello::from_reply(&json!({"r": "hello", "v": 2})), None);
    }

    #[test]
    fn decodes_events() {
        let ev = |s: &str| match decode(s.as_bytes()) {
            Some(Incoming::Event(v)) => Event::from_value(&v),
            _ => None,
        };
        assert_eq!(
            ev(r#"tsp;e;{"ev":"ack","sf":"rmon","s":7}"#),
            Some(Event::Ack(7))
        );
        assert_eq!(
            ev(r#"tsp;e;{"ev":"action","sf":"rmon","id":"dock.q","act":"key","value":"q"}"#),
            Some(Event::Action {
                act: "key".into(),
                value: Some("q".into())
            })
        );
        assert_eq!(
            ev(r#"tsp;e;{"ev":"select","sf":"rmon","id":"proc.list","item":"proc.p12"}"#),
            Some(Event::Select {
                id: "proc.list".into(),
                item: "proc.p12".into()
            })
        );
        assert_eq!(
            ev(r#"tsp;e;{"ev":"resize","sf":"rmon","cols":80}"#),
            Some(Event::Resize(80))
        );
        assert_eq!(ev(r#"tsp;e;{"ev":"theme","dark":false}"#), None);
        assert_eq!(ev(r#"tsp;e;not json"#), None);
    }
}
