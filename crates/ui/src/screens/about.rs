use gloo_net::http::Request;
use leptos::prelude::*;

use crate::app::Loading;
use crate::model::is_plain_text;

const RETOOL: &str = "https://github.com/unexpectedpanda/retool";
const CLONE_LISTS: &str = "https://github.com/unexpectedpanda/retool-clonelists-metadata";

/// A licence text shipped beside the workbench's bundle, under `/licences/`.
struct Licence {
    title: &'static str,
    file: &'static str,
    /// Where to read it when this build does not ship it; Title Select's own is in its source.
    upstream: Option<&'static str>,
}

const LICENCES: &[Licence] = &[
    Licence {
        title: "Title Select: MIT License",
        file: "/licences/title-select.txt",
        upstream: None,
    },
    Licence {
        title: "Retool: BSD 3-Clause License",
        file: "/licences/retool.txt",
        upstream: Some("https://github.com/unexpectedpanda/retool/blob/main/LICENSE"),
    },
    Licence {
        title: "Retool clone lists and metadata: BSD 3-Clause License",
        file: "/licences/retool-clonelists-metadata.txt",
        upstream: Some(
            "https://github.com/unexpectedpanda/retool-clonelists-metadata/blob/main/LICENSE",
        ),
    },
];

/// The text at `file`, or `None` when the bundle does not have it: the Service then answers with the workbench's
/// page instead.
async fn fetch(file: &'static str) -> Option<String> {
    let response = Request::get(file).send().await.ok()?;
    let plain = is_plain_text(response.headers().get("content-type").as_deref());
    if !response.ok() || !plain {
        return None;
    }
    response.text().await.ok()
}

/// Where selection comes from, and the licences of Title Select and of the projects it is derived from.
#[component]
pub fn About() -> impl IntoView {
    view! {
        <h1>"About Title Select"</h1>
        <p class="lede">
            "Title Select audits your source sets against DAT files and records exactly what was verified, and against which DAT revision."
        </p>
        <section aria-labelledby="selection-title">
            <h2 id="selection-title">"Where selection comes from"</h2>
            <p>
                "Selection is derived from " <a href=RETOOL>"Retool"</a>
                ", with the community-maintained " <a href=CLONE_LISTS>"Retool clone lists and metadata"</a>
                ". Title Select runs a pinned, patched copy of Retool offline, with a pinned copy of the clone lists and metadata, and explains each choice from Retool's own trace. Both projects are published under the BSD 3-Clause License, shown below."
            </p>
            <p class="note">
                "To see the exact upstream commits this build is pinned to, run " <code>"title-select --version"</code>
                "."
            </p>
        </section>
        <section aria-labelledby="licences-title">
            <h2 id="licences-title">"Licences"</h2>
            {LICENCES.iter().map(|licence| view! { <LicenceText licence /> }).collect_view()}
            <p class="note">
                "The workbench's fonts, Liberation Sans and Liberation Mono, are served under the "
                <a href="/fonts/LICENSE-OFL.txt">"SIL Open Font License"</a> "."
            </p>
        </section>
    }
}

#[component]
fn LicenceText(licence: &'static Licence) -> impl IntoView {
    let text = LocalResource::new(move || fetch(licence.file));
    view! {
        <section class="licence" aria-label=licence.title>
            <h3>{licence.title}</h3>
            {move || match text.get() {
                None => view! { <Loading what="the licence" /> }.into_any(),
                Some(Some(text)) => view! { <pre class="licence-text">{text}</pre> }.into_any(),
                Some(None) => {
                    view! {
                        <p class="problem" role="alert">
                            "This build does not include this licence text. "
                            {match licence.upstream {
                                Some(url) => view! { "Read it at " <a href=url>{url}</a> "." }.into_any(),
                                None => "It is the LICENSE file at the root of Title Select's source.".into_any(),
                            }}
                        </p>
                    }
                        .into_any()
                }
            }}
        </section>
    }
}
