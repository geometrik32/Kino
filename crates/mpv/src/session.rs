//! One playback window. The caller starts a [`Session`], gets [`PlayerEvent`]s and may
//! [`load`](Session::load) the next file into the same window.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::{EndReason, Error, Event, Format, Mpv, Value};

/// Seconds before the end when "next episode" is offered.
const NEXT_PROMPT_SEC: f64 = 40.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackKind {
    Audio,
    Subtitle,
}

/// An audio or subtitle track of the loaded file.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub kind: TrackKind,
    /// Position among tracks of the same kind, from 0 (the numbering of `MediaTrack::index`).
    pub index: usize,
    /// mpv's own track number.
    pub id: i64,
    pub language: Option<String>,
    pub title: Option<String>,
    pub codec: Option<String>,
}

/// What to play and how to start.
#[derive(Debug, Clone)]
pub struct PlayOptions {
    pub url: String,
    /// Shown in the window title and the on-screen controller.
    pub title: String,
    /// Resume position, seconds.
    pub start: f64,
    /// Audio track by index; `None` lets mpv choose (Russian first).
    pub audio: Option<usize>,
    /// Subtitle track by index; `None` shows no subtitles.
    pub subtitle: Option<usize>,
    /// "S02E07" when there is a next episode: offered near the end, `N` jumps to it.
    pub next: Option<String>,
    /// Whether playback starts immediately or waits in paused state.
    pub autoplay: bool,
    /// Initial playback speed (1.0 is normal).
    pub rate: f64,
    /// Initial volume in `0.0..=1.0`.
    pub volume: f64,
    /// Whether audio is muted initially.
    pub muted: bool,
    /// `1` = fit inside window, `2` = crop/fill proportionally (`panscan=1`).
    pub scale_mode: u8,
    /// Optional parent window handle (`HWND` on Windows) to embed video inside the host app window.
    pub wid: Option<isize>,
}

impl Default for PlayOptions {
    fn default() -> Self {
        Self {
            url: String::new(),
            title: String::new(),
            start: 0.0,
            audio: None,
            subtitle: None,
            next: None,
            autoplay: true,
            rate: 1.0,
            volume: 1.0,
            muted: false,
            scale_mode: 1,
            wid: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    /// The file is open: its length and tracks.
    Loaded {
        duration: Option<f64>,
        audio: Vec<Track>,
        subtitles: Vec<Track>,
    },
    /// Roughly once a second while playing.
    Position {
        position: f64,
        duration: Option<f64>,
    },
    Paused(bool),
    Buffering(bool),
    Seeked,
    /// The viewer picked other tracks in the player (`None`: subtitles off).
    Tracks {
        audio: Option<usize>,
        subtitle: Option<usize>,
    },
    /// The viewer asked for the next episode.
    NextRequested,
    /// Played to the end. The window stays open for [`Session::load`] or [`Session::close`].
    Ended,
    /// The window was closed (or [`Session::close`] called) at this position.
    Closed {
        position: f64,
    },
    Error(String),
}

#[derive(Default)]
struct Shared {
    next: Option<String>,
    prompted: bool,
    position: f64,
    duration: Option<f64>,
    audio_ids: Vec<i64>,
    subtitle_ids: Vec<i64>,
    /// Between `loadfile` and the file being open: old position events are ignored.
    loading: bool,
    /// Tracks to switch to once the file is open.
    wanted: Option<(Option<usize>, Option<usize>)>,
    /// Tracks last reported, to tell the viewer's choice from our own settings.
    reported: (Option<usize>, Option<usize>),
}

pub struct Session {
    mpv: Arc<Mpv>,
    shared: Arc<Mutex<Shared>>,
    closing: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Window and decoding settings; `headless` is for tests (no window, no sound).
fn base_options(headless: bool, wid: Option<isize>) -> Vec<(String, String)> {
    let mut o: Vec<(String, String)> = vec![
        ("config", "no"),
        ("idle", "yes"),
        ("keep-open", "no"),
        ("force-window", "yes"),
        ("osc", "yes"),
        ("osd-bar", "no"),
        ("input-default-bindings", "yes"),
        ("input-vo-keyboard", "yes"),
        ("input-media-keys", "yes"),
        ("window-dragging", "yes"),
        ("autofit", "75%"),
        ("autofit-larger", "90%x90%"),
        ("title", "${force-media-title} — Орловский кинопоказ"),
        // The GPU renderer with D3D11 on Windows, falling back to the older one.
        ("vo", "gpu-next,gpu"),
        ("hwdec", "auto-safe"),
        ("hr-seek", "yes"),
        ("alang", "rus,ru,eng,en"),
        ("slang", "rus,ru,eng,en"),
        ("sub-auto", "no"),
        ("sub-font-size", "46"),
        ("cache", "yes"),
        ("demuxer-max-bytes", "200MiB"),
        ("demuxer-readahead-secs", "120"),
        ("network-timeout", "30"),
        ("user-agent", "KinoDesktop"),
        ("save-position-on-quit", "no"),
        ("terminal", "no"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    if headless {
        o.retain(|(k, _)| !matches!(k.as_str(), "vo" | "force-window" | "osc"));
        o.push(("vo".into(), "null".into()));
        o.push(("ao".into(), "null".into()));
        o.push(("force-window".into(), "no".into()));
    } else if let Some(handle) = wid {
        // Embedded inside the host window: disable mpv's own OSC and window/input handling.
        o.retain(|(k, _)| {
            !matches!(
                k.as_str(),
                "osc"
                    | "input-default-bindings"
                    | "input-vo-keyboard"
                    | "window-dragging"
                    | "autofit"
                    | "autofit-larger"
                    | "title"
            )
        });
        o.push(("osc".into(), "no".into()));
        o.push(("input-default-bindings".into(), "no".into()));
        o.push(("input-vo-keyboard".into(), "no".into()));
        o.push(("input-cursor".into(), "no".into()));
        o.push(("cursor-autohide".into(), "no".into()));
        o.push(("window-dragging".into(), "no".into()));
        o.push(("keepaspect".into(), "yes".into()));
        o.push(("background-color".into(), "#000000".into()));
        o.push(("wid".into(), handle.to_string()));
    }
    o
}

impl Session {
    /// Opens the window and starts playing. `on_event` is called from a background thread.
    pub fn start(
        opts: PlayOptions,
        headless: bool,
        on_event: impl Fn(PlayerEvent) + Send + 'static,
    ) -> Result<Self, Error> {
        let mut options = base_options(headless, opts.wid);
        // For debugging: KINO_MPV_OPTS="vo=x11;hwdec=no" overrides the defaults.
        if let Ok(extra) = std::env::var("KINO_MPV_OPTS") {
            for pair in extra.split(';') {
                if let Some((k, v)) = pair.split_once('=') {
                    options.push((k.trim().to_owned(), v.trim().to_owned()));
                }
            }
        }
        let options: Vec<(&str, &str)> = options.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let mpv = Mpv::new(&options)?;
        mpv.observe("time-pos", 1, Format::Double)?;
        mpv.observe("duration", 2, Format::Double)?;
        mpv.observe("pause", 3, Format::Flag)?;
        mpv.observe("aid", 4, Format::Str)?;
        mpv.observe("sid", 5, Format::Str)?;
        mpv.observe("paused-for-cache", 6, Format::Flag)?;
        if opts.wid.is_none() {
            // Standalone fallback window: N (Shift+n) asks for the next episode.
            for key in ["n", "N"] {
                let _ = mpv.command(&["keybind", key, "script-message kino-next"]);
            }
        }
        let shared = Arc::new(Mutex::new(Shared::default()));
        let closing = Arc::new(AtomicBool::new(false));
        load_file(&mpv, &shared, &opts)?;
        let thread = {
            let (mpv, shared, closing) = (mpv.clone(), shared.clone(), closing.clone());
            std::thread::Builder::new()
                .name("kino-mpv".into())
                .spawn(move || event_loop(mpv, shared, closing, on_event))
                .map_err(|e| Error::Mpv(e.to_string()))?
        };
        Ok(Session {
            mpv,
            shared,
            closing,
            thread: Some(thread),
        })
    }

    /// Plays another file in the same window.
    pub fn load(&self, opts: PlayOptions) -> Result<(), Error> {
        load_file(&self.mpv, &self.shared, &opts)
    }

    pub fn pause(&self, paused: bool) {
        let _ = self.mpv.set_property("pause", if paused { "yes" } else { "no" });
    }

    pub fn seek(&self, sec: f64) {
        let target = sec.max(0.0);
        let _ = self.mpv.command(&["seek", &format!("{target:.3}"), "absolute"]);
    }

    pub fn set_rate(&self, rate: f64) {
        if rate.is_finite() {
            let _ = self.mpv.set_property("speed", &format!("{:.2}", rate.clamp(0.25, 4.0)));
        }
    }

    pub fn set_volume(&self, vol: f64) {
        if vol.is_finite() {
            let _ = self
                .mpv
                .set_property("volume", &format!("{:.0}", vol.clamp(0.0, 1.0) * 100.0));
        }
    }

    pub fn set_muted(&self, muted: bool) {
        let _ = self.mpv.set_property("mute", if muted { "yes" } else { "no" });
    }

    pub fn set_audio(&self, audio: Option<usize>) {
        let val = {
            let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            if s.loading {
                if let Some((ref mut want_a, _)) = s.wanted {
                    *want_a = audio;
                } else {
                    s.wanted = Some((audio, None));
                }
            }
            audio
                .and_then(|i| s.audio_ids.get(i).copied())
                .map_or_else(|| "auto".to_owned(), |id| id.to_string())
        };
        let _ = self.mpv.set_property("aid", &val);
    }

    pub fn set_subtitle(&self, subtitle: Option<usize>) {
        let val = {
            let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            if s.loading {
                if let Some((_, ref mut want_s)) = s.wanted {
                    *want_s = subtitle;
                } else {
                    s.wanted = Some((None, subtitle));
                }
            }
            subtitle
                .and_then(|i| s.subtitle_ids.get(i).copied())
                .map_or_else(|| "no".to_owned(), |id| id.to_string())
        };
        let _ = self.mpv.set_property("sid", &val);
    }

    pub fn set_scale(&self, mode: u8) {
        let panscan = if mode == 2 { "1.0" } else { "0" };
        let _ = self.mpv.set_property("panscan", panscan);
    }

    /// Shows a message over the video.
    pub fn osd(&self, text: &str) {
        let _ = self.mpv.command(&["show-text", text, "4000"]);
    }

    /// Closes the window; a [`PlayerEvent::Closed`] follows.
    pub fn close(&self) {
        self.closing.store(true, Ordering::SeqCst);
        let _ = self.mpv.command(&["quit"]);
        self.mpv.wakeup();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            // Never join the event loop thread from within itself (prevents self-join deadlock on Shutdown).
            if t.thread().id() == std::thread::current().id() {
                return;
            }
            self.close();
            let _ = t.join();
        }
    }
}

fn load_file(mpv: &Mpv, shared: &Mutex<Shared>, opts: &PlayOptions) -> Result<(), Error> {
    *shared.lock().unwrap_or_else(|e| e.into_inner()) = Shared {
        next: opts.next.clone(),
        loading: true,
        wanted: Some((opts.audio, opts.subtitle)),
        ..Shared::default()
    };
    mpv.set_property("start", &format!("{:.1}", opts.start.max(0.0)))?;
    mpv.set_property("pause", if opts.autoplay { "no" } else { "yes" })?;
    if opts.rate.is_finite() {
        mpv.set_property("speed", &format!("{:.2}", opts.rate.clamp(0.25, 4.0)))?;
    }
    if opts.volume.is_finite() {
        mpv.set_property("volume", &format!("{:.0}", opts.volume.clamp(0.0, 1.0) * 100.0))?;
    }
    mpv.set_property("mute", if opts.muted { "yes" } else { "no" })?;
    mpv.set_property("panscan", if opts.scale_mode == 2 { "1.0" } else { "0" })?;
    mpv.set_property("aid", "auto")?;
    mpv.set_property("sid", "no")?;
    mpv.set_property("force-media-title", &opts.title)?;
    mpv.command(&["loadfile", &opts.url])
}

fn tracks(mpv: &Mpv) -> (Vec<Track>, Vec<Track>) {
    let count = mpv.property_i64("track-list/count").unwrap_or(0);
    let (mut audio, mut subs) = (Vec::new(), Vec::new());
    for n in 0..count {
        let get = |k: &str| mpv.property(&format!("track-list/{n}/{k}")).filter(|v| !v.is_empty());
        let kind = match get("type").as_deref() {
            Some("audio") => TrackKind::Audio,
            Some("sub") => TrackKind::Subtitle,
            _ => continue,
        };
        let list = if kind == TrackKind::Audio {
            &mut audio
        } else {
            &mut subs
        };
        list.push(Track {
            kind,
            index: list.len(),
            id: get("id").and_then(|v| v.parse().ok()).unwrap_or(0),
            language: get("lang"),
            title: get("title"),
            codec: get("codec"),
        });
    }
    (audio, subs)
}

/// The audio and subtitle tracks mpv plays now, as indexes among tracks of their kind.
fn current_tracks(mpv: &Mpv, s: &Shared) -> (Option<usize>, Option<usize>) {
    let pos = |name: &str, ids: &[i64]| {
        let id = mpv.property(name)?.parse::<i64>().ok()?;
        ids.iter().position(|x| *x == id)
    };
    (pos("aid", &s.audio_ids), pos("sid", &s.subtitle_ids))
}

fn event_loop(mpv: Arc<Mpv>, shared: Arc<Mutex<Shared>>, closing: Arc<AtomicBool>, on_event: impl Fn(PlayerEvent)) {
    let mut last_sent_sec = -1.0_f64;
    loop {
        let ev = mpv.wait_event(0.5);
        let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
        match ev {
            Event::Timeout | Event::StartFile | Event::Other(_) => {}
            Event::PlaybackRestart => {
                let loading = s.loading;
                drop(s);
                if !loading {
                    on_event(PlayerEvent::Seeked);
                }
            }
            Event::Shutdown => {
                let position = s.position;
                drop(s);
                on_event(PlayerEvent::Closed { position });
                return;
            }
            Event::FileLoaded => {
                s.loading = false;
                s.prompted = false;
                let (audio, subs) = tracks(&mpv);
                s.audio_ids = audio.iter().map(|t| t.id).collect();
                s.subtitle_ids = subs.iter().map(|t| t.id).collect();
                let duration = mpv.property_f64("duration");
                s.duration = duration;
                // Remembered tracks of the viewer.
                if let Some((a, sub)) = s.wanted.take() {
                    if let Some(t) = a.and_then(|i| audio.get(i)) {
                        let _ = mpv.set_property("aid", &t.id.to_string());
                    }
                    if let Some(t) = sub.and_then(|i| subs.get(i)) {
                        let _ = mpv.set_property("sid", &t.id.to_string());
                    }
                }
                s.reported = current_tracks(&mpv, &s);
                drop(s);
                on_event(PlayerEvent::Loaded {
                    duration,
                    audio,
                    subtitles: subs,
                });
            }
            Event::EndFile(reason) => match reason {
                EndReason::Eof => {
                    let duration = s.duration;
                    if let Some(d) = duration {
                        s.position = d;
                    }
                    drop(s);
                    on_event(PlayerEvent::Ended);
                }
                EndReason::Error(msg) => {
                    s.loading = false;
                    drop(s);
                    on_event(PlayerEvent::Error(msg));
                    if closing.load(Ordering::SeqCst) {
                        return;
                    }
                }
                // A new file replaced this one, or the window is closing (Shutdown follows).
                EndReason::Stopped => {}
            },
            Event::ClientMessage(args) => {
                drop(s);
                if args.first().map(String::as_str) == Some("kino-next") {
                    on_event(PlayerEvent::NextRequested);
                }
            }
            Event::Property { name, value } => match (name.as_str(), value) {
                ("time-pos", Value::Double(pos)) if !s.loading => {
                    s.position = pos;
                    let duration = s.duration;
                    let mut offer = None;
                    if let (Some(next), Some(d)) = (s.next.clone(), duration) {
                        if !s.prompted && d > NEXT_PROMPT_SEC * 2.0 && pos >= d - NEXT_PROMPT_SEC {
                            s.prompted = true;
                            offer = Some(format!("Следующая серия {next}: нажмите N"));
                        }
                    }
                    drop(s);
                    if let Some(text) = offer {
                        let _ = mpv.command(&["show-text", &text, "6000"]);
                    }
                    // About once a second of video.
                    if (pos.floor() - last_sent_sec).abs() >= 1.0 {
                        last_sent_sec = pos.floor();
                        on_event(PlayerEvent::Position {
                            position: pos,
                            duration,
                        });
                    }
                }
                ("duration", Value::Double(d)) => s.duration = Some(d),
                ("pause", Value::Flag(p)) => {
                    drop(s);
                    on_event(PlayerEvent::Paused(p));
                }
                ("paused-for-cache", Value::Flag(b)) if !s.loading => {
                    drop(s);
                    on_event(PlayerEvent::Buffering(b));
                }
                ("aid" | "sid", _) if !s.loading => {
                    let now = current_tracks(&mpv, &s);
                    if now != s.reported {
                        s.reported = now;
                        drop(s);
                        on_event(PlayerEvent::Tracks {
                            audio: now.0,
                            subtitle: now.1,
                        });
                    }
                }
                _ => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::base_options;

    #[test]
    fn embedded_wid_disables_osc_and_sets_wid() {
        let opts = base_options(false, Some(123456));
        assert!(opts.iter().any(|(k, v)| k == "wid" && v == "123456"));
        assert!(opts.iter().any(|(k, v)| k == "osc" && v == "no"));
        assert!(opts.iter().any(|(k, v)| k == "input-default-bindings" && v == "no"));
        assert!(opts.iter().any(|(k, v)| k == "window-dragging" && v == "no"));
        assert!(!opts.iter().any(|(k, _)| k == "autofit"));
    }
}
