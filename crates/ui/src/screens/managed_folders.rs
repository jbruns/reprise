use std::collections::VecDeque;
use std::sync::Arc;

use leptos::prelude::*;
use leptos::tachys::view::any_view::AnyView;
use leptos_router::components::A;
use leptos_router::hooks::use_params_map;
use leptos_struct_table::ColumnSort;
use title_select_api::build::{
    AddManagedFolder, ApprovePlan, BuildDetail, BuildItem, BuildSummary, BuiltFile,
    ChangeManagedFolder, ItemKind, ManagedFolder, Plan, PlanBuild, PlanItem, PlanSummary,
};
use title_select_api::{
    Dat, Job, PathTemplateDefaults, ProfileRecord, Root, SelectionRecord, SourceSetRecord,
};

use crate::api;
use crate::app::{Loading, Problem, navigator};
use crate::build_model::{
    blocker_label, build_state_label, by_kind, cause_label, folders_for, group_href,
    item_kind_label, item_kind_note, item_state_label,
};
use crate::events::use_live;
use crate::model::{short_sha, when};
use crate::registration;
use crate::screens::jobs::progress_view;
use crate::table::{DataTable, Grid, Rows, rows_of, sort_of};

fn folder_href(id: i64) -> String {
    format!("/managed-folders/{id}")
}

fn location(folder: &ManagedFolder) -> String {
    format!("{}: {}", folder.root, folder.path)
}

fn current_build(build: &Option<BuildSummary>) -> AnyView {
    match build {
        Some(b) => view! {
            <A href=format!("/builds/{}", b.id)>{format!("#{}", b.id)}</A>
            {format!(", generation {} · {}", b.generation, when(b.finished_at.as_deref().unwrap_or(&b.started_at)))}
        }
        .into_any(),
        None => "Never built".into_any(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FolderCol {
    Location,
    System,
    SourceSet,
    Profile,
    Build,
    Files,
}

#[derive(Clone, PartialEq)]
struct FolderItem(ManagedFolder);

impl Grid for FolderItem {
    type Col = FolderCol;
    const COLUMNS: &'static [FolderCol] = &[
        FolderCol::Location,
        FolderCol::System,
        FolderCol::SourceSet,
        FolderCol::Profile,
        FolderCol::Build,
        FolderCol::Files,
    ];

    fn header(col: FolderCol) -> &'static str {
        match col {
            FolderCol::Location => "Root and path",
            FolderCol::System => "System",
            FolderCol::SourceSet => "Source set",
            FolderCol::Profile => "Selection profile",
            FolderCol::Build => "Current build",
            FolderCol::Files => "Files",
        }
    }

    fn class(col: FolderCol) -> &'static str {
        match col {
            FolderCol::Location => "c-wide data",
            FolderCol::Files => "c-num",
            _ => "c-wide",
        }
    }

    fn sortable(col: FolderCol) -> bool {
        matches!(
            col,
            FolderCol::Location | FolderCol::System | FolderCol::SourceSet
        )
    }

    fn cell(&self, col: FolderCol) -> AnyView {
        let f = &self.0;
        match col {
            FolderCol::Location => {
                let (href, text) = (folder_href(f.id), location(f));
                view! { <A href=href>{text}</A> }.into_any()
            }
            FolderCol::System => f.system.clone().into_any(),
            FolderCol::SourceSet => f.source_set.clone().into_any(),
            FolderCol::Profile => f.profile.clone().into_any(),
            FolderCol::Build => current_build(&f.build),
            FolderCol::Files => f.files.to_string().into_any(),
        }
    }
}

/// The managed folders, and the form to register another.
#[component]
pub fn ManagedFolders() -> impl IntoView {
    let live = use_live();
    let folders = LocalResource::new(move || {
        live.settled.track();
        api::get::<Vec<ManagedFolder>>("/managed-folders")
    });
    let sorting = RwSignal::new(VecDeque::<(FolderCol, ColumnSort)>::new());
    let selected = RwSignal::new(None::<usize>);
    let shown = Memo::new(move |_| {
        let mut items: Vec<FolderItem> = folders
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .map(FolderItem)
            .collect();
        if let Some(sort) = sorting.with(sort_of) {
            items.sort_by(|a, b| {
                let ord = match sort.column {
                    FolderCol::System => a.0.system.cmp(&b.0.system),
                    FolderCol::SourceSet => a.0.source_set.cmp(&b.0.source_set),
                    _ => location(&a.0).cmp(&location(&b.0)),
                };
                if sort.descending { ord.reverse() } else { ord }
            });
        }
        Arc::new(items)
    });
    let rows = Signal::derive(move || rows_of(shown.get().as_ref().clone()) as Rows<FolderItem>);
    let go = navigator();

    view! {
        <h1>"Managed folders"</h1>
        <p class="lede">
            "A managed folder is a folder in a writable root that you build a source set's selections into, \
            for your front end to play from. Open one to plan a build, review it and approve it."
        </p>
        {move || match folders.get() {
            None => view! { <Loading what="the managed folders" /> }.into_any(),
            Some(Err(message)) => view! { <Problem message /> }.into_any(),
            Some(Ok(_)) => ().into_any(),
        }}
        <DataTable
            id="managed-folders"
            label="Managed folders".to_string()
            rows
            sorting
            selected
            on_open=Callback::new(move |item: FolderItem| go.run(folder_href(item.0.id)))
            empty="No managed folders yet. Register one in a writable root below.".to_string()
        />
        <RegisterFolder on_added=Callback::new(move |id: i64| go.run(folder_href(id))) />
    }
}

#[component]
fn RegisterFolder(on_added: Callback<i64>) -> impl IntoView {
    let roots = LocalResource::new(|| api::get::<Vec<Root>>("/roots"));
    let sets = LocalResource::new(|| api::get::<Vec<SourceSetRecord>>("/source-sets"));
    let dats = LocalResource::new(|| api::get::<Vec<Dat>>("/dats"));
    let profiles = LocalResource::new(|| api::get::<Vec<ProfileRecord>>("/profiles"));
    let defaults = LocalResource::new(|| api::get::<PathTemplateDefaults>("/path-templates"));
    let writable = move || {
        roots
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.writable)
            .collect::<Vec<_>>()
    };
    let saved = move || {
        profiles
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.latest.is_some())
            .collect::<Vec<_>>()
    };
    let root = RwSignal::new(String::new());
    let set = RwSignal::new(String::new());
    let profile = RwSignal::new(String::new());
    let template = RwSignal::new(None::<String>);
    let typed = RwSignal::new(None::<String>);
    let problem = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);

    let proposal = Memo::new(move |_| {
        let Some(template) = template.get() else {
            return Err("Loading the path template…".to_string());
        };
        let set = set.get();
        let sets = sets.get().and_then(Result::ok).unwrap_or_default();
        let Some(record) = sets.into_iter().find(|s| s.name == set) else {
            return Err("Choose a source set to propose a path.".to_string());
        };
        let dats = dats.get().and_then(Result::ok).unwrap_or_default();
        match dats.iter().find(|d| d.dat == record.dat) {
            Some(d) => registration::propose(&template, d),
            None => Err("Loading the source set's DAT…".to_string()),
        }
    });
    let path = Memo::new(move |_| {
        registration::chosen_path(typed.get().as_deref(), &proposal.get()).unwrap_or_default()
    });
    let preview = move || {
        let root_path = writable()
            .into_iter()
            .find(|r| r.name == root.get())
            .map(|r| r.path)
            .unwrap_or_default();
        match (typed.get(), &proposal.get()) {
            (None, Err(reason)) => Err(reason.clone()),
            _ => registration::preview(&root_path, &path.get()),
        }
    };

    Effect::new(move || {
        if root.get_untracked().is_empty()
            && let Some(first) = writable().into_iter().next()
        {
            root.set(first.name);
        }
    });
    Effect::new(move || {
        if let Some(Ok(sets)) = sets.get()
            && set.get_untracked().is_empty()
            && let Some(first) = sets.first()
        {
            set.set(first.name.clone());
        }
    });
    Effect::new(move || {
        if profile.get_untracked().is_empty()
            && let Some(first) = saved().into_iter().next()
        {
            profile.set(first.name);
        }
    });
    Effect::new(move || {
        if let Some(Ok(defaults)) = defaults.get()
            && template.get_untracked().is_none()
        {
            template.set(Some(defaults.managed_folder));
        }
    });

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let chosen = path.get_untracked();
        if chosen.trim().is_empty() {
            problem.set(Some(
                "Type a path, or change the template so it proposes one.".into(),
            ));
            return;
        }
        let request = AddManagedFolder {
            root: root.get_untracked(),
            path: Some(chosen),
            template: None,
            source_set: set.get_untracked(),
            profile: profile.get_untracked(),
        };
        busy.set(true);
        leptos::task::spawn_local(async move {
            let added = api::post::<_, ManagedFolder>("/managed-folders", &request).await;
            busy.set(false);
            match added {
                Ok(folder) => on_added.run(folder.id),
                Err(message) => problem.set(Some(message)),
            }
        });
    };

    view! {
        <section aria-labelledby="register-title">
            <h2 id="register-title">"Register a managed folder"</h2>
            {move || match roots.get() {
                Some(Ok(_)) if writable().is_empty() => view! {
                    <p class="note">
                        "None of your roots is writable. To build into your front end's folders, add a root \
                        for them with "<code>"writable = true"</code>" to your configuration."
                    </p>
                }
                .into_any(),
                _ => ().into_any(),
            }}
            <form class="form" on:submit=submit>
                <label>
                    "Writable root"
                    <select required prop:value=root on:change=move |ev| root.set(event_target_value(&ev))>
                        {move || {
                            writable()
                                .into_iter()
                                .map(|r| {
                                    let label = format!("{} ({})", r.name, r.path);
                                    view! { <option value=r.name>{label}</option> }
                                })
                                .collect_view()
                        }}
                    </select>
                </label>
                <label>
                    "Source set"
                    <select required prop:value=set on:change=move |ev| set.set(event_target_value(&ev))>
                        {move || {
                            sets.get()
                                .and_then(Result::ok)
                                .unwrap_or_default()
                                .into_iter()
                                .map(|s| {
                                    let label = format!("{} ({})", s.name, s.dat);
                                    view! { <option value=s.name>{label}</option> }
                                })
                                .collect_view()
                        }}
                    </select>
                </label>
                <label>
                    "Selection profile"
                    <select required prop:value=profile on:change=move |ev| profile.set(event_target_value(&ev))>
                        {move || {
                            saved()
                                .into_iter()
                                .map(|p| view! { <option value=p.name.clone()>{p.name.clone()}</option> })
                                .collect_view()
                        }}
                    </select>
                </label>
                <label>
                    "Path template"
                    <input
                        type="text"
                        class="data"
                        title="Tokens: {provider}, {dat} and {platform}"
                        prop:value=move || template.get().unwrap_or_default()
                        on:input=move |ev| template.set(Some(event_target_value(&ev)))
                    />
                </label>
                <label>
                    "Path in the root"
                    <input
                        type="text"
                        class="data"
                        placeholder="Type a path"
                        prop:value=path
                        on:input=move |ev| typed.set(Some(event_target_value(&ev)))
                    />
                </label>
                {move || {
                    (typed.get().is_some() && proposal.get().is_ok())
                        .then(|| {
                            view! {
                                <button type="button" on:click=move |_| typed.set(None)>
                                    "Use the proposed path"
                                </button>
                            }
                        })
                }}
                <button
                    type="submit"
                    class="primary"
                    disabled=move || busy.get() || root.with(String::is_empty) || profile.with(String::is_empty)
                >
                    "Register"
                </button>
            </form>
            <p class="note" aria-live="polite">
                {move || match preview() {
                    Ok(folder) => view! { "Registers " <code>{folder}</code> }.into_any(),
                    Err(reason) => reason.into_any(),
                }}
            </p>
            <p class="note small">
                "The folder must be empty or missing; registering it creates it with a marker file that ties it \
                to this store. {platform} is the front end's folder name for the system. Several managed folders \
                may hold the same system, each with its own selection profile."
            </p>
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}

/// One managed folder: what it is bound to, its latest plan, its builds and its files.
#[component]
pub fn ManagedFolderScreen() -> impl IntoView {
    let params = use_params_map();
    let id = move || {
        params
            .read()
            .get("id")
            .and_then(|id| id.parse::<i64>().ok())
    };
    let live = use_live();
    let refresh = RwSignal::new(0u64);
    let folder = LocalResource::new(move || {
        live.settled.track();
        refresh.track();
        let id = id();
        async move {
            match id {
                Some(id) => api::get::<ManagedFolder>(&format!("/managed-folders/{id}")).await,
                None => Err("That is not a managed folder number.".into()),
            }
        }
    });
    move || match folder.get() {
        None => view! { <Loading what="the managed folder" /> }.into_any(),
        Some(Err(message)) => view! {
            <h1>"Managed folder"</h1>
            <Problem message />
        }
        .into_any(),
        Some(Ok(folder)) => view! { <FolderView folder refresh /> }.into_any(),
    }
}

#[component]
fn FolderView(folder: ManagedFolder, refresh: RwSignal<u64>) -> impl IntoView {
    let id = folder.id;
    let set_href = format!("/source-sets/{}", api::segment(&folder.source_set));
    let profile_href = format!("/profiles/{}", api::segment(&folder.profile));
    let active_job = folder.active_job;
    let (title, system, source_set, profile) = (
        format!("Managed folder {}", location(&folder)),
        folder.system.clone(),
        folder.source_set.clone(),
        folder.profile.clone(),
    );
    view! {
        <h1>{title}</h1>
        <dl class="facts">
            <dt>"System"</dt>
            <dd>{system}</dd>
            <dt>"Source set"</dt>
            <dd>
                <A href=set_href>{source_set}</A>
            </dd>
            <dt>"Selection profile"</dt>
            <dd>
                <A href=profile_href>{profile}</A>
            </dd>
            <dt>"Root"</dt>
            <dd>
                {folder.root.clone()}
                {if folder.writable { ", writable" } else { ", not writable now" }}
            </dd>
            <dt>"Generation"</dt>
            <dd>{folder.generation}</dd>
            <dt>"Current build"</dt>
            <dd>{current_build(&folder.build)}</dd>
            <dt>"Files"</dt>
            <dd>{folder.files}</dd>
            <dt>"Registered"</dt>
            <dd>{when(&folder.created_at)}</dd>
        </dl>
        {active_job.map(|job| view! { <BuildProgress job /> })}
        <PlanForm folder=folder.clone() refresh />
        {folder.latest_plan.map(|plan| view! { <PlanPanel plan refresh /> })}
        <ChangeBinding folder=folder.clone() refresh />
        <Builds folder=id />
        <BuiltFiles folder=id />
    }
}

/// A build job's progress, live.
#[component]
fn BuildProgress(job: i64) -> impl IntoView {
    let live = use_live();
    view! {
        <p class="notice" role="status">
            {format!("Build job #{job} ")}
            {move || match live.book.with(|b| b.get(job).cloned()) {
                Some(l) if l.job.state.is_final() => {
                    let outcome = match &l.job.error {
                        Some(error) => format!("ended {}: {error}", l.job.state.as_str()),
                        None => format!("ended {}.", l.job.state.as_str()),
                    };
                    view! { <span>{outcome}</span> }.into_any()
                }
                Some(l) => view! { <span>"is building. "</span> {progress_view(&l)} }.into_any(),
                None => view! { <A href="/jobs">"Follow it on the Jobs screen"</A> }.into_any(),
            }}
        </p>
    }
}

/// Plans building one of the bound selection profile's selections of the source set.
#[component]
fn PlanForm(folder: ManagedFolder, refresh: RwSignal<u64>) -> impl IntoView {
    let id = folder.id;
    let set = folder.source_set.clone();
    let selections = LocalResource::new(move || {
        let path = format!("/selections?source_set={}", api::segment(&set));
        async move { api::get::<Vec<SelectionRecord>>(&path).await }
    });
    let folders = StoredValue::new(vec![folder]);
    let usable = move || {
        selections
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| !s.pruned && !folders_for(&folders.get_value(), s).is_empty())
            .collect::<Vec<_>>()
    };
    let chosen = RwSignal::new(String::new());
    Effect::new(move || {
        if chosen.get_untracked().is_empty()
            && let Some(first) = usable().into_iter().next()
        {
            chosen.set(first.id.to_string());
        }
    });
    let problem = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let Ok(selection) = chosen.get_untracked().parse::<i64>() else {
            return;
        };
        busy.set(true);
        problem.set(None);
        leptos::task::spawn_local(async move {
            let planned = api::post::<_, Plan>(
                &format!("/managed-folders/{id}/plans"),
                &PlanBuild { selection },
            )
            .await;
            busy.set(false);
            match planned {
                Ok(_) => refresh.update(|n| *n += 1),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    view! {
        <section aria-labelledby="plan-title">
            <h2 id="plan-title">"Plan a build"</h2>
            <p class="lede">
                "A plan lists what a build would write into the folder from a selection and the source set's \
                latest audit. Nothing is written until you approve it."
            </p>
            {move || match selections.get() {
                Some(Ok(_)) if usable().is_empty() => view! {
                    <p class="note">
                        "This source set has no selection with this selection profile yet. Make one from the source set."
                    </p>
                }
                .into_any(),
                _ => view! {
                    <form class="form" on:submit=submit>
                        <label>
                            "Selection"
                            <select prop:value=chosen on:change=move |ev| chosen.set(event_target_value(&ev))>
                                {move || {
                                    usable()
                                        .into_iter()
                                        .map(|s| {
                                            let label = format!(
                                                "#{}, version {}, DAT revision {}, {}",
                                                s.id,
                                                s.profile.version,
                                                s.dat_revision_version,
                                                when(&s.created_at),
                                            );
                                            view! { <option value=s.id.to_string()>{label}</option> }
                                        })
                                        .collect_view()
                                }}
                            </select>
                        </label>
                        <button type="submit" class="primary" disabled=move || busy.get() || chosen.with(String::is_empty)>
                            "Plan a build"
                        </button>
                    </form>
                }
                .into_any(),
            }}
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}

fn summary_view(s: &PlanSummary) -> AnyView {
    let counts = [
        ("Copy", s.copy, "count verified"),
        ("Repack", s.repack, "count verified"),
        ("Unchanged", s.unchanged, "count"),
        ("No source", s.no_source, "count review"),
        ("Conflict", s.conflict, "count review"),
        ("Blocked", s.blocked, "count review"),
    ];
    view! {
        <ul class="counts">
            {counts
                .into_iter()
                .map(|(what, n, class)| {
                    view! {
                        <li class=class>
                            <span class="n">{n}</span>
                            <span class="what">{what}</span>
                        </li>
                    }
                })
                .collect_view()}
        </ul>
    }
    .into_any()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ItemCol {
    Kind,
    Game,
    Target,
    Detail,
}

#[derive(Clone, PartialEq)]
struct ItemRow(PlanItem);

fn item_detail(item: &PlanItem) -> String {
    if let Some(reason) = &item.reason {
        return reason.clone();
    }
    let mut paths: Vec<String> = item
        .sources
        .iter()
        .map(|s| match &s.member {
            Some(member) => format!("{} ({member})", s.path),
            None => s.path.clone(),
        })
        .collect();
    paths.dedup();
    paths.join(", ")
}

impl Grid for ItemRow {
    type Col = ItemCol;
    const COLUMNS: &'static [ItemCol] = &[
        ItemCol::Kind,
        ItemCol::Game,
        ItemCol::Target,
        ItemCol::Detail,
    ];

    fn header(col: ItemCol) -> &'static str {
        match col {
            ItemCol::Kind => "Does",
            ItemCol::Game => "Game",
            ItemCol::Target => "File",
            ItemCol::Detail => "From, or why not",
        }
    }

    fn class(col: ItemCol) -> &'static str {
        match col {
            ItemCol::Kind => "c-state",
            ItemCol::Target | ItemCol::Detail => "c-wide data",
            ItemCol::Game => "c-wide",
        }
    }

    fn sortable(col: ItemCol) -> bool {
        matches!(col, ItemCol::Kind | ItemCol::Game)
    }

    fn cell(&self, col: ItemCol) -> AnyView {
        let i = &self.0;
        match col {
            ItemCol::Kind => item_kind_label(i.kind).into_any(),
            ItemCol::Game => i.game.clone().into_any(),
            ItemCol::Target => i.target.clone().into_any(),
            ItemCol::Detail => item_detail(i).into_any(),
        }
    }
}

/// A plan: what it would write, what it skips, what blocks it, and the approval.
#[component]
fn PlanPanel(plan: i64, refresh: RwSignal<u64>) -> impl IntoView {
    let live = use_live();
    let loaded = LocalResource::new(move || {
        live.settled.track();
        async move { api::get::<Plan>(&format!("/plans/{plan}")).await }
    });
    move || match loaded.get() {
        None => view! { <Loading what="the plan" /> }.into_any(),
        Some(Err(message)) => view! { <Problem message /> }.into_any(),
        Some(Ok(plan)) => view! { <PlanView plan refresh /> }.into_any(),
    }
}

#[component]
fn PlanView(plan: Plan, refresh: RwSignal<u64>) -> impl IntoView {
    let id = plan.id;
    let selection = plan.content.selection;
    let digest = StoredValue::new(plan.digest.clone());
    let kind = RwSignal::new(String::new());
    let all_items = StoredValue::new(plan.content.items.clone());
    let sorting = RwSignal::new(VecDeque::<(ItemCol, ColumnSort)>::new());
    let selected = RwSignal::new(None::<usize>);
    let shown = Memo::new(move |_| {
        let wanted = ItemKind::parse(&kind.get());
        let mut items: Vec<ItemRow> = all_items.with_value(|items| {
            by_kind(items)
                .into_iter()
                .filter(|(k, _)| wanted.is_none_or(|w| w == *k))
                .flat_map(|(_, items)| items.into_iter().cloned().map(ItemRow))
                .collect()
        });
        if let Some(sort) = sorting.with(sort_of) {
            items.sort_by(|a, b| {
                let ord = match sort.column {
                    ItemCol::Kind => a.0.kind.cmp(&b.0.kind),
                    _ => a.0.game.cmp(&b.0.game),
                };
                if sort.descending { ord.reverse() } else { ord }
            });
        }
        Arc::new(items)
    });
    let rows = Signal::derive(move || rows_of(shown.get().as_ref().clone()) as Rows<ItemRow>);
    let kinds: Vec<ItemKind> = by_kind(&plan.content.items)
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    let outcome = RwSignal::new(None::<Result<Job, String>>);
    let busy = RwSignal::new(false);
    let approve = move |_| {
        busy.set(true);
        let request = ApprovePlan {
            digest: digest.get_value(),
        };
        leptos::task::spawn_local(async move {
            let approved = api::post::<_, Job>(&format!("/plans/{id}/approval"), &request).await;
            busy.set(false);
            outcome.set(Some(approved));
            refresh.update(|n| *n += 1);
        });
    };
    let approvable = plan.approvable() && plan.approval.is_none();
    let writes = plan.summary.copy + plan.summary.repack;

    view! {
        <section aria-labelledby="latest-plan-title">
            <h2 id="latest-plan-title">{format!("Plan #{id}")}</h2>
            <dl class="facts">
                <dt>"Selection"</dt>
                <dd>
                    <A href=format!("/selections/{selection}")>{format!("#{selection}")}</A>
                    {plan.previous_selection.map(|p| format!(", replacing #{p} of the current build"))}
                </dd>
                <dt>"Audit"</dt>
                <dd>
                    {match plan.content.audit {
                        Some(a) => view! { <A href=format!("/audits/{a}")>{format!("#{a}")}</A> }.into_any(),
                        None => "None".into_any(),
                    }}
                </dd>
                <dt>"DAT revision"</dt>
                <dd>
                    <code class="hash">{short_sha(&plan.content.dat_revision).to_string()}</code>
                </dd>
                <dt>"Planned"</dt>
                <dd>{when(&plan.created_at)}</dd>
                <dt>"Digest"</dt>
                <dd>
                    <code class="hash">{plan.digest.clone()}</code>
                </dd>
                <dt>"Approval"</dt>
                <dd>
                    {match &plan.approval {
                        Some(a) => format!("Approved by {} · {}", a.actor, when(&a.at)),
                        None => "Not approved".to_string(),
                    }}
                    {plan.job.map(|j| view! { ", build job " <A href="/jobs">{format!("#{j}")}</A> })}
                </dd>
            </dl>
            {summary_view(&plan.summary)}
            {(!plan.content.blockers.is_empty())
                .then(|| {
                    view! {
                        <div class="notice" role="status">
                            <p>"You cannot approve this plan until these are fixed:"</p>
                            <ul class="plain">
                                {plan
                                    .content
                                    .blockers
                                    .iter()
                                    .map(|b| {
                                        let link = b
                                            .group
                                            .as_ref()
                                            .map(|g| {
                                                view! {
                                                    " " <A href=group_href(selection, g)>"Confirm a choice"</A>
                                                }
                                            });
                                        view! {
                                            <li>
                                                <strong>{blocker_label(b.kind)}</strong>
                                                {format!(": {}", b.message)}
                                                {link}
                                            </li>
                                        }
                                    })
                                    .collect_view()}
                            </ul>
                        </div>
                    }
                })}
            {approvable
                .then(|| {
                    view! {
                        <div class="form">
                            <button type="button" class="primary" disabled=busy on:click=approve>
                                {format!("Approve and build {writes} files")}
                            </button>
                        </div>
                    }
                })}
            {move || {
                outcome
                    .get()
                    .map(|outcome| match outcome {
                        Ok(job) => view! { <BuildProgress job=job.id /> }.into_any(),
                        Err(message) => view! { <Problem message /> }.into_any(),
                    })
            }}
            {(!plan.changes.is_empty())
                .then(|| {
                    view! {
                        <h3>"What changed since the current build"</h3>
                        <ul class="plain">
                            {plan
                                .changes
                                .iter()
                                .map(|c| {
                                    let what = match (c.before, c.after) {
                                        (None, _) => "added",
                                        (_, None) => "gone",
                                        _ => "changed",
                                    };
                                    view! {
                                        <li>
                                            {format!("{}: {what}, by {}", c.title, cause_label(c.cause))}
                                        </li>
                                    }
                                })
                                .collect_view()}
                        </ul>
                    }
                })}
            <h3>"Files"</h3>
            <ul class="plain small">
                {kinds
                    .iter()
                    .map(|k| view! { <li><strong>{item_kind_label(*k)}</strong>{format!(": {}", item_kind_note(*k))}</li> })
                    .collect_view()}
            </ul>
            <form class="form" on:submit=|ev| ev.prevent_default()>
                <label>
                    "Show"
                    <select prop:value=kind on:change=move |ev| kind.set(event_target_value(&ev))>
                        <option value="">"Every file"</option>
                        {kinds
                            .iter()
                            .map(|k| view! { <option value=k.as_str()>{item_kind_label(*k)}</option> })
                            .collect_view()}
                    </select>
                </label>
            </form>
            <DataTable
                id="plan-items"
                label="Plan items".to_string()
                rows
                sorting
                selected
                on_open=Callback::new(|_: ItemRow| {})
                empty="The plan has no files.".to_string()
            />
        </section>
    }
}

/// Rebinds the folder to another source set of the same DAT, or to another selection profile.
#[component]
fn ChangeBinding(folder: ManagedFolder, refresh: RwSignal<u64>) -> impl IntoView {
    let id = folder.id;
    let system = folder.system.clone();
    let sets = LocalResource::new(|| api::get::<Vec<SourceSetRecord>>("/source-sets"));
    let profiles = LocalResource::new(|| api::get::<Vec<ProfileRecord>>("/profiles"));
    let set = RwSignal::new(folder.source_set.clone());
    let profile = RwSignal::new(folder.profile.clone());
    let (was_set, was_profile) = (folder.source_set.clone(), folder.profile.clone());
    let problem = RwSignal::new(None::<String>);
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let (s, p) = (set.get_untracked(), profile.get_untracked());
        let request = ChangeManagedFolder {
            source_set: (s != was_set).then_some(s),
            profile: (p != was_profile).then_some(p),
        };
        leptos::task::spawn_local(async move {
            match api::patch::<_, ManagedFolder>(&format!("/managed-folders/{id}"), &request).await
            {
                Ok(_) => refresh.update(|n| *n += 1),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    view! {
        <section aria-labelledby="binding-title">
            <h2 id="binding-title">"Change what it builds"</h2>
            <p class="lede">
                "The next plan builds the new source set's or selection profile's selections. Files already built \
                stay until a build replaces them."
            </p>
            <form class="form" on:submit=submit>
                <label>
                    "Source set"
                    <select prop:value=set on:change=move |ev| set.set(event_target_value(&ev))>
                        {move || {
                            let system = system.clone();
                            sets.get()
                                .and_then(Result::ok)
                                .unwrap_or_default()
                                .into_iter()
                                .filter(move |s| s.dat == system)
                                .map(|s| view! { <option value=s.name.clone()>{s.name.clone()}</option> })
                                .collect_view()
                        }}
                    </select>
                </label>
                <label>
                    "Selection profile"
                    <select prop:value=profile on:change=move |ev| profile.set(event_target_value(&ev))>
                        {move || {
                            profiles
                                .get()
                                .and_then(Result::ok)
                                .unwrap_or_default()
                                .into_iter()
                                .filter(|p| p.latest.is_some())
                                .map(|p| view! { <option value=p.name.clone()>{p.name.clone()}</option> })
                                .collect_view()
                        }}
                    </select>
                </label>
                <button type="submit">"Change"</button>
            </form>
            {move || problem.get().map(|message| view! { <Problem message /> })}
        </section>
    }
}

fn build_line(b: BuildSummary) -> impl IntoView {
    view! {
        <li>
            <A href=format!("/builds/{}", b.id)>{format!("Build #{}", b.id)}</A>
            {format!(
                ", {}, selection #{}, {}: {} written, {} failed, {} not written",
                build_state_label(b.state).to_lowercase(),
                b.selection,
                when(b.finished_at.as_deref().unwrap_or(&b.started_at)),
                b.applied,
                b.failed,
                b.pending,
            )}
        </li>
    }
}

#[component]
fn Builds(folder: i64) -> impl IntoView {
    let live = use_live();
    let builds = LocalResource::new(move || {
        live.settled.track();
        async move { api::get::<Vec<BuildSummary>>(&format!("/managed-folders/{folder}/builds")).await }
    });
    view! {
        <section aria-labelledby="builds-title">
            <h2 id="builds-title">"Builds"</h2>
            {move || match builds.get() {
                None => view! { <Loading what="the builds" /> }.into_any(),
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                Some(Ok(list)) if list.is_empty() => {
                    view! { <p class="note">"Not built yet."</p> }.into_any()
                }
                Some(Ok(list)) => view! {
                    <ul class="plain">{list.into_iter().map(build_line).collect_view()}</ul>
                }
                .into_any(),
            }}
        </section>
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FileCol {
    Target,
    Game,
    Kind,
    Build,
    Size,
}

#[derive(Clone, PartialEq)]
struct FileRow(BuiltFile);

impl Grid for FileRow {
    type Col = FileCol;
    const COLUMNS: &'static [FileCol] = &[
        FileCol::Target,
        FileCol::Game,
        FileCol::Kind,
        FileCol::Build,
        FileCol::Size,
    ];

    fn header(col: FileCol) -> &'static str {
        match col {
            FileCol::Target => "File",
            FileCol::Game => "Game",
            FileCol::Kind => "Made by",
            FileCol::Build => "Build",
            FileCol::Size => "Bytes",
        }
    }

    fn class(col: FileCol) -> &'static str {
        match col {
            FileCol::Target => "c-wide data",
            FileCol::Game => "c-wide",
            FileCol::Kind => "c-state",
            FileCol::Build => "c-id",
            FileCol::Size => "c-num",
        }
    }

    fn sortable(col: FileCol) -> bool {
        matches!(col, FileCol::Target | FileCol::Build)
    }

    fn cell(&self, col: FileCol) -> AnyView {
        let f = &self.0;
        match col {
            FileCol::Target => f.target.clone().into_any(),
            FileCol::Game => f.game.clone().into_any(),
            FileCol::Kind => item_kind_label(f.kind).into_any(),
            FileCol::Build => {
                view! { <A href=format!("/builds/{}", f.build)>{format!("#{}", f.build)}</A> }
                    .into_any()
            }
            FileCol::Size => f.file.size.to_string().into_any(),
        }
    }
}

#[component]
fn BuiltFiles(folder: i64) -> impl IntoView {
    let live = use_live();
    let files = LocalResource::new(move || {
        live.settled.track();
        async move { api::get::<Vec<BuiltFile>>(&format!("/managed-folders/{folder}/files")).await }
    });
    let sorting = RwSignal::new(VecDeque::<(FileCol, ColumnSort)>::new());
    let selected = RwSignal::new(None::<usize>);
    let shown = Memo::new(move |_| {
        let mut items: Vec<FileRow> = files
            .get()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .map(FileRow)
            .collect();
        if let Some(sort) = sorting.with(sort_of) {
            items.sort_by(|a, b| {
                let ord = match sort.column {
                    FileCol::Build => a.0.build.cmp(&b.0.build),
                    _ => a.0.target.cmp(&b.0.target),
                };
                if sort.descending { ord.reverse() } else { ord }
            });
        }
        Arc::new(items)
    });
    let rows = Signal::derive(move || rows_of(shown.get().as_ref().clone()) as Rows<FileRow>);
    view! {
        <section aria-labelledby="files-title">
            <h2 id="files-title">"Built files"</h2>
            <p class="lede">"Each file the store built into the folder, with the build that wrote it."</p>
            {move || match files.get() {
                None => view! { <Loading what="the built files" /> }.into_any(),
                Some(Err(message)) => view! { <Problem message /> }.into_any(),
                Some(Ok(_)) => ().into_any(),
            }}
            <DataTable
                id="built-files"
                label="Built files".to_string()
                rows
                sorting
                selected
                on_open=Callback::new(|_: FileRow| {})
                empty="No files built yet.".to_string()
            />
        </section>
    }
}

/// One build: what it wrote, and what failed.
#[component]
pub fn BuildScreen() -> impl IntoView {
    let params = use_params_map();
    let id = move || {
        params
            .read()
            .get("id")
            .and_then(|id| id.parse::<i64>().ok())
    };
    let live = use_live();
    let detail = LocalResource::new(move || {
        live.settled.track();
        let id = id();
        async move {
            match id {
                Some(id) => api::get::<BuildDetail>(&format!("/builds/{id}")).await,
                None => Err("That is not a build number.".into()),
            }
        }
    });
    move || match detail.get() {
        None => view! { <Loading what="the build" /> }.into_any(),
        Some(Err(message)) => view! {
            <h1>"Build"</h1>
            <Problem message />
        }
        .into_any(),
        Some(Ok(detail)) => build_view(detail).into_any(),
    }
}

fn build_item_line(item: BuildItem) -> impl IntoView {
    view! {
        <li>
            <code>{item.target}</code>
            {format!(
                ": {}, {}",
                item_kind_label(item.kind).to_lowercase(),
                item_state_label(item.state).to_lowercase(),
            )}
            {item.error.map(|e| view! { <span class="failed">{format!(": {e}")}</span> })}
        </li>
    }
}

fn build_view(detail: BuildDetail) -> impl IntoView {
    let b = detail.build;
    view! {
        <h1>{format!("Build #{}", b.id)}</h1>
        <dl class="facts">
            <dt>"Managed folder"</dt>
            <dd>
                <A href=folder_href(b.folder)>{format!("#{}", b.folder)}</A>
            </dd>
            <dt>"State"</dt>
            <dd>{build_state_label(b.state)}</dd>
            <dt>"Plan"</dt>
            <dd>{format!("#{}", b.plan)}</dd>
            <dt>"Selection"</dt>
            <dd>
                <A href=format!("/selections/{}", b.selection)>{format!("#{}", b.selection)}</A>
            </dd>
            <dt>"Audit"</dt>
            <dd>
                <A href=format!("/audits/{}", b.audit)>{format!("#{}", b.audit)}</A>
            </dd>
            <dt>"DAT revision"</dt>
            <dd>
                <code class="hash">{b.dat_revision.clone()}</code>
            </dd>
            <dt>"Generation"</dt>
            <dd>{b.generation}</dd>
            <dt>"Started"</dt>
            <dd>{when(&b.started_at)}</dd>
            <dt>"Finished"</dt>
            <dd>{b.finished_at.as_deref().map(when).unwrap_or_default()}</dd>
        </dl>
        <ul class="counts">
            <li class="count verified">
                <span class="n">{b.applied}</span>
                <span class="what">"Written"</span>
            </li>
            <li class="count review">
                <span class="n">{b.failed}</span>
                <span class="what">"Failed"</span>
            </li>
            <li class="count">
                <span class="n">{b.pending}</span>
                <span class="what">"Not written"</span>
            </li>
        </ul>
        <section aria-labelledby="items-title">
            <h2 id="items-title">"Files"</h2>
            {if detail.items.is_empty() {
                view! { <p class="note">"The build had no files to write."</p> }.into_any()
            } else {
                view! { <ul class="plain">{detail.items.into_iter().map(build_item_line).collect_view()}</ul> }
                    .into_any()
            }}
        </section>
    }
}

/// On a selection's screen: plans building it into a managed folder bound to its source set and profile.
#[component]
pub fn PlanABuild(selection: SelectionRecord) -> impl IntoView {
    let folders = LocalResource::new(|| api::get::<Vec<ManagedFolder>>("/managed-folders"));
    let selection = StoredValue::new(selection);
    let problem = RwSignal::new(None::<String>);
    let go = navigator();
    let plan = move |folder: i64| {
        let id = selection.with_value(|s| s.id);
        leptos::task::spawn_local(async move {
            let planned = api::post::<_, Plan>(
                &format!("/managed-folders/{folder}/plans"),
                &PlanBuild { selection: id },
            )
            .await;
            match planned {
                Ok(_) => go.run(folder_href(folder)),
                Err(message) => problem.set(Some(message)),
            }
        });
    };
    view! {
        {move || {
            let all = folders.get().and_then(Result::ok).unwrap_or_default();
            let matching: Vec<ManagedFolder> = selection
                .with_value(|s| folders_for(&all, s).into_iter().cloned().collect());
            if matching.is_empty() {
                return view! {
                    <p class="note">
                        "To build this selection for your front end, "
                        <A href="/managed-folders">"register a managed folder"</A>
                        " with its source set and selection profile."
                    </p>
                }
                .into_any();
            }
            view! {
                <div class="form">
                    {matching
                        .into_iter()
                        .map(|f| {
                            let label = format!("Plan a build into {}", location(&f));
                            let id = f.id;
                            view! {
                                <button type="button" on:click=move |_| plan(id)>
                                    {label}
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
            }
            .into_any()
        }}
        {move || problem.get().map(|message| view! { <Problem message /> })}
    }
}
