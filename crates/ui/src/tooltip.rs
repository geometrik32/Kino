//! App-styled floating tooltips (never clipped by overflow containers) and hover-scrolling titles.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use dioxus::html::MountedData;
use dioxus::prelude::*;

static NEXT_TIP_OWNER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, PartialEq)]
struct ActiveTooltip {
    owner: u64,
    slot: u32,
    text: String,
    left: f64,
    top: f64,
    bottom: f64,
    center_x: f64,
    right: f64,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct TooltipHost {
    active: Signal<Option<ActiveTooltip>>,
    root_el: Signal<Option<Rc<MountedData>>>,
    viewport_w: Signal<f64>,
}

impl TooltipHost {
    pub(crate) fn mount_root(&self, el: Rc<MountedData>) {
        let mut root_el = self.root_el;
        root_el.set(Some(el.clone()));
        let mut viewport_w = self.viewport_w;
        spawn(async move {
            if let Ok(r) = el.get_client_rect().await {
                viewport_w.set(r.width());
            }
        });
    }

    pub(crate) fn clear(&self) {
        let mut active = self.active;
        if active.peek().is_some() {
            active.set(None);
        }
    }

    fn clear_owner(&self, owner: u64) {
        let mut active = self.active;
        if active.peek().as_ref().is_some_and(|t| t.owner == owner) {
            active.set(None);
        }
    }
}

/// Provides a root-level tooltip host for `Shell` or `PlayerView`.
pub(crate) fn provide_tooltip_host() -> TooltipHost {
    let host = TooltipHost {
        active: use_signal(|| None::<ActiveTooltip>),
        root_el: use_signal(|| None::<Rc<MountedData>>),
        viewport_w: use_signal(|| 0.0f64),
    };
    use_context_provider(|| host);
    host
}

pub(crate) fn clear_active_tooltip() {
    if let Some(host) = try_consume_context::<TooltipHost>() {
        host.clear();
    }
}

#[derive(Clone, Copy)]
pub(crate) struct TipHandle {
    owner: u64,
    host: Option<TooltipHost>,
    els: Signal<HashMap<u32, Rc<MountedData>>>,
    hover: Signal<Option<(u32, u64)>>,
    seq: Signal<u64>,
}

impl TipHandle {
    pub(crate) fn mount(&self, slot: u32, el: Rc<MountedData>) {
        let mut els = self.els;
        els.write().insert(slot, el);
    }

    pub(crate) fn show(&self, slot: u32, text: impl Into<String>) {
        let text = text.into();
        let Some(host) = self.host else { return };
        if text.trim().is_empty() {
            return;
        }
        let mut seq_sig = self.seq;
        let next_seq = *seq_sig.peek() + 1;
        seq_sig.set(next_seq);
        let mut hover_sig = self.hover;
        hover_sig.set(Some((slot, next_seq)));
        let Some(el) = self.els.peek().get(&slot).cloned() else {
            return;
        };
        let owner = self.owner;
        spawn(async move {
            let Ok(rect) = el.get_client_rect().await else {
                return;
            };
            if *hover_sig.peek() != Some((slot, next_seq)) {
                return;
            }
            if let Some(root) = host.root_el.peek().clone() {
                if let Ok(rr) = root.get_client_rect().await {
                    let mut vw = host.viewport_w;
                    if (rr.width() - *vw.peek()).abs() > 1.0 {
                        vw.set(rr.width());
                    }
                }
            }
            if *hover_sig.peek() != Some((slot, next_seq)) {
                return;
            }
            let left = rect.origin.x;
            let top = rect.origin.y;
            let right = left + rect.width();
            let bottom = top + rect.height();
            let center_x = left + rect.width() / 2.0;
            let mut active = host.active;
            active.set(Some(ActiveTooltip {
                owner,
                slot,
                text,
                left,
                top,
                bottom,
                center_x,
                right,
            }));
        });
    }

    pub(crate) fn hide(&self, slot: u32) {
        let mut hover_sig = self.hover;
        if hover_sig.peek().is_some_and(|(s, _)| s == slot) {
            hover_sig.set(None);
        }
        if let Some(host) = self.host {
            let mut active = host.active;
            if active
                .peek()
                .as_ref()
                .is_some_and(|t| t.owner == self.owner && t.slot == slot)
            {
                active.set(None);
            }
        }
    }

    pub(crate) fn hide_all(&self) {
        let mut hover_sig = self.hover;
        hover_sig.set(None);
        if let Some(host) = self.host {
            host.clear_owner(self.owner);
        }
    }
}

/// Allocates a component-scoped tooltip handle that automatically dismisses on unmount.
pub(crate) fn use_tip() -> TipHandle {
    let owner = use_hook(|| NEXT_TIP_OWNER.fetch_add(1, Ordering::Relaxed));
    let host = try_consume_context::<TooltipHost>();
    let els = use_signal(HashMap::new);
    let hover = use_signal(|| None::<(u32, u64)>);
    let seq = use_signal(|| 0u64);
    use_drop(move || {
        if let Some(h) = host {
            h.clear_owner(owner);
        }
    });
    TipHandle {
        owner,
        host,
        els,
        hover,
        seq,
    }
}

/// Renders the active app-styled floating tooltip at the root level.
#[component]
pub(crate) fn TooltipLayer(host: TooltipHost) -> Element {
    let Some(tip) = (host.active)() else {
        return rsx! {};
    };
    let vw = (host.viewport_w)();
    let place_below = tip.top < 86.0;
    let (h_style, align_cls) = if tip.center_x < 164.0 {
        (format!("left: {:.1}px;", tip.left.max(12.0)), "align-left")
    } else if vw > 0.0 && tip.center_x > vw - 164.0 {
        (format!("right: {:.1}px;", (vw - tip.right).max(12.0)), "align-right")
    } else {
        (format!("left: {:.1}px;", tip.center_x), "align-center")
    };
    let v_style = if place_below {
        format!("top: {:.1}px;", tip.bottom + 8.0)
    } else {
        format!("top: {:.1}px;", (tip.top - 8.0).max(12.0))
    };
    let pos_cls = if place_below { "below" } else { "above" };
    let lines: Vec<&str> = tip.text.lines().filter(|l| !l.is_empty()).collect();
    rsx! {
        div {
            class: "kino-tooltip {pos_cls} {align_cls}",
            style: "{h_style} {v_style}",
            role: "tooltip",
            for (idx, line) in lines.into_iter().enumerate() {
                div { key: "tl-{idx}", class: "kino-tooltip-line", "{line}" }
            }
        }
    }
}

/// Single-line title that shows ellipsis when clipped and smoothly scrolls the full text on hover.
#[component]
pub(crate) fn ScrollTitle(title: String, #[props(default = "name".to_owned())] class: String) -> Element {
    let mut el = use_signal(|| None::<Rc<MountedData>>);
    let mut hovered = use_signal(|| false);
    let mut scroll_px = use_signal(|| 0.0f64);
    let mut dur_s = use_signal(|| 0.0f64);

    let active = hovered() && scroll_px() > 0.0;
    let cls = if active { format!("{class} scrolling") } else { class };
    let track_style = if active {
        format!(
            "--scroll-x: -{:.1}px; animation-duration: {:.2}s;",
            scroll_px(),
            dur_s()
        )
    } else {
        String::new()
    };

    rsx! {
        div {
            class: "{cls}",
            onmounted: move |e| el.set(Some(e.data())),
            onmouseenter: move |_| {
                hovered.set(true);
                let Some(node) = el() else { return };
                spawn(async move {
                    if let (Ok(sz), Ok(rect)) = (node.get_scroll_size().await, node.get_client_rect().await) {
                        if !*hovered.peek() {
                            return;
                        }
                        let overflow = sz.width - rect.width();
                        if overflow > 2.0 {
                            let dist = (overflow + 8.0).ceil();
                            let dur = (dist / 38.0 * 2.0 + 1.4).clamp(2.6, 9.0);
                            scroll_px.set(dist);
                            dur_s.set(dur);
                        } else {
                            scroll_px.set(0.0);
                        }
                    }
                });
            },
            onmouseleave: move |_| {
                hovered.set(false);
                scroll_px.set(0.0);
            },
            span { class: "scroll-title-track", style: "{track_style}", "{title}" }
        }
    }
}
