//! Raw libmpv: the library is opened at run time (`libmpv-2.dll`, `libmpv.so.2`, ...),
//! so building the app needs no mpv SDK and the app starts even when the library is missing.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::path::PathBuf;

use libloading::Library;

use crate::Error;

#[repr(C)]
pub struct RawEvent {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct RawProperty {
    pub name: *const c_char,
    pub format: c_int,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct RawEndFile {
    pub reason: c_int,
    pub error: c_int,
}

#[repr(C)]
pub struct RawClientMessage {
    pub num_args: c_int,
    pub args: *const *const c_char,
}

pub const FORMAT_NONE: c_int = 0;
pub const FORMAT_STRING: c_int = 1;
pub const FORMAT_FLAG: c_int = 3;
pub const FORMAT_INT64: c_int = 4;
pub const FORMAT_DOUBLE: c_int = 5;

pub const EV_SHUTDOWN: c_int = 1;
pub const EV_START_FILE: c_int = 6;
pub const EV_END_FILE: c_int = 7;
pub const EV_FILE_LOADED: c_int = 8;
pub const EV_CLIENT_MESSAGE: c_int = 16;
pub const EV_PLAYBACK_RESTART: c_int = 21;
pub const EV_PROPERTY_CHANGE: c_int = 22;

/// The libmpv functions the player uses.
pub struct Api {
    pub create: unsafe extern "C" fn() -> *mut c_void,
    pub initialize: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub terminate_destroy: unsafe extern "C" fn(*mut c_void),
    pub set_option_string: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    pub set_property_string: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int,
    pub get_property_string: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_char,
    pub command: unsafe extern "C" fn(*mut c_void, *mut *const c_char) -> c_int,
    pub observe_property: unsafe extern "C" fn(*mut c_void, u64, *const c_char, c_int) -> c_int,
    pub wait_event: unsafe extern "C" fn(*mut c_void, f64) -> *mut RawEvent,
    pub wakeup: unsafe extern "C" fn(*mut c_void),
    pub free: unsafe extern "C" fn(*mut c_void),
    pub error_string: unsafe extern "C" fn(c_int) -> *const c_char,
    // Keeps the library mapped for as long as the function pointers above are used.
    _lib: Library,
}

fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var("KINO_LIBMPV") {
        out.push(PathBuf::from(p));
    }
    // Next to the executable first: the installer puts the DLL there.
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)) {
        for name in names() {
            out.push(dir.join(name));
        }
    }
    out.extend(names().iter().map(PathBuf::from));
    out
}

fn names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["libmpv-2.dll", "mpv-2.dll", "mpv-1.dll"]
    } else if cfg!(target_os = "macos") {
        &[
            "libmpv.2.dylib",
            "libmpv.dylib",
            "/opt/homebrew/lib/libmpv.dylib",
            "/usr/local/lib/libmpv.dylib",
        ]
    } else {
        &["libmpv.so.2", "libmpv.so.1", "libmpv.so"]
    }
}

macro_rules! sym {
    ($lib:expr, $name:literal) => {
        // SAFETY: the signature matches libmpv's client.h.
        *unsafe { $lib.get($name) }.map_err(|e| Error::Load(e.to_string()))?
    };
}

impl Api {
    pub fn load() -> Result<Self, Error> {
        let mut last = String::from("libmpv not found");
        for path in candidates() {
            // SAFETY: loading libmpv runs its initialisers, which are harmless.
            match unsafe { Library::new(&path) } {
                Ok(lib) => return Self::from_library(lib),
                Err(e) => last = format!("{}: {e}", path.display()),
            }
        }
        Err(Error::Load(last))
    }

    fn from_library(lib: Library) -> Result<Self, Error> {
        Ok(Self {
            create: sym!(lib, b"mpv_create\0"),
            initialize: sym!(lib, b"mpv_initialize\0"),
            terminate_destroy: sym!(lib, b"mpv_terminate_destroy\0"),
            set_option_string: sym!(lib, b"mpv_set_option_string\0"),
            set_property_string: sym!(lib, b"mpv_set_property_string\0"),
            get_property_string: sym!(lib, b"mpv_get_property_string\0"),
            command: sym!(lib, b"mpv_command\0"),
            observe_property: sym!(lib, b"mpv_observe_property\0"),
            wait_event: sym!(lib, b"mpv_wait_event\0"),
            wakeup: sym!(lib, b"mpv_wakeup\0"),
            free: sym!(lib, b"mpv_free\0"),
            error_string: sym!(lib, b"mpv_error_string\0"),
            _lib: lib,
        })
    }

    pub fn error_text(&self, code: c_int) -> String {
        // SAFETY: mpv_error_string returns a static string.
        let p = unsafe { (self.error_string)(code) };
        if p.is_null() {
            return format!("mpv error {code}");
        }
        // SAFETY: non-null, NUL-terminated.
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

#[cfg(not(windows))]
extern "C" {
    fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
}

/// libmpv refuses to start unless numbers are parsed in the "C" locale (GTK changes it on Linux).
pub fn set_c_numeric_locale() {
    #[cfg(not(windows))]
    {
        const LC_NUMERIC: c_int = if cfg!(target_os = "macos") { 4 } else { 1 };
        // SAFETY: plain libc call with a static string.
        unsafe { setlocale(LC_NUMERIC, c"C".as_ptr()) };
    }
}
