use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use leptos::html::Form;
use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use leptos_struct_table::ColumnSort;
use title_select_api::{
    AuditSummary, Dat, DatRevisionRecord, ImportOutcome, ImportedDatRevision, SourceSetRecord,
};

use crate::api;
use crate::app::{Loading, Problem};
use crate::events::use_live;
use crate::model::{short_sha, when};
use crate::screens::audit::focus;
use crate::table::{DataTable, Grid, Rows, rows_of, sort_of};

/// An Audit that refers to a DAT Revision.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UsedBy {
    audit: i64,
    source_set: String,
    pruned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RevisionItem {
    revision: DatRevisionRecord,
    used_by: Vec<UsedBy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum RevisionCol {
    Dat,
    Version,
    Imported,
    File,
    Sha,
    Blob,
    Audits,
}

fn audit_links(used_by: &[UsedBy]) -> AnyView {
    used_by
        .iter()
        .map(|u| {
            let title = format!("{}{}", u.source_set, if u.pruned { ", pruned" } else { "" });
            let label = format!("#{}", u.audit);
            view! {
                <A href=format!("/audits/{}", u.audit) attr:title=title>
                    {label}
                </A>
                " "
            }
        })
        .collect_view()
        .into_any()
}

impl Grid for RevisionItem {
    type Col = RevisionCol;
    const COLUMNS: &'static [RevisionCol] = &[
        RevisionCol::Dat,
        RevisionCol::Version,
        RevisionCol::Imported,
        RevisionCol::File,
        RevisionCol::Sha,
        RevisionCol::Blob,
        RevisionCol::Audits,
    ];

    fn header(col: RevisionCol) -> &'static str {
        match col {
            RevisionCol::Dat => "DAT",
            RevisionCol::Version => "Version",
            RevisionCol::Imported => "Imported",
            RevisionCol::File => "File",
            RevisionCol::Sha => "SHA-256",
            RevisionCol::Blob => "File kept",
            RevisionCol::Audits => "Audits",
        }
    }

    fn class(col: RevisionCol) -> &'static str {
        match col {
            RevisionCol::Dat | RevisionCol::File | RevisionCol::Audits => "c-wide",
            RevisionCol::Version | RevisionCol::Imported => "c-time data",
            RevisionCol::Sha => "c-hash data",
            RevisionCol::Blob => "c-state",
        }
    }

    fn sortable(col: RevisionCol) -> bool {
        matches!(
            col,
            RevisionCol::Dat | RevisionCol::Version | RevisionCol::Imported
        )
    }

    fn cell(&self, col: RevisionCol) -> AnyView {
        let r = &self.revision;
        match col {
            RevisionCol::Dat => r.dat().into_any(),
            RevisionCol::Version => r.version.clone().into_any(),
            RevisionCol::Imported => when(&r.imported_at).into_any(),
            RevisionCol::File => r.file.clone().into_any(),
            RevisionCol::Sha => short_sha(&r.sha256).to_string().into_any(),
            RevisionCol::Blob => if r.blob_present { "Kept" } else { "Deleted" }.into_any(),
            RevisionCol::Audits => match self.used_by.len() {
                0 => "None".into_any(),
                _ => audit_links(&self.used_by),
            },
        }
    }
}

/// Every DAT Revision, with the Audits of every Source Set's history that refer to it.
async fn revisions() -> Result<Vec<RevisionItem>, String> {
    let dats: Vec<Dat> = api::get("/dats").await?;
    let sets: Vec<SourceSetRecord> = api::get("/source-sets").await?;
    let mut used_by: BTreeMap<String, Vec<UsedBy>> = BTreeMap::new();
    for set in sets {
        let history: Vec<AuditSummary> =
            api::get(&format!("/source-sets/{}/history", api::segment(&set.name))).await?;
        for audit in history {
            used_by
                .entry(audit.dat_revision_sha256)
                .or_default()
                .push(UsedBy {
                    audit: audit.id,
                    source_set: audit.source_set,
                    pruned: audit.pruned,
                });
        }
    }
    Ok(dats
        .into_iter()
        .flat_map(|dat| dat.revisions.into_iter().rev())
        .map(|revision| RevisionItem {
            used_by: used_by.remove(&revision.sha256).unwrap_or_default(),
            revision,
        })
        .collect())
}

/// The DATs and their DAT Revisions, with their provenance, and the form to upload another.
#[component]
pub fn DatRevisions() -> impl IntoView {
    let live = use_live();
    let uploaded = RwSignal::new(0u32);
    let items = LocalResource::new(move || {
        live.settled.track();
        uploaded.track();
        revisions()
    });
    let sorting = RwSignal::new(VecDeque::<(RevisionCol, ColumnSort)>::new());
    let selected = RwSignal::new(None::<usize>);
    let shown = Memo::new(move |_| {
        let mut items = items.get().and_then(Result::ok).unwrap_or_default();
        if let Some(sort) = sorting.with(sort_of) {
            items.sort_by(|a, b| {
                let (a, b) = (&a.revision, &b.revision);
                let ord = match sort.column {
                    RevisionCol::Version => a.version.cmp(&b.version),
                    RevisionCol::Imported => a.imported_at.cmp(&b.imported_at),
                    _ => a.dat().cmp(&b.dat()),
                };
                if sort.descending { ord.reverse() } else { ord }
            });
        }
        Arc::new(items)
    });
    let rows = Signal::derive(move || rows_of(shown.get().as_ref().clone()) as Rows<RevisionItem>);
    let detail = move || {
        let item = shown.with(|s| s.get(selected.get()?).cloned())?;
        Some(revision_detail(item))
    };

    view! {
        <h1>"DAT revisions"</h1>
        <p class="lede">
            "Each import of a DAT file is a DAT revision, named by its SHA-256. Newest import first within each DAT."
        </p>
        {move || match items.get() {
            None => view! { <Loading what="the DAT revisions" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <DataTable
            id="dat-revisions"
            label="DAT revisions".to_string()
            rows
            sorting
            selected
            on_open=Callback::new(|_| focus("revision-detail"))
            empty="No DAT revisions yet. Upload a DAT below.".to_string()
        />
        <section class="detail" id="revision-detail" tabindex="-1" aria-label="Selected DAT revision">
            {move || match detail() {
                Some(view) => view,
                None => view! { <p class="note">"Select a DAT revision to see its provenance."</p> }.into_any(),
            }}
        </section>
        <Upload on_done=Callback::new(move |_| uploaded.update(|n| *n += 1)) />
    }
}

fn revision_detail(item: RevisionItem) -> AnyView {
    let r = item.revision;
    view! {
        <h3>{format!("{} {}", r.dat(), r.version)}</h3>
        <dl class="facts">
            <dt>"SHA-256"</dt>
            <dd>
                <code class="hash">{r.sha256.clone()}</code>
            </dd>
            <dt>"File"</dt>
            <dd>
                <code class="path">{r.file.clone()}</code>
            </dd>
            <dt>"Provider"</dt>
            <dd>{r.provider.clone()}</dd>
            <dt>"Source URL"</dt>
            <dd>
                {match r.source_url.clone() {
                    Some(url) => view! { <code class="path">{url}</code> }.into_any(),
                    None => "Not recorded".into_any(),
                }}
            </dd>
            <dt>"Imported"</dt>
            <dd>{when(&r.imported_at)}</dd>
            <dt>"File kept"</dt>
            <dd>
                {if r.blob_present {
                    "Kept in the store"
                } else {
                    "Deleted from the store; the DAT revision is still recorded, and uploading the same file again restores it"
                }}
            </dd>
            <dt>"Audits"</dt>
            <dd>
                {if item.used_by.is_empty() {
                    view! { "No audit refers to it" }.into_any()
                } else {
                    view! {
                        <ul>
                            {item
                                .used_by
                                .into_iter()
                                .map(|u| {
                                    view! {
                                        <li>
                                            <A href=format!("/audits/{}", u.audit)>{format!("Audit #{}", u.audit)}</A>
                                            {format!(
                                                " of {}{}",
                                                u.source_set,
                                                if u.pruned { ", pruned" } else { "" },
                                            )}
                                        </li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                        .into_any()
                }}
            </dd>
        </dl>
    }
    .into_any()
}

#[component]
fn Upload(on_done: Callback<()>) -> impl IntoView {
    let form = NodeRef::<Form>::new();
    let outcome = RwSignal::new(None::<Result<ImportedDatRevision, String>>);
    let busy = RwSignal::new(false);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let Some(el) = form.get_untracked() else {
            return;
        };
        let Ok(data) = web_sys::FormData::new_with_form(&el) else {
            return;
        };
        busy.set(true);
        leptos::task::spawn_local(async move {
            let result = api::post_form::<ImportedDatRevision>("/dat-revisions", data).await;
            busy.set(false);
            if result.is_ok() {
                el.reset();
                on_done.run(());
            }
            outcome.set(Some(result));
        });
    };
    view! {
        <section aria-labelledby="upload-title">
            <h2 id="upload-title">"Upload a DAT"</h2>
            <form class="form" node_ref=form on:submit=submit>
                <label>
                    "Provider"
                    <input type="text" name="provider" required placeholder="No-Intro" />
                </label>
                <label>
                    "Source URL"
                    <input type="url" name="source_url" placeholder="Where the DAT came from" />
                </label>
                <label>
                    "DAT file"
                    <input type="file" name="file" required accept=".dat,.xml" />
                </label>
                <button type="submit" class="primary" disabled=busy>
                    {move || if busy.get() { "Uploading…" } else { "Upload" }}
                </button>
            </form>
            {move || {
                outcome
                    .get()
                    .map(|outcome| match outcome {
                        Ok(imported) => {
                            let r = imported.dat_revision;
                            let what = match imported.outcome {
                                ImportOutcome::Added => "Added DAT revision",
                                ImportOutcome::AlreadyImported => "Already imported: DAT revision",
                                ImportOutcome::Restored => "Restored the kept file of DAT revision",
                            };
                            view! {
                                <p class="done" role="status">
                                    {format!("{what} {} {} ({}).", r.dat(), r.version, short_sha(&r.sha256))}
                                </p>
                            }
                                .into_any()
                        }
                        Err(message) => view! { <Problem message /> }.into_any(),
                    })
            }}
        </section>
    }
}
