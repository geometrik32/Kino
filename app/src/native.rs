//! Telegram's own controls as Dioxus components: mount one to take the control,
//! unmount it to give the control back. Outside Telegram they do nothing.

use std::{cell::Cell, rc::Rc};

use dioxus::prelude::*;

use crate::telegram;

/// Gives the handler back when the owner unmounts.
struct BackGuard(u64);

impl Drop for BackGuard {
    fn drop(&mut self) {
        telegram::pop_back(self.0);
    }
}

/// The native Back button (and the Android back gesture) is shown while this is mounted
/// and calls `on_back`. The newest mounted one wins.
#[component]
pub fn BackButton(on_back: EventHandler<()>) -> Element {
    let latest = use_hook(|| Rc::new(Cell::new(on_back)));
    latest.set(on_back);
    use_hook(|| {
        let latest = latest.clone();
        Rc::new(BackGuard(telegram::push_back(move || latest.get().call(()))))
    });
    rsx! {}
}

struct MainGuard;

impl Drop for MainGuard {
    fn drop(&mut self) {
        telegram::hide_main_button();
    }
}

/// The big native button at the bottom of the screen, labelled `text`, while mounted.
#[component]
pub fn MainButton(text: String, on_click: EventHandler<()>) -> Element {
    let latest = use_hook(|| Rc::new(Cell::new(on_click)));
    latest.set(on_click);
    use_hook(|| Rc::new(MainGuard));
    let shown = use_hook(|| Rc::new(std::cell::RefCell::new(String::new())));
    if *shown.borrow() != text {
        *shown.borrow_mut() = text.clone();
        let latest = latest.clone();
        telegram::show_main_button(&text, move || latest.get().call(()));
    }
    rsx! {}
}
