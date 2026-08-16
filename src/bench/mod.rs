//! disk benchmark engine — no ui dependencies; emits BenchEvent via a closure

use std::path::PathBuf;

pub mod cli;
mod engine;

pub use engine::run;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestKind {
    SeqWrite,
    SeqRead,
    RandRead,
    RandWrite,
}

impl TestKind {
    pub fn label(self) -> &'static str {
        match self {
            TestKind::SeqWrite => "seq write",
            TestKind::SeqRead => "seq read",
            TestKind::RandRead => "rand read",
            TestKind::RandWrite => "rand write",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TestResult {
    pub kind: TestKind,
    pub bytes_per_sec: f64,
    pub iops: f64,
    pub p50_us: Option<u64>,
    pub p99_us: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct BenchReport {
    pub ts: u64,
    pub target: String,
    pub size: u64,
    pub direct: bool,
    pub results: Vec<TestResult>,
}

#[derive(Debug)]
pub enum BenchEvent {
    Progress {
        kind: TestKind,
        frac: f64,
        bytes_per_sec: f64,
    },
    TestDone(TestResult),
    Finished(BenchReport),
    Error(String),
}

pub struct BenchConfig {
    pub target_dir: PathBuf,
    pub size: u64,
    pub secs_per_rand_test: u64,
    /// Some(path): read-only raw device bench instead of the file bench.
    /// the device is READ-ONLY BY CONSTRUCTION — nothing in the engine can write it.
    pub device: Option<PathBuf>,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            target_dir: std::env::temp_dir(),
            size: 256 << 20,
            secs_per_rand_test: 3,
            device: None,
        }
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn opt_json(v: Option<u64>) -> String {
    v.map_or_else(|| "null".into(), |x| x.to_string())
}

impl BenchReport {
    /// one JSONL line; flat schema, hand-rolled to avoid a serde dependency
    pub fn to_json_line(&self) -> String {
        let results: Vec<String> = self
            .results
            .iter()
            .map(|r| {
                format!(
                    r#"{{"kind":"{}","bytes_per_sec":{:.1},"iops":{:.1},"p50_us":{},"p99_us":{}}}"#,
                    r.kind.label(),
                    r.bytes_per_sec,
                    r.iops,
                    opt_json(r.p50_us),
                    opt_json(r.p99_us),
                )
            })
            .collect();
        format!(
            r#"{{"ts":{},"target":"{}","size":{},"direct":{},"results":[{}]}}"#,
            self.ts,
            json_escape(&self.target),
            self.size,
            self.direct,
            results.join(","),
        )
    }
}

/// nearest-rank percentile on an already-sorted slice
pub(crate) fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// tiny deterministic prng; quality is irrelevant, non-repetition is enough
pub(crate) struct Lcg(pub u64);

impl Lcg {
    pub fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 11
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_picks_expected_ranks() {
        let v: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&v, 50.0), 50);
        assert_eq!(percentile(&v, 99.0), 99);
        assert_eq!(percentile(&v, 100.0), 100);
        assert_eq!(percentile(&[42], 50.0), 42);
        assert_eq!(percentile(&[], 50.0), 0);
    }

    #[test]
    fn json_line_shape() {
        let r = BenchReport {
            ts: 1700000000,
            target: "/tmp/my \"dir\"".into(),
            size: 256 << 20,
            direct: true,
            results: vec![
                TestResult {
                    kind: TestKind::SeqWrite,
                    bytes_per_sec: 2.0e9,
                    iops: 2000.0,
                    p50_us: None,
                    p99_us: None,
                },
                TestResult {
                    kind: TestKind::RandRead,
                    bytes_per_sec: 4.9e7,
                    iops: 12000.0,
                    p50_us: Some(81),
                    p99_us: Some(113),
                },
            ],
        };
        let line = r.to_json_line();
        assert!(line.starts_with('{') && line.ends_with('}'));
        assert!(!line.contains('\n'));
        assert!(line.contains(r#""ts":1700000000"#));
        assert!(line.contains(r#""target":"/tmp/my \"dir\"""#)); // quotes escaped
        assert!(line.contains(r#""kind":"seq write""#));
        assert!(line.contains(r#""p50_us":81"#));
        assert!(line.contains(r#""p50_us":null"#));
    }

    #[test]
    fn lcg_is_deterministic_and_varied() {
        let mut a = Lcg(7);
        let mut b = Lcg(7);
        let xs: Vec<u64> = (0..8).map(|_| a.next()).collect();
        let ys: Vec<u64> = (0..8).map(|_| b.next()).collect();
        assert_eq!(xs, ys);
        assert!(xs.windows(2).all(|w| w[0] != w[1]));
    }
}
