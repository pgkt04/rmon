//! cpu identity, die temperature, and gpu utilization for macos.
//!
//! temperature uses the PRIVATE IOHIDEventSystemClient API (the same one btop
//! uses); gpu utilization reads the public IOAccelerator registry entry's
//! PerformanceStatistics dictionary. signatures below were verified by a live
//! FFI probe on this machine — do not "correct" them from public headers.
//!
//! the IOHID client is recreated per call rather than cached: the collector is
//! a `Send` unit struct, and holding a raw private-API handle across ticks
//! would need an unfounded `Send` assertion; creation costs ~1ms per tick,
//! which is noise at the collection rate.

use libc::{c_char, c_int, c_uint, c_void, sysctlbyname};

type CFRef = *const c_void;
type IoObject = c_uint;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    // public registry API
    fn IOServiceMatching(name: *const c_char) -> CFRef;
    fn IOServiceGetMatchingServices(port: c_uint, matching: CFRef, it: *mut IoObject) -> c_int;
    fn IOIteratorNext(it: IoObject) -> IoObject;
    fn IOObjectRelease(obj: IoObject) -> c_int;
    fn IORegistryEntryCreateCFProperty(
        entry: IoObject,
        key: CFRef,
        allocator: CFRef,
        options: c_uint,
    ) -> CFRef;
    // private IOHID event system (sensor services)
    fn IOHIDEventSystemClientCreate(alloc: CFRef) -> CFRef;
    fn IOHIDEventSystemClientSetMatching(client: CFRef, dict: CFRef) -> c_int;
    fn IOHIDEventSystemClientCopyServices(client: CFRef) -> CFRef;
    fn IOHIDServiceClientCopyProperty(sc: CFRef, key: CFRef) -> CFRef;
    fn IOHIDServiceClientCopyEvent(sc: CFRef, kind: i64, options: i32, timeout: i64) -> CFRef;
    fn IOHIDEventGetFloatValue(ev: CFRef, field: i32) -> f64;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(alloc: CFRef, s: *const c_char, encoding: u32) -> CFRef;
    fn CFStringGetCString(s: CFRef, buf: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFNumberCreate(alloc: CFRef, ty: isize, value: *const c_void) -> CFRef;
    fn CFNumberGetValue(n: CFRef, ty: isize, out: *mut c_void) -> u8;
    fn CFDictionaryCreate(
        alloc: CFRef,
        keys: *const CFRef,
        values: *const CFRef,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFRef;
    fn CFDictionaryGetValue(d: CFRef, key: CFRef) -> CFRef;
    fn CFArrayGetCount(a: CFRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFRef, i: isize) -> CFRef;
    fn CFRelease(r: CFRef);
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

const UTF8: u32 = 0x0800_0100;
const SINT32: isize = 3;
const SINT64: isize = 4;
/// IOHIDEventTypeTemperature
const EVENT_TEMPERATURE: i64 = 15;
/// IOHIDEventFieldTemperatureLevel = type << 16
const TEMPERATURE_FIELD: i32 = 15 << 16;
/// sensor services match {PrimaryUsagePage: 0xff00, PrimaryUsage: 5}
const SENSOR_USAGE_PAGE: i32 = 0xff00;
const SENSOR_USAGE: i32 = 5;

/// owned CF object from a Copy/Create call; released exactly once on drop
struct Cf(CFRef);

impl Cf {
    fn new(r: CFRef) -> Option<Self> {
        // then (not then_some): eager construction would drop-release a null
        (!r.is_null()).then(|| Self(r))
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        // SAFETY: self.0 came from a CF Copy/Create call, is non-null, and
        // this drop is its single release
        unsafe { CFRelease(self.0) };
    }
}

fn cfstr(s: &str) -> Option<Cf> {
    let c = std::ffi::CString::new(s).ok()?;
    // SAFETY: c is a valid NUL-terminated string; null allocator = default
    Cf::new(unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), UTF8) })
}

fn cfnum_i32(v: i32) -> Option<Cf> {
    // SAFETY: SINT32 tells CFNumberCreate the pointee is a 32-bit int, which
    // matches &v exactly
    Cf::new(unsafe { CFNumberCreate(std::ptr::null(), SINT32, (&raw const v).cast()) })
}

/// rust string from a (borrowed or owned) CFString ref
fn cf_string(v: CFRef) -> Option<String> {
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

/// `machdep.cpu.brand_string`, e.g. "Apple M1 Pro"
pub fn cpu_brand() -> Option<String> {
    let mut buf = [0u8; 256];
    let mut len = buf.len();
    // SAFETY: name is NUL-terminated; buf/len describe a real buffer and the
    // kernel writes at most len bytes including the trailing NUL
    let rc = unsafe {
        sysctlbyname(
            c"machdep.cpu.brand_string".as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let s = std::str::from_utf8(&buf[..end]).ok()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// IOHID client matched to the sensor usage page, ready for CopyServices
fn hid_sensor_client() -> Option<Cf> {
    let keys = [cfstr("PrimaryUsagePage")?, cfstr("PrimaryUsage")?];
    let vals = [cfnum_i32(SENSOR_USAGE_PAGE)?, cfnum_i32(SENSOR_USAGE)?];
    let raw_keys = [keys[0].0, keys[1].0];
    let raw_vals = [vals[0].0, vals[1].0];
    // SAFETY: raw_keys/raw_vals are live CF refs in arrays of length 2; the
    // kCFType callbacks make the dict retain them, so our Cf drops stay
    // balanced (one release per Create)
    let dict = Cf::new(unsafe {
        CFDictionaryCreate(
            std::ptr::null(),
            raw_keys.as_ptr(),
            raw_vals.as_ptr(),
            2,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        )
    })?;
    // SAFETY: null allocator = default; Create -> we own the client
    let client = Cf::new(unsafe { IOHIDEventSystemClientCreate(std::ptr::null()) })?;
    // SAFETY: client and dict are both live; SetMatching retains the dict.
    // its return value is junk on this OS (live probe read 16824288), so a
    // failed match is detected downstream as an empty services array instead
    let _ = unsafe { IOHIDEventSystemClientSetMatching(client.0, dict.0) };
    Some(client)
}

fn avg(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

/// cpu die temperature in °C: average of `PMU tdie*` sensors, falling back to
/// `eACC*`/`pACC*`, then `SOC MTR Temp Sensor*` (btop's chain)
pub fn cpu_temp() -> Option<f64> {
    let client = hid_sensor_client()?;
    // SAFETY: client is live; Copy -> we own the services array
    let services = Cf::new(unsafe { IOHIDEventSystemClientCopyServices(client.0) })?;
    let product_key = cfstr("Product")?;
    // SAFETY: services is a live CFArray
    let count = unsafe { CFArrayGetCount(services.0) };
    let mut tdie = Vec::new();
    let mut acc = Vec::new();
    let mut soc = Vec::new();
    for i in 0..count {
        // SAFETY: i < count; GetValueAtIndex returns a borrowed service ref
        let sc = unsafe { CFArrayGetValueAtIndex(services.0, i) };
        if sc.is_null() {
            continue;
        }
        // SAFETY: sc and product_key are live; Copy -> owned, dropped by Cf
        let Some(product) = Cf::new(unsafe { IOHIDServiceClientCopyProperty(sc, product_key.0) })
        else {
            continue;
        };
        let Some(name) = cf_string(product.0) else {
            continue;
        };
        let bucket = if name.starts_with("PMU tdie") {
            &mut tdie
        } else if name.starts_with("eACC") || name.starts_with("pACC") {
            &mut acc
        } else if name.starts_with("SOC MTR Temp Sensor") {
            &mut soc
        } else {
            continue;
        };
        // SAFETY: sc is live; Copy -> owned event, dropped by Cf
        let Some(ev) = Cf::new(unsafe { IOHIDServiceClientCopyEvent(sc, EVENT_TEMPERATURE, 0, 0) })
        else {
            continue;
        };
        // SAFETY: ev is a live temperature event; the field id is type << 16
        let t = unsafe { IOHIDEventGetFloatValue(ev.0, TEMPERATURE_FIELD) };
        if t > 0.0 && t < 150.0 {
            bucket.push(t);
        }
    }
    avg(&tdie).or_else(|| avg(&acc)).or_else(|| avg(&soc))
}

/// "Device Utilization %" from the entry's PerformanceStatistics, if present
fn accel_util(entry: IoObject, stats_key: &Cf, util_key: &Cf) -> Option<f64> {
    // SAFETY: entry is a live registry object; CreateCFProperty -> owned
    let stats = Cf::new(unsafe {
        IORegistryEntryCreateCFProperty(entry, stats_key.0, std::ptr::null(), 0)
    })?;
    // SAFETY: stats is a live dict; GetValue borrows, never released by us
    let v = unsafe { CFDictionaryGetValue(stats.0, util_key.0) };
    if v.is_null() {
        return None;
    }
    let mut out: i64 = 0;
    // SAFETY: SINT64 matches the i64 out pointee
    let ok = unsafe { CFNumberGetValue(v, SINT64, (&raw mut out).cast()) };
    (ok != 0).then_some(out as f64)
}

/// gpu utilization percent from the first IOAccelerator service reporting it
pub fn gpu_util() -> Option<f64> {
    let stats_key = cfstr("PerformanceStatistics")?;
    let util_key = cfstr("Device Utilization %")?;
    // SAFETY: literal service name; the matching dict is consumed by
    // IOServiceGetMatchingServices even on failure
    let matching = unsafe { IOServiceMatching(c"IOAccelerator".as_ptr()) };
    if matching.is_null() {
        return None;
    }
    let mut it: IoObject = 0;
    // SAFETY: matching is live and consumed here; it is an out-param
    let kr = unsafe { IOServiceGetMatchingServices(0, matching, &mut it) };
    if kr != 0 || it == 0 {
        return None;
    }
    let mut util = None;
    loop {
        // SAFETY: it is a live iterator; Next returns a retained entry or 0
        let entry = unsafe { IOIteratorNext(it) };
        if entry == 0 {
            break;
        }
        util = accel_util(entry, &stats_key, &util_key);
        // SAFETY: entry came from IOIteratorNext and is ours to release
        unsafe { IOObjectRelease(entry) };
        if util.is_some() {
            break;
        }
    }
    // SAFETY: the iterator came from GetMatchingServices and is ours to release
    unsafe { IOObjectRelease(it) };
    util
}

#[cfg(test)]
mod tests {
    use super::*;

    // live tests: this dev machine is an apple-silicon mac with PMU tdie
    // sensors and an IOAccelerator that reports Device Utilization %

    #[test]
    fn brand_is_apple_silicon() {
        let b = cpu_brand().unwrap();
        assert!(b.starts_with("Apple"), "unexpected brand: {b}");
    }

    #[test]
    fn cpu_temp_is_plausible() {
        let t = cpu_temp().unwrap();
        assert!((20.0..120.0).contains(&t), "implausible cpu temp: {t}");
    }

    #[test]
    fn gpu_util_is_percent() {
        let u = gpu_util().unwrap();
        assert!((0.0..=100.0).contains(&u), "implausible gpu util: {u}");
    }
}
