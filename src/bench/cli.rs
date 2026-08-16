use std::io::Write;
use std::path::PathBuf;

use super::{BenchConfig, BenchEvent, run};

const USAGE: &str = "usage: rmon bench [--path DIR] [--size-mb N] [--secs N]\n       rmon bench --device /dev/rdisk0   read-only raw device test (root)";

pub fn parse_args(args: &[String]) -> Result<BenchConfig, String> {
    let mut cfg = BenchConfig::default();
    let mut path_given = false;
    let mut size_given = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--path" => {
                cfg.target_dir = PathBuf::from(value("--path")?);
                path_given = true;
            }
            "--size-mb" => {
                let mb: u64 = value("--size-mb")?
                    .parse()
                    .map_err(|_| format!("--size-mb wants a number\n{USAGE}"))?;
                if mb == 0 {
                    return Err(format!("--size-mb must be positive\n{USAGE}"));
                }
                cfg.size = mb
                    .checked_mul(1 << 20)
                    .ok_or_else(|| format!("--size-mb is too large\n{USAGE}"))?;
                size_given = true;
            }
            "--secs" => {
                cfg.secs_per_rand_test = value("--secs")?
                    .parse()
                    .map_err(|_| format!("--secs wants a number\n{USAGE}"))?;
            }
            "--device" => cfg.device = Some(PathBuf::from(value("--device")?)),
            other => return Err(format!("unknown flag {other}\n{USAGE}")),
        }
    }
    if cfg.device.is_some() {
        if path_given {
            return Err(format!(
                "--device and --path are mutually exclusive\n{USAGE}"
            ));
        }
        if !size_given {
            // devices default to a 1 GiB span cap instead of the file default
            cfg.size = 1 << 30;
        }
    }
    Ok(cfg)
}

fn history_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".rmon").join("bench_history.jsonl"))
}

pub fn run_headless(cfg: &BenchConfig) -> Result<(), String> {
    match &cfg.device {
        Some(dev) => println!(
            "benchmarking {} (read-only, up to {} MiB) ...",
            dev.display(),
            cfg.size >> 20
        ),
        None => println!(
            "benchmarking {} ({} MiB file) ...",
            cfg.target_dir.display(),
            cfg.size >> 20
        ),
    }
    let mut report = None;
    let mut error = None;
    run(cfg, &mut |ev| match ev {
        BenchEvent::Progress {
            kind,
            frac,
            bytes_per_sec,
        } => {
            print!(
                "\r  {:<10} {:>3.0}%  {:>10.1} MB/s   ",
                kind.label(),
                frac * 100.0,
                bytes_per_sec / 1e6
            );
            let _ = std::io::stdout().flush();
        }
        BenchEvent::TestDone(r) => {
            let lat = match (r.p50_us, r.p99_us) {
                (Some(p50), Some(p99)) => format!("  p50 {p50} µs  p99 {p99} µs"),
                _ => String::new(),
            };
            println!(
                "\r  {:<10} {:>10.1} MB/s {:>9.0} iops{lat}",
                r.kind.label(),
                r.bytes_per_sec / 1e6,
                r.iops
            );
        }
        BenchEvent::Finished(r) => report = Some(r),
        BenchEvent::Error(e) => error = Some(e),
    });
    if let Some(e) = error {
        return Err(e);
    }
    let report = report.ok_or("bench produced no report")?;
    if !report.direct {
        println!("  note: direct io unavailable here; numbers include the page cache");
    }
    if let Some(path) = history_path() {
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            writeln!(f, "{}", report.to_json_line())?;
            Ok(())
        };
        match write() {
            Ok(()) => println!("saved to {}", path.display()),
            Err(e) => println!("could not save history: {e}"),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn defaults_when_no_args() {
        let cfg = parse_args(&[]).unwrap();
        assert_eq!(cfg.size, 256 << 20);
        assert_eq!(cfg.secs_per_rand_test, 3);
    }

    #[test]
    fn parses_all_flags() {
        let cfg = parse_args(&s(&["--path", "/tmp", "--size-mb", "64", "--secs", "5"])).unwrap();
        assert_eq!(cfg.target_dir, std::path::PathBuf::from("/tmp"));
        assert_eq!(cfg.size, 64 << 20);
        assert_eq!(cfg.secs_per_rand_test, 5);
    }

    #[test]
    fn rejects_unknown_and_malformed() {
        assert!(parse_args(&s(&["--nope"])).is_err());
        assert!(parse_args(&s(&["--size-mb"])).is_err());
        assert!(parse_args(&s(&["--size-mb", "abc"])).is_err());
        assert!(parse_args(&s(&["--size-mb", "0"])).is_err());
    }

    #[test]
    fn device_conflicts_with_path() {
        assert!(parse_args(&s(&["--device", "/x", "--path", "/y"])).is_err());
        assert!(parse_args(&s(&["--path", "/y", "--device", "/x"])).is_err());
    }

    #[test]
    fn device_alone_parses() {
        let cfg = parse_args(&s(&["--device", "/dev/rdisk0"])).unwrap();
        assert_eq!(cfg.device, Some(std::path::PathBuf::from("/dev/rdisk0")));
        // no --size-mb: devices default to a 1 GiB cap
        assert_eq!(cfg.size, 1 << 30);
        let cfg = parse_args(&s(&["--device", "/dev/rdisk0", "--size-mb", "64"])).unwrap();
        assert_eq!(cfg.size, 64 << 20);
    }
}
