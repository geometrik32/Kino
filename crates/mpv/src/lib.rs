//! A small safe wrapper over libmpv, plus [`Session`]: one playback of one video
//! in mpv's own window, reporting position and track changes over a channel.

mod session;
mod sys;

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::Arc;

pub use session::{PlayOptions, PlayerEvent, Session, Track, TrackKind};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// libmpv is not installed next to the app (or not at all).
    #[error("не найден проигрыватель libmpv: {0}")]
    Load(String),
    #[error("{0}")]
    Mpv(String),
}

/// A property value delivered with [`Event::Property`].
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    None,
    Flag(bool),
    Int(i64),
    Double(f64),
    Str(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum EndReason {
    /// Played to the end.
    Eof,
    /// Stopped by a command or the window was closed.
    Stopped,
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Nothing happened within the wait.
    Timeout,
    Shutdown,
    StartFile,
    FileLoaded,
    PlaybackRestart,
    EndFile(EndReason),
    Property {
        name: String,
        value: Value,
    },
    /// `script-message` sent by a key binding.
    ClientMessage(Vec<String>),
    Other(i32),
}

/// An initialised mpv instance. Safe to share between threads (libmpv is thread-safe).
pub struct Mpv {
    api: sys::Api,
    handle: *mut c_void,
}

// SAFETY: libmpv's client API may be called from any thread.
unsafe impl Send for Mpv {}
unsafe impl Sync for Mpv {}

fn cstr(s: &str) -> CString {
    // Interior NULs cannot be passed to C; drop them.
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

impl Mpv {
    /// Creates an instance; `options` are set before initialisation.
    pub fn new(options: &[(&str, &str)]) -> Result<Arc<Self>, Error> {
        sys::set_c_numeric_locale();
        let api = sys::Api::load()?;
        // SAFETY: plain constructor.
        let handle = unsafe { (api.create)() };
        if handle.is_null() {
            return Err(Error::Mpv("mpv_create вернул пустой указатель".into()));
        }
        let mpv = Self { api, handle };
        for (k, v) in options {
            mpv.set_option(k, v)?;
        }
        // SAFETY: handle is valid.
        let rc = unsafe { (mpv.api.initialize)(mpv.handle) };
        mpv.check(rc)?;
        Ok(Arc::new(mpv))
    }

    fn check(&self, rc: i32) -> Result<(), Error> {
        if rc < 0 {
            Err(Error::Mpv(self.api.error_text(rc)))
        } else {
            Ok(())
        }
    }

    /// Option, before initialisation (afterwards it behaves like [`Mpv::set_property`]).
    pub fn set_option(&self, name: &str, value: &str) -> Result<(), Error> {
        let (n, v) = (cstr(name), cstr(value));
        // SAFETY: valid handle and C strings.
        let rc = unsafe { (self.api.set_option_string)(self.handle, n.as_ptr(), v.as_ptr()) };
        self.check(rc).map_err(|e| Error::Mpv(format!("{name}={value}: {e}")))
    }

    pub fn set_property(&self, name: &str, value: &str) -> Result<(), Error> {
        let (n, v) = (cstr(name), cstr(value));
        // SAFETY: valid handle and C strings.
        let rc = unsafe { (self.api.set_property_string)(self.handle, n.as_ptr(), v.as_ptr()) };
        self.check(rc).map_err(|e| Error::Mpv(format!("{name}={value}: {e}")))
    }

    pub fn property(&self, name: &str) -> Option<String> {
        let n = cstr(name);
        // SAFETY: valid handle and C string; the result is freed with mpv_free.
        let p = unsafe { (self.api.get_property_string)(self.handle, n.as_ptr()) };
        if p.is_null() {
            return None;
        }
        // SAFETY: non-null NUL-terminated string owned by mpv.
        let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
        // SAFETY: allocated by mpv.
        unsafe { (self.api.free)(p.cast()) };
        Some(s)
    }

    pub fn property_f64(&self, name: &str) -> Option<f64> {
        self.property(name)?.parse().ok()
    }

    pub fn property_i64(&self, name: &str) -> Option<i64> {
        self.property(name)?.parse().ok()
    }

    /// Runs a command, e.g. `command(&["loadfile", url])`.
    pub fn command(&self, args: &[&str]) -> Result<(), Error> {
        let owned: Vec<CString> = args.iter().map(|a| cstr(a)).collect();
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        // SAFETY: NULL-terminated array of valid C strings.
        let rc = unsafe { (self.api.command)(self.handle, ptrs.as_mut_ptr()) };
        self.check(rc)
            .map_err(|e| Error::Mpv(format!("{}: {e}", args.join(" "))))
    }

    /// Asks for [`Event::Property`] whenever the property changes.
    pub fn observe(&self, name: &str, id: u64, format: Format) -> Result<(), Error> {
        let n = cstr(name);
        // SAFETY: valid handle and C string.
        let rc = unsafe { (self.api.observe_property)(self.handle, id, n.as_ptr(), format as i32) };
        self.check(rc)
    }

    /// Waits up to `timeout` seconds for the next event.
    pub fn wait_event(&self, timeout: f64) -> Event {
        // SAFETY: the returned event stays valid until the next wait_event call.
        let ev = unsafe { &*(self.api.wait_event)(self.handle, timeout) };
        match ev.event_id {
            0 => Event::Timeout,
            sys::EV_SHUTDOWN => Event::Shutdown,
            sys::EV_START_FILE => Event::StartFile,
            sys::EV_FILE_LOADED => Event::FileLoaded,
            sys::EV_PLAYBACK_RESTART => Event::PlaybackRestart,
            sys::EV_END_FILE => {
                // SAFETY: END_FILE carries an mpv_event_end_file.
                let end = unsafe { &*(ev.data as *const sys::RawEndFile) };
                Event::EndFile(match end.reason {
                    0 => EndReason::Eof,
                    4 => EndReason::Error(self.api.error_text(end.error)),
                    _ => EndReason::Stopped,
                })
            }
            sys::EV_PROPERTY_CHANGE => {
                // SAFETY: PROPERTY_CHANGE carries an mpv_event_property.
                let p = unsafe { &*(ev.data as *const sys::RawProperty) };
                // SAFETY: name is a valid C string.
                let name = unsafe { CStr::from_ptr(p.name) }.to_string_lossy().into_owned();
                let value = if p.data.is_null() {
                    Value::None
                } else {
                    // SAFETY: data points to the type announced in `format`.
                    unsafe {
                        match p.format {
                            sys::FORMAT_FLAG => Value::Flag(*(p.data as *const i32) != 0),
                            sys::FORMAT_INT64 => Value::Int(*(p.data as *const i64)),
                            sys::FORMAT_DOUBLE => Value::Double(*(p.data as *const f64)),
                            sys::FORMAT_STRING => {
                                let s = *(p.data as *const *const c_char);
                                if s.is_null() {
                                    Value::None
                                } else {
                                    Value::Str(CStr::from_ptr(s).to_string_lossy().into_owned())
                                }
                            }
                            _ => Value::None,
                        }
                    }
                };
                Event::Property { name, value }
            }
            sys::EV_CLIENT_MESSAGE => {
                // SAFETY: CLIENT_MESSAGE carries an mpv_event_client_message.
                let m = unsafe { &*(ev.data as *const sys::RawClientMessage) };
                let args = (0..m.num_args.max(0) as usize)
                    // SAFETY: args has num_args valid C strings.
                    .map(|i| unsafe { CStr::from_ptr(*m.args.add(i)) }.to_string_lossy().into_owned())
                    .collect();
                Event::ClientMessage(args)
            }
            other => Event::Other(other),
        }
    }

    /// Makes a blocked [`Mpv::wait_event`] return.
    pub fn wakeup(&self) {
        // SAFETY: valid handle.
        unsafe { (self.api.wakeup)(self.handle) }
    }
}

impl Drop for Mpv {
    fn drop(&mut self) {
        // SAFETY: the handle is not used after this.
        unsafe { (self.api.terminate_destroy)(self.handle) }
    }
}

/// How an observed property is delivered.
#[derive(Debug, Clone, Copy)]
#[repr(i32)]
pub enum Format {
    Flag = sys::FORMAT_FLAG,
    Int = sys::FORMAT_INT64,
    Double = sys::FORMAT_DOUBLE,
    Str = sys::FORMAT_STRING,
    None = sys::FORMAT_NONE,
}
