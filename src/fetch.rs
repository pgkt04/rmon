//! neofetch-style system info for the `s` popup. everything here is a cheap
//! one-shot read (sysctl / uname / env / a couple of files) — no subprocesses.
//! ascii art and colors ported from the neofetch script's heredocs.

use ratatui::style::Color;

pub struct FetchInfo {
    pub logo: &'static [&'static str],
    /// one color per logo row, cycled when shorter (single-color logos)
    pub palette: &'static [Color],
    /// static (label, value) rows; the popup appends live ones from App
    pub lines: Vec<(String, String)>,
}

/// gather what we can; a field that fails to read just doesn't get a row
pub fn collect() -> FetchInfo {
    let mut lines: Vec<(String, String)> = Vec::new();
    let un = uname_strings();
    if let Some(os) = os_line(un.as_ref().map_or("", |(_, m)| m.as_str())) {
        lines.push(("os".into(), os));
    }
    if let Some(host) = host_line() {
        lines.push(("host".into(), host));
    }
    if let Some((release, _)) = &un
        && !release.is_empty()
    {
        lines.push(("kernel".into(), release.clone()));
    }
    if let Some(sh) = std::env::var_os("SHELL") {
        let base = std::path::Path::new(&sh)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned());
        if let Some(base) = base {
            lines.push(("shell".into(), base));
        }
    }
    if let Some(term) = std::env::var("TERM_PROGRAM")
        .or_else(|_| std::env::var("TERM"))
        .ok()
        .filter(|t| !t.is_empty())
    {
        lines.push(("terminal".into(), term));
    }
    let (logo, palette) = pick_logo();
    FetchInfo {
        logo,
        palette,
        lines,
    }
}

/// (release, machine) from uname(3); shared by both platforms
fn uname_strings() -> Option<(String, String)> {
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: uname just fills the struct we hand it
    if unsafe { libc::uname(&mut u) } != 0 {
        return None;
    }
    let s = |buf: &[libc::c_char]| {
        // SAFETY: uname NUL-terminates every field
        unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    };
    Some((s(&u.release), s(&u.machine)))
}

#[cfg(target_os = "macos")]
fn sysctl_string(name: &std::ffi::CStr) -> Option<String> {
    // classic two-call dance: size first, then the bytes
    let mut len: usize = 0;
    // SAFETY: null buf + len query is the documented probe form
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    } != 0
        || len == 0
    {
        return None;
    }
    let mut buf = vec![0u8; len];
    // SAFETY: buf is len bytes, kernel writes at most that
    if unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    buf.truncate(len);
    while buf.last() == Some(&0) {
        buf.pop();
    }
    String::from_utf8(buf).ok()
}

#[cfg(target_os = "macos")]
fn os_line(arch: &str) -> Option<String> {
    let ver = sysctl_string(c"kern.osproductversion")?;
    Some(match macos_name(&ver) {
        Some(name) => format!("{name} {ver} {arch}"),
        // future release we don't know yet: just show the number
        None => format!("macOS {ver} {arch}"),
    })
}

#[cfg(target_os = "macos")]
fn host_line() -> Option<String> {
    sysctl_string(c"hw.model")
}

#[cfg(target_os = "macos")]
fn pick_logo() -> (&'static [&'static str], &'static [Color]) {
    (DARWIN_LOGO, DARWIN_COLORS)
}

#[cfg(not(target_os = "macos"))]
fn os_line(_arch: &str) -> Option<String> {
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    os_release_field(&text, "PRETTY_NAME")
}

#[cfg(not(target_os = "macos"))]
fn host_line() -> Option<String> {
    let read = |p: &str| {
        std::fs::read_to_string(p)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let name = read("/sys/devices/virtual/dmi/id/product_name")?;
    Some(match read("/sys/devices/virtual/dmi/id/product_version") {
        Some(ver) => format!("{name} {ver}"),
        None => name,
    })
}

#[cfg(not(target_os = "macos"))]
fn pick_logo() -> (&'static [&'static str], &'static [Color]) {
    let id = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|t| os_release_field(&t, "ID"))
        .unwrap_or_default();
    logo_for_id(&id)
}

/// version -> marketing name, same table as neofetch's get_distro (plus the
/// releases that shipped after the script stopped moving)
// only the macos os_line calls this outside tests
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn macos_name(version: &str) -> Option<&'static str> {
    let mut it = version.split('.');
    let major = it.next()?;
    if major == "10" {
        Some(match it.next()? {
            "4" => "Mac OS X Tiger",
            "5" => "Mac OS X Leopard",
            "6" => "Mac OS X Snow Leopard",
            "7" => "Mac OS X Lion",
            "8" => "OS X Mountain Lion",
            "9" => "OS X Mavericks",
            "10" => "OS X Yosemite",
            "11" => "OS X El Capitan",
            "12" => "macOS Sierra",
            "13" => "macOS High Sierra",
            "14" => "macOS Mojave",
            "15" => "macOS Catalina",
            "16" => "macOS Big Sur",
            _ => return None,
        })
    } else {
        Some(match major {
            "11" => "macOS Big Sur",
            "12" => "macOS Monterey",
            "13" => "macOS Ventura",
            "14" => "macOS Sonoma",
            "15" => "macOS Sequoia",
            "26" => "macOS Tahoe",
            _ => return None,
        })
    }
}

/// pull one KEY=value line out of os-release text, quotes stripped
// linux-only outside tests
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn os_release_field(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let v = l.strip_prefix(key)?.strip_prefix('=')?;
        Some(v.trim().trim_matches('"').to_string())
    })
}

/// os-release ID -> logo; anything we don't carry art for gets Tux
// linux-only outside tests
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn logo_for_id(id: &str) -> (&'static [&'static str], &'static [Color]) {
    match id {
        "debian" => (DEBIAN_LOGO, DEBIAN_COLORS),
        "ubuntu" => (UBUNTU_LOGO, UBUNTU_COLORS),
        "arch" => (ARCH_LOGO, ARCH_COLORS),
        _ => (TUX_LOGO, TUX_COLORS),
    }
}

// ---- ascii art, ${cN} markers stripped; colors are the script's set_colors
// numbers as Color::Indexed, flattened to one color per row ----

// set_colors 2 3 1 1 5 4 — the classic six-stripe apple
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const DARWIN_LOGO: &[&str] = &[
    r#"                    c.'"#,
    r#"                 ,xNMM."#,
    r#"               .OMMMMo"#,
    r#"               lMMM""#,
    r#"     .;loddo:.  .olloddol;."#,
    r#"   cKMMMMMMMMMMNWMMMMMMMMMM0:"#,
    r#" .KMMMMMMMMMMMMMMMMMMMMMMMWd."#,
    r#" XMMMMMMMMMMMMMMMMMMMMMMMX."#,
    r#";MMMMMMMMMMMMMMMMMMMMMMMM:"#,
    r#":MMMMMMMMMMMMMMMMMMMMMMMM:"#,
    r#".MMMMMMMMMMMMMMMMMMMMMMMMX."#,
    r#" kMMMMMMMMMMMMMMMMMMMMMMMMWd."#,
    r#" 'XMMMMMMMMMMMMMMMMMMMMMMMMMMk"#,
    r#"  'XMMMMMMMMMMMMMMMMMMMMMMMMK."#,
    r#"    kMMMMMMMMMMMMMMMMMMMMMMd"#,
    r#"     ;KMMMMMMMWXXWMMMMMMMk."#,
    r#"       "cooc*"    "*coo'""#,
];
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const DARWIN_COLORS: &[Color] = &[
    Color::Indexed(2),
    Color::Indexed(2),
    Color::Indexed(2),
    Color::Indexed(2),
    Color::Indexed(2),
    Color::Indexed(2),
    Color::Indexed(3),
    Color::Indexed(3),
    Color::Indexed(1),
    Color::Indexed(1),
    Color::Indexed(1),
    Color::Indexed(1),
    Color::Indexed(5),
    Color::Indexed(5),
    Color::Indexed(4),
    Color::Indexed(4),
    Color::Indexed(4),
];

// set_colors 1 7 3 — the swirl itself is c2 white
const DEBIAN_LOGO: &[&str] = &[
    r#"       _,met$$$$$gg."#,
    r#"    ,g$$$$$$$$$$$$$$$P."#,
    r#"  ,g$$P"        """Y$$."."#,
    r#" ,$$P'              `$$$."#,
    r#"',$$P       ,ggs.     `$$b:"#,
    r#"`d$$'     ,$P"'   .    $$$"#,
    r#" $$P      d$'     ,    $$P"#,
    r#" $$:      $$.   -    ,d$$'"#,
    r#" $$;      Y$b._   _,d$P'"#,
    r#" Y$$.    `.`"Y$$$$P"'"#,
    r#" `$$b      "-.__"#,
    r#"  `Y$$"#,
    r#"   `Y$$."#,
    r#"     `$$b."#,
    r#"       `Y$$b."#,
    r#"          `"Y$b._"#,
    r#"              `""""#,
];
const DEBIAN_COLORS: &[Color] = &[Color::Indexed(7)];

// set_colors 1 7 3 — the circle of friends is c1 red
const UBUNTU_LOGO: &[&str] = &[
    r#"            .-/+oossssoo+\-."#,
    r#"        ´:+ssssssssssssssssss+:`"#,
    r#"      -+ssssssssssssssssssyyssss+-"#,
    r#"    .ossssssssssssssssssdMMMNysssso."#,
    r#"   /ssssssssssshdmmNNmmyNMMMMhssssss\"#,
    r#"  +ssssssssshmydMMMMMMMNddddyssssssss+"#,
    r#" /sssssssshNMMMyhhyyyyhmNMMMNhssssssss\"#,
    r#".ssssssssdMMMNhsssssssssshNMMMdssssssss."#,
    r#"+sssshhhyNMMNyssssssssssssyNMMMysssssss+"#,
    r#"ossyNMMMNyMMhsssssssssssssshmmmhssssssso"#,
    r#"ossyNMMMNyMMhsssssssssssssshmmmhssssssso"#,
    r#"+sssshhhyNMMNyssssssssssssyNMMMysssssss+"#,
    r#".ssssssssdMMMNhsssssssssshNMMMdssssssss."#,
    r#" \sssssssshNMMMyhhyyyyhdNMMMNhssssssss/"#,
    r#"  +sssssssssdmydMMMMMMMMddddyssssssss+"#,
    r#"   \ssssssssssshdmNNNNmyNMMMMhssssss/"#,
    r#"    .ossssssssssssssssssdMMMNysssso."#,
    r#"      -+sssssssssssssssssyyyssss+-"#,
    r#"        `:+ssssssssssssssssss+:`"#,
    r#"            .-\+oossssoo+/-."#,
];
const UBUNTU_COLORS: &[Color] = &[Color::Indexed(1)];

// set_colors 6 6 7 1 — c1 and c2 are both cyan anyway
const ARCH_LOGO: &[&str] = &[
    r#"                   -`"#,
    r#"                  .o+`"#,
    r#"                 `ooo/"#,
    r#"                `+oooo:"#,
    r#"               `+oooooo:"#,
    r#"               -+oooooo+:"#,
    r#"             `/:-:++oooo+:"#,
    r#"            `/++++/+++++++:"#,
    r#"           `/++++++++++++++:"#,
    r#"          `/+++ooooooooooooo/`"#,
    r#"         ./ooosssso++osssssso+`"#,
    r#"        .oossssso-````/ossssss+`"#,
    r#"       -osssssso.      :ssssssso."#,
    r#"      :osssssss/        osssso+++."#,
    r#"     /ossssssss/        +ssssooo/-"#,
    r#"   `/ossssso+/:-        -:/+osssso+-"#,
    r#"  `+sso+:-`                 `.-/+oso:"#,
    r#" `++:.                           `-/+/"#,
    r#" .`                                 `/"#,
];
const ARCH_COLORS: &[Color] = &[Color::Indexed(6)];

// set_colors fg 8 3 — grey head, default-fg belly, yellow beak and feet
const TUX_LOGO: &[&str] = &[
    r#"        #####"#,
    r#"       #######"#,
    r#"       ##O#O##"#,
    r#"       #######"#,
    r#"     ###########"#,
    r#"    #############"#,
    r#"   ###############"#,
    r#"   ################"#,
    r#"  #################"#,
    r#"#####################"#,
    r#"#####################"#,
    r#"  #################"#,
];
const TUX_COLORS: &[Color] = &[
    Color::Indexed(8),
    Color::Indexed(8),
    Color::Indexed(8),
    Color::Indexed(3),
    Color::Reset,
    Color::Reset,
    Color::Reset,
    Color::Reset,
    Color::Indexed(3),
    Color::Indexed(3),
    Color::Indexed(3),
    Color::Indexed(3),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn darwin_versions_map_to_marketing_names() {
        assert_eq!(macos_name("10.4.11"), Some("Mac OS X Tiger"));
        assert_eq!(macos_name("10.15.7"), Some("macOS Catalina"));
        assert_eq!(macos_name("10.16"), Some("macOS Big Sur"));
        assert_eq!(macos_name("11.6.1"), Some("macOS Big Sur"));
        assert_eq!(macos_name("12.0"), Some("macOS Monterey"));
        assert_eq!(macos_name("15.5"), Some("macOS Sequoia"));
        assert_eq!(macos_name("26.0"), Some("macOS Tahoe"));
        // unknown releases fall through so the caller shows the bare number
        assert_eq!(macos_name("99.1"), None);
        assert_eq!(macos_name("10.99"), None);
        assert_eq!(macos_name(""), None);
    }

    #[test]
    fn os_release_parses_quoted_and_bare_values() {
        let text = concat!(
            "NAME=\"Debian GNU/Linux\"\n",
            "PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\n",
            "ID_LIKE=debian-ish\n",
            "ID=debian\n",
            "VERSION_ID=\"12\"\n",
        );
        assert_eq!(
            os_release_field(text, "PRETTY_NAME").as_deref(),
            Some("Debian GNU/Linux 12 (bookworm)")
        );
        // ID must not match ID_LIKE or VERSION_ID
        assert_eq!(os_release_field(text, "ID").as_deref(), Some("debian"));
        assert_eq!(os_release_field(text, "MISSING"), None);
    }

    #[test]
    fn logo_selection_covers_known_ids_and_falls_back_to_tux() {
        // consts re-materialize per use site, so pointer identity is UB-ish;
        // content is the contract anyway
        assert_eq!(logo_for_id("debian").0, DEBIAN_LOGO);
        assert_eq!(logo_for_id("ubuntu").0, UBUNTU_LOGO);
        assert_eq!(logo_for_id("arch").0, ARCH_LOGO);
        assert_eq!(logo_for_id("gentoo").0, TUX_LOGO);
    }

    #[test]
    fn every_logo_has_a_usable_palette() {
        for (logo, pal) in [
            (DARWIN_LOGO, DARWIN_COLORS),
            (DEBIAN_LOGO, DEBIAN_COLORS),
            (UBUNTU_LOGO, UBUNTU_COLORS),
            (ARCH_LOGO, ARCH_COLORS),
            (TUX_LOGO, TUX_COLORS),
        ] {
            assert!(!logo.is_empty() && !pal.is_empty());
            // per-row palettes must line up with their art
            assert!(pal.len() == 1 || pal.len() == logo.len());
        }
    }

    #[test]
    fn collect_is_infallible_and_skips_nothing_essential() {
        let info = collect();
        assert!(!info.logo.is_empty());
        // kernel comes from uname and should exist everywhere we build
        assert!(info.lines.iter().any(|(l, _)| l == "kernel"));
    }
}
