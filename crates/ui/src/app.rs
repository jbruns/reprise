use leptos::prelude::*;
use leptos_router::components::{A, Redirect, Route, Router, Routes};
use leptos_router::hooks::use_navigate;
use leptos_router::path;

use crate::events::{Stream, provide_live, use_live};
use crate::screens::{AuditScreen, Bench, DatRevisions, Jobs, SourceSet, SourceSets};

/// The workbench: a bar of screens over the routes.
#[component]
pub fn App() -> impl IntoView {
    provide_live();
    view! {
        <Router>
            <a class="skip" href="#main">"Skip to content"</a>
            <header class="bar">
                <a class="brand" href="/">
                    <img src="/icon-64.png" alt="" width="24" height="24" />
                    "Title Select"
                </a>
                <nav aria-label="Workbench">
                    <A href="/source-sets">"Source Sets"</A>
                    <A href="/dat-revisions">"DAT Revisions"</A>
                    <A href="/jobs">"Jobs"</A>
                </nav>
                <StreamStatus />
            </header>
            <main id="main" tabindex="-1">
                <Routes fallback=|| view! { <NotFound /> }>
                    <Route path=path!("/") view=|| view! { <Redirect path="/source-sets" /> } />
                    <Route path=path!("/source-sets") view=SourceSets />
                    <Route path=path!("/source-sets/:name") view=SourceSet />
                    <Route path=path!("/dat-revisions") view=DatRevisions />
                    <Route path=path!("/audits/:id") view=AuditScreen />
                    <Route path=path!("/jobs") view=Jobs />
                    <Route path=path!("/bench") view=Bench />
                </Routes>
            </main>
        </Router>
    }
}

#[component]
fn StreamStatus() -> impl IntoView {
    let live = use_live();
    let active = move || live.book.with(|b| b.active());
    view! {
        <p class="stream" aria-live="polite">
            <span class=move || format!("dot {}", live.stream.get().label().to_lowercase())></span>
            {move || match live.stream.get() {
                Stream::Live => format!("Live · {} Jobs active", active()),
                other => other.label().to_string(),
            }}
        </p>
    }
}

#[component]
fn NotFound() -> impl IntoView {
    view! {
        <h1>"Not found"</h1>
        <p>"There is no workbench screen here. Choose one above."</p>
    }
}

/// Navigates within the workbench from an event handler.
pub fn navigator() -> Callback<String> {
    let target = RwSignal::new(None::<String>);
    let navigate = use_navigate();
    Effect::new(move || {
        if let Some(path) = target.get() {
            navigate(&path, Default::default());
        }
    });
    Callback::new(move |path: String| target.set(Some(path)))
}

/// An error from the Service, announced as it appears.
#[component]
pub fn Problem(message: String) -> impl IntoView {
    view! { <p class="problem" role="alert">{message}</p> }
}

#[component]
pub fn Loading(what: &'static str) -> impl IntoView {
    view! { <p class="loading-note" aria-live="polite">{format!("Loading {what}…")}</p> }
}
