//! Selection Profiles and the explained Selection the engine derives from one DAT Revision (ADR 0001).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// The Operator's rules for choosing releases: global settings, with per-system settings that replace or add to
/// them. It holds no DAT content. Every Retool setting that affects Selection has a field here; settings that only
/// shape Retool's output DAT have none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SelectionProfile {
    /// Regions in order of preference. A region left out is filtered out. Empty means Retool's default order.
    pub region_order: Vec<String>,
    /// Languages in order of preference. Empty means the languages implied by the region order.
    pub language_order: Vec<String>,
    /// Languages for local names, in order. Retool uses it only to rename releases, so it has no effect on which
    /// releases are chosen; it is kept for parity.
    pub localization_order: Vec<String>,
    /// Video standards in order of preference. Empty means Retool's default order.
    pub video_order: Vec<String>,
    /// Kinds of release to leave out.
    pub exclude: BTreeSet<Category>,
    pub options: SelectionOptions,
    pub overrides: Overrides,
    /// Patterns a chosen release must match to stay chosen, in Retool's override syntax.
    pub post_filters: Vec<String>,
    /// Per-system settings, by the system name Retool derives from the DAT header, such as
    /// `Atari - Atari 2600 (No-Intro)`.
    pub systems: BTreeMap<String, SystemSettings>,
}

/// Settings for one system. An order, `exclude` or `options` that is present replaces the global one; overrides are
/// added to the global ones; post filters are added unless `replace_post_filters` is set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SystemSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub region_order: Option<Vec<String>>,
    /// An empty list means the languages implied by the effective region order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_order: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub localization_order: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_order: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude: Option<BTreeSet<Category>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<SelectionOptions>,
    pub overrides: Overrides,
    pub post_filters: Vec<String>,
    pub replace_post_filters: bool,
}

/// A kind of release that can be left out, as Retool categorizes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    AddOns,
    Aftermarket,
    Applications,
    Audio,
    BadDumps,
    Bios,
    BonusDiscs,
    Coverdiscs,
    Demos,
    Educational,
    Games,
    Manuals,
    /// Releases with ROMs that are missing in action.
    Mia,
    Multimedia,
    Pirate,
    Preproduction,
    Promotional,
    Unlicensed,
    Video,
}

impl Category {
    pub const ALL: [Category; 19] = [
        Category::AddOns,
        Category::Aftermarket,
        Category::Applications,
        Category::Audio,
        Category::BadDumps,
        Category::Bios,
        Category::BonusDiscs,
        Category::Coverdiscs,
        Category::Demos,
        Category::Educational,
        Category::Games,
        Category::Manuals,
        Category::Mia,
        Category::Multimedia,
        Category::Pirate,
        Category::Preproduction,
        Category::Promotional,
        Category::Unlicensed,
        Category::Video,
    ];
}

/// Preferences and switches that change how releases are chosen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SelectionOptions {
    /// Prefer releases that support RetroAchievements.
    pub prefer_retroachievements: bool,
    /// Prefer the oldest production versions over the newest.
    pub prefer_oldest: bool,
    /// Follow the region order strictly, even when a lower region supports a preferred language.
    pub prefer_regions: bool,
    /// Prefer licensed releases over unlicensed, aftermarket and pirate ones.
    pub prefer_licensed: bool,
    /// Prefer releases ripped from modern rereleases.
    pub prefer_modern_rips: bool,
    /// Leave out releases that support none of the languages in the language order.
    pub filter_languages: bool,
    /// Keep every release instead of one per Game; clone lists are ignored.
    pub disable_1g1r: bool,
    pub compilations: Compilations,
}

/// How compilations compete with individual releases.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compilations {
    /// Retool's default: individual releases, unless a compilation ranks higher or holds unique Games.
    #[default]
    Default,
    /// Always prefer individual releases.
    Individual,
    /// Keep individual releases and compilations, comparing each only with its own kind.
    Keep,
    /// Least duplication (a beta mode in Retool).
    Optimize,
}

/// Releases forced out or in, in Retool's override syntax: plain text matches part of a name, `|` a full name and
/// `/` a regular expression; wrapped in `<…>` the match extends to related releases.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Overrides {
    pub exclude: Vec<String>,
    pub include: Vec<String>,
}

/// The version of the trace records a Selection is derived from.
pub const SELECTION_TRACE_SCHEMA: u32 = 1;

/// An explained Selection: per Game group, the chosen releases, the alternatives and the exclusions, each with the
/// rule responsible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub pins: SelectionPins,
    /// SHA-256 of the DAT file: the DAT Revision the Selection was made from.
    pub dat_sha256: String,
    /// The system name Retool derived from the DAT header; it keys the per-system settings.
    pub system: String,
    /// Whether the Profile has settings for this system.
    pub system_settings: bool,
    pub summary: SelectionSummary,
    pub coverage_drift: CoverageDrift,
    /// Sorted by group name.
    pub groups: Vec<SelectionGroup>,
}

/// What a Selection was made with, so that it can be reproduced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionPins {
    pub retool_commit: String,
    /// SHA-256 over the ordered patch hashes.
    pub patch_set_sha256: String,
    /// The clone list and metadata commit.
    pub clone_list_commit: String,
    pub trace_schema: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionSummary {
    pub groups: usize,
    pub winners: usize,
    pub alternatives: usize,
    pub exclusions: usize,
    pub unresolved: usize,
}

/// Where the clone list and metadata no longer match the DAT.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageDrift {
    /// Whether there is a clone list for this system at all.
    pub clone_list: bool,
    /// Whether there is metadata for this system at all.
    pub metadata: bool,
    /// Clone-list search terms that match no release in the DAT.
    pub unmatched_clone_list_terms: Vec<String>,
    /// Releases in the DAT with no metadata entry.
    pub titles_without_metadata: Vec<String>,
}

/// One Game group: what was chosen, what was not, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectionGroup {
    pub group: String,
    /// Retool picked at least one winner by a tie-breaker; the pick is provisional and needs a decision.
    pub unresolved: bool,
    /// Usually one. None when every release in the group is excluded.
    pub winners: Vec<Winner>,
    pub alternatives: Vec<Alternative>,
    pub exclusions: Vec<Exclusion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Winner {
    pub title: String,
    /// The rule that decided it.
    pub rule: Rule,
    pub unresolved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alternative {
    pub title: String,
    /// The winner it is an alternative to.
    pub clone_of: String,
    /// The rule that removed it.
    pub rule: Rule,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exclusion {
    pub title: String,
    pub rule: Rule,
    /// Retool's reason, such as `Demos, kiosks, and samples`.
    pub reason: String,
}

/// A rule in Retool's selection, as its trace reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub stage: String,
    /// Retool's trace reference, such as `REF0014`; absent when no trace record names the release.
    #[serde(rename = "ref", default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    pub message: String,
}
