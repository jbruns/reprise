//! The `/events` stream: every Job's `state` and `progress`, and a `resync` on connecting or after falling behind.

use std::cell::RefCell;

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{EventSource, MessageEvent};

use crate::api;
use crate::jobs::{Applied, JobBook, parse_event};

/// How long to wait before opening a stream the browser gave up on.
const RECONNECT_MS: i32 = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    Connecting,
    Live,
    Reconnecting,
}

impl Stream {
    pub fn label(self) -> &'static str {
        match self {
            Stream::Connecting => "Connecting",
            Stream::Live => "Live",
            Stream::Reconnecting => "Reconnecting",
        }
    }
}

/// The Jobs as the stream reports them, shared by every screen.
#[derive(Clone, Copy)]
pub struct Live {
    pub book: RwSignal<JobBook>,
    pub stream: RwSignal<Stream>,
    /// Goes up when a Job reaches a final state or the stream resyncs, so screens showing Audits fetch again.
    pub settled: RwSignal<u64>,
}

pub fn use_live() -> Live {
    expect_context::<Live>()
}

/// Opens the stream and provides [`Live`] to the app.
pub fn provide_live() -> Live {
    let live = Live {
        book: RwSignal::new(JobBook::default()),
        stream: RwSignal::new(Stream::Connecting),
        settled: RwSignal::new(0),
    };
    provide_context(live);
    connect(live);
    live
}

struct Connection {
    source: EventSource,
    _listeners: Vec<Closure<dyn FnMut(web_sys::Event)>>,
}

thread_local! {
    static CONNECTION: RefCell<Option<Connection>> = const { RefCell::new(None) };
}

fn apply(live: Live, name: &str, data: &str) {
    let event = match parse_event(name, data) {
        Ok(event) => event,
        Err(err) => {
            leptos::logging::warn!("ignoring a server-sent event: {err}");
            return;
        }
    };
    let applied = live.book.try_update_untracked(|book| book.apply(event));
    match applied {
        None | Some(Applied::Ignored) => {}
        Some(Applied::Changed) => live.book.notify(),
        Some(Applied::Finished | Applied::Resynced) => {
            live.book.notify();
            live.settled.update(|n| *n += 1);
        }
    }
}

fn connect(live: Live) {
    let Ok(source) = EventSource::new(&api::url("/events")) else {
        reconnect_later(live);
        return;
    };
    let mut listeners = Vec::new();
    for name in ["state", "progress", "resync"] {
        let listener = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
            if let Some(data) = event.unchecked_into::<MessageEvent>().data().as_string() {
                apply(live, name, &data);
            }
        });
        let _ = source.add_event_listener_with_callback(name, listener.as_ref().unchecked_ref());
        listeners.push(listener);
    }
    let on_open = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| live.stream.set(Stream::Live));
    source.set_onopen(Some(on_open.as_ref().unchecked_ref()));
    listeners.push(on_open);
    let on_error = Closure::<dyn FnMut(web_sys::Event)>::new({
        let source = source.clone();
        move |_| {
            live.stream.set(Stream::Reconnecting);
            // The browser retries a dropped stream itself unless the response was an error; then it is closed.
            if source.ready_state() == EventSource::CLOSED {
                reconnect_later(live);
            }
        }
    });
    source.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    listeners.push(on_error);
    CONNECTION.with(|c| {
        *c.borrow_mut() = Some(Connection {
            source,
            _listeners: listeners,
        })
    });
}

fn reconnect_later(live: Live) {
    let retry = Closure::once_into_js(move || {
        if let Some(old) = CONNECTION.with(|c| c.borrow_mut().take()) {
            old.source.close();
        }
        connect(live);
    });
    if let Some(window) = web_sys::window() {
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            retry.unchecked_ref(),
            RECONNECT_MS,
        );
    }
}
