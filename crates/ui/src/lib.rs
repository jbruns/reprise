//! Title Select's workbench: DAT Revisions, Source Sets, Audits and Jobs over the `/api/v1` JSON API (ADR 0005).
//!
//! `model` and `jobs` are pure and tested on the host; the rest renders in the browser.

pub mod api;
pub mod app;
pub mod events;
pub mod jobs;
pub mod model;
pub mod screens;
pub mod table;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn start() {
    leptos::mount::mount_to_body(app::App);
}
