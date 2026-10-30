//! The managed folder and build screens' pure logic: labels, the plan's items by what they do, and which managed
//! folders a selection can be built into.

use title_select_api::SelectionRecord;
use title_select_api::build::{
    BlockerKind, BuildState, Cause, ItemKind, ItemState, ManagedFolder, PlanItem,
};

pub fn item_kind_label(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Copy => "Copy",
        ItemKind::Repack => "Repack",
        ItemKind::Unchanged => "Unchanged",
        ItemKind::NoSource => "No source",
        ItemKind::Conflict => "Conflict",
        ItemKind::Blocked => "Blocked",
    }
}

/// What a group of items means, for the heading over them.
pub fn item_kind_note(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Copy => "Copied as they are: the source is already this game's TorrentZip.",
        ItemKind::Repack => "Written as a TorrentZip from the verified ROMs.",
        ItemKind::Unchanged => "Already in the folder, built from the same ROMs.",
        ItemKind::NoSource => "No audited file holds every ROM, so these are skipped.",
        ItemKind::Conflict => {
            "A file the store did not build, or one changed since it was built, is at the target. It is never \
             overwritten, so these are skipped."
        }
        ItemKind::Blocked => "These cannot be built as planned, so they are skipped.",
    }
}

pub fn cause_label(cause: Cause) -> &'static str {
    match cause {
        Cause::Settings => "your settings",
        Cause::Choices => "your choices",
        Cause::DatRevision => "the DAT revision",
    }
}

pub fn blocker_label(kind: BlockerKind) -> &'static str {
    match kind {
        BlockerKind::NoAudit => "No audit",
        BlockerKind::RevisionMismatch => "DAT revision mismatch",
        BlockerKind::Unresolved => "Unresolved group",
        BlockerKind::ReadOnlyRoot => "Read-only root",
        BlockerKind::FolderMissing => "Folder missing",
        BlockerKind::Marker => "Marker",
        BlockerKind::Replacement => "Replacement",
        BlockerKind::Removal => "Removal",
    }
}

pub fn build_state_label(state: BuildState) -> &'static str {
    match state {
        BuildState::Running => "Running",
        BuildState::Succeeded => "Succeeded",
        BuildState::Failed => "Failed",
        BuildState::Interrupted => "Interrupted",
    }
}

pub fn item_state_label(state: ItemState) -> &'static str {
    match state {
        ItemState::Pending => "Not written",
        ItemState::Applied => "Written",
        ItemState::Failed => "Failed",
    }
}

/// The plan's items by kind, the ones a build writes first, leaving out kinds with none.
pub fn by_kind(items: &[PlanItem]) -> Vec<(ItemKind, Vec<&PlanItem>)> {
    ItemKind::ALL
        .into_iter()
        .map(|kind| (kind, items.iter().filter(|i| i.kind == kind).collect()))
        .filter(|(_, items): &(ItemKind, Vec<&PlanItem>)| !items.is_empty())
        .collect()
}

/// The managed folders `selection` can be built into: those bound to its source set and selection profile.
pub fn folders_for<'a>(
    folders: &'a [ManagedFolder],
    selection: &SelectionRecord,
) -> Vec<&'a ManagedFolder> {
    folders
        .iter()
        .filter(|f| f.source_set == selection.source_set && f.profile == selection.profile.name)
        .collect()
}

/// Where you confirm an unresolved group: its selection, opened at the group.
pub fn group_href(selection: i64, group: &str) -> String {
    format!("/selections/{selection}?group={}", url_component(group))
}

/// Percent-encodes everything but unreserved characters, as `encodeURIComponent` does.
fn url_component(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use title_select_api::ProfileVersionRef;
    use title_select_api::selection::{SelectionPins, SelectionSummary};

    fn item(game: &str, kind: ItemKind) -> PlanItem {
        PlanItem {
            group: game.to_lowercase(),
            game: game.into(),
            target: format!("{game}.zip"),
            kind,
            roms: Vec::new(),
            sources: Vec::new(),
            reason: None,
            existing: None,
        }
    }

    fn folder(id: i64, source_set: &str, profile: &str) -> ManagedFolder {
        ManagedFolder {
            id,
            root: "play".into(),
            path: "atari2600".into(),
            system: "No-Intro/Atari - Atari 2600".into(),
            source_set: source_set.into(),
            profile: profile.into(),
            generation: 0,
            created_at: String::new(),
            writable: true,
            build: None,
            files: 0,
            active_job: None,
            latest_plan: None,
        }
    }

    #[test]
    fn items_are_grouped_by_kind_with_written_ones_first_and_empty_kinds_left_out() {
        let items = [
            item("Pong", ItemKind::NoSource),
            item("Combat", ItemKind::Repack),
            item("Yars", ItemKind::Copy),
            item("Adventure", ItemKind::Repack),
        ];

        let grouped = by_kind(&items);

        let shape: Vec<(ItemKind, Vec<&str>)> = grouped
            .iter()
            .map(|(k, items)| (*k, items.iter().map(|i| i.game.as_str()).collect()))
            .collect();
        assert_eq!(
            shape,
            [
                (ItemKind::Copy, vec!["Yars"]),
                (ItemKind::Repack, vec!["Combat", "Adventure"]),
                (ItemKind::NoSource, vec!["Pong"]),
            ]
        );
    }

    #[test]
    fn a_selection_is_offered_the_folders_bound_to_its_source_set_and_profile() {
        let folders = [
            folder(1, "Atari", "Mine"),
            folder(2, "Atari", "Theirs"),
            folder(3, "Other", "Mine"),
            folder(4, "Atari", "Mine"),
        ];
        let selection = SelectionRecord {
            id: 7,
            source_set: "Atari".into(),
            dat_revision_sha256: String::new(),
            dat_revision_version: String::new(),
            profile: ProfileVersionRef {
                name: "Mine".into(),
                version: 1,
            },
            latest_profile_version: 1,
            job: None,
            system: String::new(),
            system_settings: false,
            pins: SelectionPins::default(),
            summary: SelectionSummary::default(),
            created_at: String::new(),
            pruned: false,
        };

        let ids: Vec<i64> = folders_for(&folders, &selection)
            .iter()
            .map(|f| f.id)
            .collect();

        assert_eq!(ids, [1, 4]);
    }

    #[test]
    fn an_unresolved_group_links_to_its_selection_with_the_name_escaped() {
        assert_eq!(
            group_href(7, "café & co/1"),
            "/selections/7?group=caf%C3%A9%20%26%20co%2F1"
        );
    }
}
