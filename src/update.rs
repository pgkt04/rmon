//! `rmon update`: self-update by handing off to the release install script.
//! curl/wget do the transport -- a TLS client dep is not worth it for a
//! command that runs once a month.

use std::process::Command;

const REPO: &str = "pgkt04/rmon";

/// tag out of github's /releases/latest redirect target,
/// e.g. .../releases/tag/v1.2.0 -> v1.2.0
fn tag_from_redirect(url: &str) -> Option<String> {
    let (_, tag) = url.trim().rsplit_once("/tag/")?;
    let tag = tag.trim_matches('/');
    (!tag.is_empty() && !tag.contains('/')).then(|| tag.to_string())
}

/// "v1.2.3" or "1.2.3" -> [1,2,3]; anything else None
fn semver(s: &str) -> Option<[u64; 3]> {
    let mut it = s.trim().trim_start_matches('v').splitn(3, '.');
    let mut out = [0u64; 3];
    for slot in &mut out {
        *slot = it.next()?.parse().ok()?;
    }
    Some(out)
}

/// resolve the /releases/latest redirect instead of the api, which rate
/// limits by ip; curl first, wget for the minimal distros without it
fn latest_tag() -> Result<String, String> {
    if which("curl") {
        let out = Command::new("curl")
            .args(["-fsSLI", "-o", "/dev/null", "-w", "%{url_effective}"])
            .arg(format!("https://github.com/{REPO}/releases/latest"))
            .output()
            .map_err(|e| format!("could not run curl: {e}"))?;
        if out.status.success()
            && let Some(tag) = tag_from_redirect(&String::from_utf8_lossy(&out.stdout))
        {
            return Ok(tag);
        }
    } else if which("wget") {
        // -S prints headers on stderr; the Location line carries the tag
        let out = Command::new("wget")
            .args(["-qS", "-O", "/dev/null"])
            .arg(format!("https://github.com/{REPO}/releases/latest"))
            .output()
            .map_err(|e| format!("could not run wget: {e}"))?;
        let headers = String::from_utf8_lossy(&out.stderr);
        if let Some(tag) = headers
            .lines()
            .rev()
            .filter(|l| l.trim_start().to_ascii_lowercase().starts_with("location:"))
            .find_map(tag_from_redirect)
        {
            return Ok(tag);
        }
    } else {
        return Err("need curl or wget on PATH".into());
    }
    Err("could not resolve the latest release tag".into())
}

fn which(bin: &str) -> bool {
    Command::new(bin)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

fn fetch(url: &str, dest: &std::path::Path) -> Result<(), String> {
    let status = if which("curl") {
        Command::new("curl")
            .args(["-fsSL", url, "-o"])
            .arg(dest)
            .status()
    } else {
        Command::new("wget")
            .args(["-qO"])
            .arg(dest)
            .arg(url)
            .status()
    };
    match status {
        Ok(s) if s.success() => Ok(()),
        _ => Err(format!("could not download {url}")),
    }
}

pub fn run() -> Result<(), String> {
    let current = env!("CARGO_PKG_VERSION");
    println!("rmon {current}, checking for a newer release...");
    let tag = latest_tag()?;
    // non-semver tags fall through and update on any difference
    match (semver(&tag), semver(current)) {
        (Some(l), Some(c)) if l <= c => {
            println!("already up to date (latest release is {tag})");
            return Ok(());
        }
        (None, _) if tag.trim_start_matches('v') == current => {
            println!("already up to date (latest release is {tag})");
            return Ok(());
        }
        _ => {}
    }
    println!("updating to {tag}");

    // replace the binary we are running as, not whatever dir the script
    // would pick; canonicalize follows an rmon symlink to the real file
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("cannot locate the running binary: {e}"))?;
    let bin_dir = exe
        .parent()
        .ok_or("cannot locate the running binary's directory")?
        .to_owned();

    // the install script already does checksums and the atomic rename that
    // survives updating a running rmon; reuse it instead of reimplementing
    let script = std::env::temp_dir().join(format!("rmon-install-{}.sh", std::process::id()));
    let url = format!("https://raw.githubusercontent.com/{REPO}/main/install.sh");
    fetch(&url, &script)?;
    let status = Command::new("sh")
        .arg(&script)
        .env("RMON_VERSION", &tag)
        .env("RMON_BIN_DIR", &bin_dir)
        .status();
    let _ = std::fs::remove_file(&script);
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("install script failed with {s}")),
        Err(e) => Err(format!("could not run sh: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_parses_from_redirect_url() {
        assert_eq!(
            tag_from_redirect("https://github.com/pgkt04/rmon/releases/tag/v1.2.0"),
            Some("v1.2.0".into())
        );
        assert_eq!(
            tag_from_redirect(" Location: https://github.com/x/y/releases/tag/v2.0.0\r"),
            Some("v2.0.0".into())
        );
        // no redirect happened (rate limit page, error html, plain latest url)
        assert_eq!(
            tag_from_redirect("https://github.com/pgkt04/rmon/releases/latest"),
            None
        );
        assert_eq!(tag_from_redirect(""), None);
        // trailing path junk is not a tag
        assert_eq!(
            tag_from_redirect("https://g.com/releases/tag/v1/extra"),
            None
        );
    }

    #[test]
    fn semver_parses_and_orders() {
        assert_eq!(semver("v1.2.3"), Some([1, 2, 3]));
        assert_eq!(semver("1.0.0"), Some([1, 0, 0]));
        assert_eq!(semver("v1.10.0"), Some([1, 10, 0]));
        assert_eq!(semver("nightly"), None);
        assert_eq!(semver("v1.2"), None);
        // array compare is lexicographic: exactly what semver needs
        assert!(semver("v1.10.0") > semver("v1.9.9"));
        assert!(semver("v2.0.0") > semver("v1.99.99"));
    }
}
