//! Translates a Selection Profile into Retool's `user-config.yaml`, per-system settings files and flags.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use title_select_api::selection::{
    Category, Compilations, Overrides, SelectionOptions, SelectionProfile, SystemSettings,
};

use crate::Error;

/// Retool's user config schema version that the vendored Retool reads.
const CONFIG_VERSION: &str = "2.4.0";

/// The region, language and video names the vendored Retool knows, read from its `internal-config.json`.
pub(crate) struct Vocabulary {
    pub regions: Vec<String>,
    pub languages: BTreeSet<String>,
    pub videos: Vec<String>,
}

impl Vocabulary {
    pub fn parse(internal_config: &[u8]) -> Result<Self, Error> {
        #[derive(serde::Deserialize)]
        struct Internal {
            #[serde(rename = "defaultRegionOrder")]
            regions: serde_json::Map<String, serde_json::Value>,
            languages: serde_json::Map<String, serde_json::Value>,
            #[serde(rename = "defaultVideoOrder")]
            videos: Vec<String>,
        }
        let internal: Internal = serde_json::from_slice(internal_config)
            .map_err(|e| Error::Install(format!("internal-config.json: {e}")))?;
        Ok(Vocabulary {
            regions: internal.regions.keys().cloned().collect(),
            languages: internal.languages.keys().cloned().collect(),
            videos: internal.videos,
        })
    }

    /// Checks every name in the Profile, so that a typo is an error rather than a silently different Selection.
    pub fn check(&self, profile: &SelectionProfile) -> Result<(), Error> {
        let mut scopes = vec![(
            "profile".to_owned(),
            [
                Some(&profile.region_order),
                Some(&profile.language_order),
                Some(&profile.localization_order),
                Some(&profile.video_order),
            ],
        )];
        for (name, system) in &profile.systems {
            check_system_name(name)?;
            scopes.push((
                format!("system {name}"),
                [
                    system.region_order.as_ref(),
                    system.language_order.as_ref(),
                    system.localization_order.as_ref(),
                    system.video_order.as_ref(),
                ],
            ));
        }
        for (scope, [regions, languages, localizations, videos]) in scopes {
            type Known<'a> = &'a dyn Fn(&str) -> bool;
            let checks: [(&str, Option<&Vec<String>>, Known); 4] = [
                ("regions", regions, &|n| self.regions.iter().any(|r| r == n)),
                ("languages", languages, &|n| self.languages.contains(n)),
                ("localization languages", localizations, &|n| {
                    self.languages.contains(n)
                }),
                ("video standards", videos, &|n| {
                    self.videos.iter().any(|v| v == n)
                }),
            ];
            for (what, names, known) in checks {
                let unknown: Vec<&str> = names
                    .into_iter()
                    .flatten()
                    .map(String::as_str)
                    .filter(|n| !known(n))
                    .collect();
                if !unknown.is_empty() {
                    return Err(Error::Profile(format!(
                        "{scope}: unknown {what}: {}",
                        unknown.join(", ")
                    )));
                }
            }
        }
        Ok(())
    }
}

/// A system name becomes a file name in the run's system settings folder.
fn check_system_name(name: &str) -> Result<(), Error> {
    if name.is_empty()
        || name.starts_with('.')
        || name.contains(['/', '\\'])
        || name.chars().any(char::is_control)
    {
        return Err(Error::Profile(format!("invalid system name {name:?}")));
    }
    Ok(())
}

/// Retool's single-letter code for an exclusion category.
fn category_letter(category: Category) -> char {
    match category {
        Category::Applications => 'a',
        Category::Audio => 'A',
        Category::BadDumps => 'b',
        Category::Bios => 'B',
        Category::Coverdiscs => 'c',
        Category::Demos => 'd',
        Category::AddOns => 'D',
        Category::Educational => 'e',
        Category::Aftermarket => 'f',
        Category::Games => 'g',
        Category::Mia => 'k',
        Category::Manuals => 'm',
        Category::Multimedia => 'M',
        Category::BonusDiscs => 'o',
        Category::Pirate => 'p',
        Category::Preproduction => 'P',
        Category::Promotional => 'r',
        Category::Unlicensed => 'u',
        Category::Video => 'v',
    }
}

fn exclude_letters(categories: &BTreeSet<Category>) -> String {
    categories.iter().map(|c| category_letter(*c)).collect()
}

/// Retool's single-letter option codes that `options` turns on.
fn option_letters(options: &SelectionOptions) -> Vec<&'static str> {
    [
        (options.prefer_retroachievements, "c"),
        (options.disable_1g1r, "d"),
        (options.filter_languages, "l"),
        (options.prefer_oldest, "o"),
        (options.prefer_regions, "r"),
        (options.prefer_licensed, "y"),
        (options.prefer_modern_rips, "z"),
    ]
    .into_iter()
    .filter_map(|(on, letter)| on.then_some(letter))
    .collect()
}

fn compilations_letter(compilations: Compilations) -> Option<&'static str> {
    match compilations {
        Compilations::Default => None,
        Compilations::Individual => Some("i"),
        Compilations::Keep => Some("k"),
        Compilations::Optimize => Some("o"),
    }
}

/// The command-line flags for the Profile's global settings.
pub(crate) fn flags(profile: &SelectionProfile) -> Vec<String> {
    let mut flags: Vec<String> = option_letters(&profile.options)
        .into_iter()
        .map(|letter| format!("-{letter}"))
        .collect();
    if let Some(mode) = compilations_letter(profile.options.compilations) {
        flags.extend(["--compilations".to_owned(), mode.to_owned()]);
    }
    if !profile.exclude.is_empty() {
        flags.extend(["--exclude".to_owned(), exclude_letters(&profile.exclude)]);
    }
    flags
}

/// A YAML double-quoted scalar. JSON's string escapes are a subset of YAML's.
fn quoted(value: &str) -> String {
    serde_json::to_string(value).expect("a string serializes")
}

fn list(out: &mut String, key: &str, override_flag: Option<bool>, items: &[String]) {
    let _ = write!(out, "\n{key}:");
    if let Some(flag) = override_flag {
        let _ = write!(out, "\n- override: {flag}");
    }
    for item in items {
        let _ = write!(out, "\n- {}", quoted(item));
    }
}

fn overrides(out: &mut String, overrides: &Overrides) {
    list(out, "exclude", None, &overrides.exclude);
    list(out, "include", None, &overrides.include);
}

/// `user-config.yaml` for the Profile's global settings. An empty region or video order is Retool's default.
pub(crate) fn user_config(profile: &SelectionProfile, vocabulary: &Vocabulary) -> String {
    let or_default = |order: &[String], default: &[String]| {
        if order.is_empty() {
            default.to_vec()
        } else {
            order.to_vec()
        }
    };
    let mut out = format!("---\nconfig version: {CONFIG_VERSION}");
    list(&mut out, "language order", None, &profile.language_order);
    let regions = or_default(&profile.region_order, &vocabulary.regions);
    list(&mut out, "region order", None, &regions);
    list(
        &mut out,
        "localization order",
        None,
        &profile.localization_order,
    );
    let videos = or_default(&profile.video_order, &vocabulary.videos);
    list(&mut out, "video order", None, &videos);
    list(&mut out, "list prefix", None, &[]);
    list(&mut out, "list suffix", None, &[]);
    overrides(&mut out, &profile.overrides);
    list(&mut out, "filters", None, &profile.post_filters);
    list(&mut out, "gui settings", None, &[]);
    out.push('\n');
    out
}

/// Options Title Select always needs, which a system's `override options` would otherwise switch off: MIA and
/// RetroAchievements data feed the `mia` exclusion and the RetroAchievements preference, and an already-processed
/// DAT is processed again.
pub(crate) const ALWAYS_OPTIONS: [&str; 3] = ["labelmia", "labelretro", "reprocess"];

/// A system settings file, which Retool reads when the DAT's system name matches its file name.
pub(crate) fn system_config(system: &SystemSettings) -> String {
    let order = |out: &mut String, key: &str, order: &Option<Vec<String>>| match order {
        Some(items) => list(out, key, Some(true), items),
        None => list(out, key, Some(false), &[]),
    };
    let mut out = format!("---\nconfig version: {CONFIG_VERSION}");
    out.push_str("\npaths:\n- override: false");
    order(&mut out, "language order", &system.language_order);
    order(&mut out, "region order", &system.region_order);
    order(&mut out, "localization order", &system.localization_order);
    order(&mut out, "video order", &system.video_order);
    list(&mut out, "list prefix", None, &[]);
    list(&mut out, "list suffix", None, &[]);
    overrides(&mut out, &system.overrides);
    let filters_override = (system.replace_post_filters || !system.post_filters.is_empty())
        .then_some(system.replace_post_filters);
    list(&mut out, "filters", filters_override, &system.post_filters);
    let _ = write!(
        out,
        "\nexclusions and options:\n- override exclusions: {}\n- override options: {}",
        system.exclude.is_some(),
        system.options.is_some()
    );
    if let Some(categories) = system.exclude.as_ref().filter(|c| !c.is_empty()) {
        let _ = write!(out, "\n- exclude: {}", exclude_letters(categories));
    }
    if let Some(options) = &system.options {
        for option in option_letters(options).into_iter().chain(ALWAYS_OPTIONS) {
            let _ = write!(out, "\n- {option}");
        }
        if let Some(mode) = compilations_letter(options.compilations) {
            let _ = write!(out, "\n- compilations: {mode}");
        }
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocabulary() -> Vocabulary {
        Vocabulary {
            regions: vec!["USA".into(), "Europe".into(), "Japan".into()],
            languages: ["English".into(), "Japanese".into()].into(),
            videos: vec!["NTSC".into(), "PAL".into()],
        }
    }

    #[test]
    fn empty_orders_are_retools_defaults_and_strings_are_quoted() {
        let profile = SelectionProfile {
            overrides: Overrides {
                exclude: vec![r#"/^A "quoted" \(x\)"#.into()],
                include: vec![],
            },
            ..Default::default()
        };
        let yaml = user_config(&profile, &vocabulary());
        assert!(yaml.contains("\nregion order:\n- \"USA\"\n- \"Europe\"\n- \"Japan\"\n"));
        assert!(yaml.contains("\nvideo order:\n- \"NTSC\"\n- \"PAL\"\n"));
        assert!(yaml.contains("\nlanguage order:\nregion order:"));
        assert!(yaml.contains(r#"- "/^A \"quoted\" \\(x\\)""#));
    }

    #[test]
    fn system_settings_carry_override_flags_and_keep_required_options() {
        let system = SystemSettings {
            region_order: Some(vec!["Japan".into()]),
            exclude: Some([Category::Demos, Category::Bios].into()),
            options: Some(SelectionOptions {
                prefer_oldest: true,
                compilations: Compilations::Keep,
                ..Default::default()
            }),
            post_filters: vec!["|Kept (Japan)".into()],
            replace_post_filters: true,
            ..Default::default()
        };
        let yaml = system_config(&system);
        assert!(yaml.contains("\nregion order:\n- override: true\n- \"Japan\"\n"));
        assert!(yaml.contains("\nlanguage order:\n- override: false\n"));
        assert!(yaml.contains("\nfilters:\n- override: true\n- \"|Kept (Japan)\"\n"));
        assert!(yaml.contains("- override exclusions: true\n- override options: true\n"));
        assert!(yaml.contains("- exclude: Bd\n"));
        assert!(yaml.ends_with("- o\n- labelmia\n- labelretro\n- reprocess\n- compilations: k\n"));
    }

    #[test]
    fn unknown_names_are_refused() {
        let mut profile = SelectionProfile {
            region_order: vec!["USA".into(), "Atlantis".into()],
            ..Default::default()
        };
        let err = vocabulary().check(&profile).unwrap_err().to_string();
        assert!(err.contains("Atlantis"), "{err}");
        profile.region_order.clear();
        profile
            .systems
            .insert("../escape".into(), SystemSettings::default());
        assert!(vocabulary().check(&profile).is_err());
    }
}
