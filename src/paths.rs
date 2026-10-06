//! Where the mod's own folder is: next to the DLL, wherever the game loaded it from
//! (`<game>/mods/smart_position_lock/` or `steamapps/workshop/content/3009300/<id>/`).
//! `settings.ini`, `positions.json` and `diag.log` live there.

use std::path::PathBuf;
use std::sync::OnceLock;

/// Tests (and players who want the files elsewhere) can point the mod at another folder.
pub const DIR_ENV: &str = "SMART_POSITION_LOCK_DIR";
/// Tests point the mod at another `mods.json` (see `compat`).
pub const MODS_JSON_ENV: &str = "SMART_POSITION_LOCK_MODS_JSON";

static DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn mod_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os(DIR_ENV) {
        return PathBuf::from(dir);
    }
    DIR.get_or_init(|| {
        module_path()
            .and_then(|dll| dll.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("mods").join(crate::MOD_ID))
    })
    .clone()
}

/// An address inside this module, to ask the loader which file it came from.
fn anchor() {}

#[cfg(windows)]
fn module_path() -> Option<PathBuf> {
    use std::ffi::{c_void, OsString};
    use std::os::windows::ffi::OsStringExt;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleExW(flags: u32, name: *const u16, module: *mut *mut c_void) -> i32;
        fn GetModuleFileNameW(module: *mut c_void, filename: *mut u16, size: u32) -> u32;
    }
    const FROM_ADDRESS: u32 = 0x4;
    const UNCHANGED_REFCOUNT: u32 = 0x2;

    let mut module: *mut c_void = std::ptr::null_mut();
    // SAFETY: FROM_ADDRESS takes any address inside a loaded module; the handle is not
    // reference-counted, so nothing has to be released.
    let ok = unsafe {
        GetModuleHandleExW(FROM_ADDRESS | UNCHANGED_REFCOUNT, anchor as *const u16, &mut module)
    };
    if ok == 0 || module.is_null() {
        return None;
    }
    let mut buf = vec![0u16; 32768];
    // SAFETY: the buffer holds `buf.len()` UTF-16 units.
    let len = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if len == 0 || len >= buf.len() {
        return None;
    }
    Some(PathBuf::from(OsString::from_wide(&buf[..len])))
}

#[cfg(unix)]
fn module_path() -> Option<PathBuf> {
    use std::ffi::{c_char, c_int, c_void, CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;

    #[repr(C)]
    struct DlInfo {
        dli_fname: *const c_char,
        dli_fbase: *mut c_void,
        dli_sname: *const c_char,
        dli_saddr: *mut c_void,
    }
    #[cfg_attr(target_os = "linux", link(name = "dl"))]
    extern "C" {
        fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int;
    }
    let mut info = DlInfo {
        dli_fname: std::ptr::null(),
        dli_fbase: std::ptr::null_mut(),
        dli_sname: std::ptr::null(),
        dli_saddr: std::ptr::null_mut(),
    };
    // SAFETY: dladdr only reads the address and fills `info`.
    if unsafe { dladdr(anchor as *const c_void, &mut info) } == 0 || info.dli_fname.is_null() {
        return None;
    }
    // SAFETY: dli_fname is a NUL-terminated path owned by the loader.
    let bytes = unsafe { CStr::from_ptr(info.dli_fname) }.to_bytes();
    Some(PathBuf::from(OsStr::from_bytes(bytes)))
}

#[cfg(not(any(windows, unix)))]
fn module_path() -> Option<PathBuf> {
    None
}
