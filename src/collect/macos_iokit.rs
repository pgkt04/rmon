//! IORegistry readers: whole-disk io counters (iterate IOMedia with Whole=true,
//! read the parent IOBlockStorageDriver's Statistics dictionary) and the
//! AppleSmartBattery charge state

use libc::{c_char, c_int, c_uint, c_void};

use super::{BatterySnapshot, BatteryState, CollectError, DiskStats};

type CFRef = *const c_void;
type IoObject = c_uint;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> CFRef;
    fn IOServiceGetMatchingServices(port: c_uint, matching: CFRef, it: *mut IoObject) -> c_int;
    fn IOIteratorNext(it: IoObject) -> IoObject;
    fn IOObjectRelease(obj: IoObject) -> c_int;
    fn IORegistryEntryCreateCFProperties(
        entry: IoObject,
        props: *mut CFRef,
        allocator: CFRef,
        options: c_uint,
    ) -> c_int;
    fn IORegistryEntryGetParentEntry(
        entry: IoObject,
        plane: *const c_char,
        parent: *mut IoObject,
    ) -> c_int;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDictionaryGetValue(d: CFRef, key: CFRef) -> CFRef;
    fn CFStringCreateWithCString(alloc: CFRef, s: *const c_char, encoding: u32) -> CFRef;
    fn CFStringGetCString(s: CFRef, buf: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFNumberGetValue(n: CFRef, ty: isize, out: *mut c_void) -> u8;
    fn CFBooleanGetValue(b: CFRef) -> u8;
    fn CFRelease(r: CFRef);
}

const UTF8: u32 = 0x0800_0100;
const SINT64: isize = 4;

/// owned CFString for dictionary lookups; released on drop
struct CfKey(CFRef);

impl CfKey {
    fn new(s: &str) -> Option<Self> {
        let c = std::ffi::CString::new(s).ok()?;
        // SAFETY: c is a valid NUL-terminated string; null allocator = default
        let r = unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) };
        (!r.is_null()).then_some(Self(r))
    }
}

impl Drop for CfKey {
    fn drop(&mut self) {
        // SAFETY: self.0 came from a Create call and is non-null
        unsafe { CFRelease(self.0) };
    }
}

fn dict_get(d: CFRef, key: &str) -> CFRef {
    let Some(k) = CfKey::new(key) else {
        return std::ptr::null();
    };
    // SAFETY: d is a live CFDictionary, k.0 a live CFString; get does not consume
    unsafe { CFDictionaryGetValue(d, k.0) }
}

fn num(v: CFRef) -> Option<u64> {
    signed_num(v).map(|n| n as u64)
}

/// Amperage is negative while discharging; keep the sign
fn signed_num(v: CFRef) -> Option<i64> {
    if v.is_null() {
        return None;
    }
    let mut out: i64 = 0;
    // SAFETY: v is a CFNumber; out matches SINT64
    let ok = unsafe { CFNumberGetValue(v, SINT64, &mut out as *mut _ as *mut c_void) };
    (ok != 0).then_some(out)
}

fn boolean(v: CFRef) -> Option<bool> {
    // SAFETY: v is a CFBoolean when present; checked non-null
    (!v.is_null()).then(|| unsafe { CFBooleanGetValue(v) } != 0)
}

fn string(v: CFRef) -> Option<String> {
    if v.is_null() {
        return None;
    }
    let mut buf = [0 as c_char; 128];
    // SAFETY: buf is 128 bytes; CFStringGetCString NUL-terminates on success
    let ok = unsafe { CFStringGetCString(v, buf.as_mut_ptr(), buf.len() as isize, UTF8) };
    if ok == 0 {
        return None;
    }
    // SAFETY: buf is NUL-terminated per above
    Some(
        unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
    )
}

/// properties of one registry entry; released on drop
struct Props(CFRef);

impl Props {
    fn of(entry: IoObject) -> Option<Self> {
        let mut p: CFRef = std::ptr::null();
        // SAFETY: entry is a live registry object; p is an out-param we own on success
        let kr = unsafe { IORegistryEntryCreateCFProperties(entry, &mut p, std::ptr::null(), 0) };
        (kr == 0 && !p.is_null()).then_some(Self(p))
    }
}

impl Drop for Props {
    fn drop(&mut self) {
        // SAFETY: self.0 came from CreateCFProperties and is non-null
        unsafe { CFRelease(self.0) };
    }
}

pub fn disks() -> Result<Vec<DiskStats>, CollectError> {
    // SAFETY: literal service name; the matching dict is consumed by GetMatchingServices
    let matching = unsafe { IOServiceMatching(c"IOMedia".as_ptr()) };
    let mut it: IoObject = 0;
    // SAFETY: port 0 = kIOMainPortDefault; it is an out-param
    let kr = unsafe { IOServiceGetMatchingServices(0, matching, &mut it) };
    if kr != 0 {
        return Err(CollectError::Sys {
            call: "IOServiceGetMatchingServices",
            code: kr as i64,
        });
    }

    let mut out = Vec::new();
    loop {
        // SAFETY: it is a live iterator from the call above
        let media = unsafe { IOIteratorNext(it) };
        if media == 0 {
            break;
        }
        if let Some(mprops) = Props::of(media) {
            let whole = dict_get(mprops.0, "Whole");
            // SAFETY: whole is a CFBoolean when present; checked non-null
            if !whole.is_null() && unsafe { CFBooleanGetValue(whole) } != 0 {
                let name = string(dict_get(mprops.0, "BSD Name")).unwrap_or_else(|| "disk?".into());
                let mut drv: IoObject = 0;
                // stats live on the parent IOBlockStorageDriver
                // SAFETY: media is live; drv is an out-param we release below
                if unsafe { IORegistryEntryGetParentEntry(media, c"IOService".as_ptr(), &mut drv) }
                    == 0
                {
                    if let Some(dprops) = Props::of(drv) {
                        let stats = dict_get(dprops.0, "Statistics");
                        if !stats.is_null() {
                            let read_time = num(dict_get(stats, "Total Time (Read)"));
                            let write_time = num(dict_get(stats, "Total Time (Write)"));
                            let io_time = match (read_time, write_time) {
                                (Some(r), Some(w)) => Some(r + w),
                                _ => None, // synthetic apfs disks lack time counters
                            };
                            out.push(DiskStats {
                                name,
                                read_bytes: num(dict_get(stats, "Bytes (Read)")).unwrap_or(0),
                                written_bytes: num(dict_get(stats, "Bytes (Write)")).unwrap_or(0),
                                read_ops: num(dict_get(stats, "Operations (Read)")).unwrap_or(0),
                                write_ops: num(dict_get(stats, "Operations (Write)")).unwrap_or(0),
                                busy_time_ns: io_time,
                                io_time_ns: io_time,
                                weighted_ns: None,
                            });
                        }
                    }
                    // SAFETY: drv obtained above
                    unsafe { IOObjectRelease(drv) };
                }
            }
        }
        // SAFETY: media obtained from IOIteratorNext
        unsafe { IOObjectRelease(media) };
    }
    // SAFETY: it obtained from GetMatchingServices
    unsafe { IOObjectRelease(it) };
    Ok(out)
}

/// AppleSmartBattery registry entry; desktops have none -> None.
/// Property names verified with `ioreg -rn AppleSmartBattery` on an M1.
pub fn battery() -> Option<BatterySnapshot> {
    // SAFETY: literal service name; the matching dict is consumed by GetMatchingServices
    let matching = unsafe { IOServiceMatching(c"AppleSmartBattery".as_ptr()) };
    let mut it: IoObject = 0;
    // SAFETY: port 0 = kIOMainPortDefault; it is an out-param
    if unsafe { IOServiceGetMatchingServices(0, matching, &mut it) } != 0 {
        return None;
    }
    let mut out = None;
    loop {
        // SAFETY: it is a live iterator from the call above
        let svc = unsafe { IOIteratorNext(it) };
        if svc == 0 {
            break;
        }
        if out.is_none()
            && let Some(props) = Props::of(svc)
        {
            out = read_battery(props.0);
        }
        // SAFETY: svc obtained from IOIteratorNext
        unsafe { IOObjectRelease(svc) };
    }
    // SAFETY: it obtained from GetMatchingServices
    unsafe { IOObjectRelease(it) };
    out
}

fn read_battery(d: CFRef) -> Option<BatterySnapshot> {
    // apple silicon reports MaxCapacity=100 (so CurrentCapacity is already a
    // percent); intel machines report raw mAh — the ratio covers both
    let current = num(dict_get(d, "CurrentCapacity"))? as f64;
    let max = num(dict_get(d, "MaxCapacity")).filter(|m| *m > 0)? as f64;
    let percent = (current * 100.0 / max).clamp(0.0, 100.0);

    let charging = boolean(dict_get(d, "IsCharging")).unwrap_or(false);
    let full = boolean(dict_get(d, "FullyCharged")).unwrap_or(false);
    let external = boolean(dict_get(d, "ExternalConnected")).unwrap_or(false);
    // mA; negative = current actually leaving the battery
    let amperage = signed_num(dict_get(d, "Amperage"));
    let state = if charging {
        BatteryState::Charging
    } else if !external || amperage.is_some_and(|a| a < 0) {
        // on battery, or on ac with the load outrunning the adapter
        BatteryState::Discharging
    } else if full {
        BatteryState::Full
    } else {
        // on ac, zero current: charge limiter or thermal hold
        BatteryState::Unknown
    };

    // minutes to empty or to full depending on state; 65535 = still calculating
    let secs_left = num(dict_get(d, "TimeRemaining"))
        .filter(|m| (1..0xFFFF).contains(m))
        .map(|m| m * 60);
    // mA * mV / 1e6 = W
    let watts = match (amperage, signed_num(dict_get(d, "Voltage"))) {
        (Some(a), Some(v)) => Some((a as f64 * v as f64 / 1e6).abs()),
        _ => None,
    };

    Some(BatterySnapshot {
        percent,
        state,
        secs_left,
        watts,
    })
}
