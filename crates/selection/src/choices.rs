//! Applies the Operator's choices to the engine's Selection: a choice is the set of releases checked in one Game
//! group, and it replaces the engine's winners there. A choice that no longer fits the DAT Revision or the settings
//! is flagged, never applied and never dropped.

use std::collections::{BTreeMap, BTreeSet};

use title_select_api::selection::{
    Alternative, ChoiceStatus, GroupChoice, Rule, Selection, SelectionGroup, Winner,
};

/// Applies `choices` (by group: the checked releases) to `selection`, flags every group with coverage drift, and
/// recounts the summary.
pub fn apply(selection: &mut Selection, choices: Option<&BTreeMap<String, BTreeSet<String>>>) {
    let without_metadata: BTreeSet<&str> = selection
        .coverage_drift
        .titles_without_metadata
        .iter()
        .map(String::as_str)
        .collect();
    let empty = BTreeMap::new();
    let choices = choices.unwrap_or(&empty);
    for group in &mut selection.groups {
        group.drift = group
            .winners
            .iter()
            .map(|w| &w.title)
            .chain(group.alternatives.iter().map(|a| &a.title))
            .chain(group.exclusions.iter().map(|e| &e.title))
            .any(|t| without_metadata.contains(t.as_str()));
        if let Some(checked) = choices.get(&group.group) {
            apply_to_group(group, checked);
        }
    }
    let present: BTreeSet<&str> = selection.groups.iter().map(|g| g.group.as_str()).collect();
    selection.stale_choices = choices
        .iter()
        .filter(|(group, _)| !present.contains(group.as_str()))
        .map(|(group, checked)| (group.clone(), checked.clone()))
        .collect();
    recount(selection);
}

fn apply_to_group(group: &mut SelectionGroup, checked: &BTreeSet<String>) {
    let picks: Vec<String> = group.winners.iter().map(|w| w.title.clone()).collect();
    let engine_unresolved = group.unresolved;
    let excluded: BTreeSet<&str> = group.exclusions.iter().map(|e| e.title.as_str()).collect();
    let checkable: BTreeSet<&str> = group
        .winners
        .iter()
        .map(|w| w.title.as_str())
        .chain(group.alternatives.iter().map(|a| a.title.as_str()))
        .collect();
    let conflicting: Vec<String> = checked
        .iter()
        .filter(|t| excluded.contains(t.as_str()))
        .cloned()
        .collect();
    let gone: Vec<String> = checked
        .iter()
        .filter(|t| !excluded.contains(t.as_str()) && !checkable.contains(t.as_str()))
        .cloned()
        .collect();
    let (status, problems) = if !gone.is_empty() {
        (ChoiceStatus::Stale, gone)
    } else if !conflicting.is_empty() {
        (ChoiceStatus::Conflicting, conflicting)
    } else if !engine_unresolved && picks.iter().cloned().collect::<BTreeSet<_>>() == *checked {
        (ChoiceStatus::MatchesDefault, Vec::new())
    } else {
        (ChoiceStatus::Applied, Vec::new())
    };
    group.choice = Some(GroupChoice {
        checked: checked.clone(),
        status,
        picks,
        engine_unresolved,
        problems,
    });
    if status != ChoiceStatus::Applied {
        return;
    }

    let mut releases: Vec<(String, Option<Rule>, Option<String>)> = Vec::new();
    for w in group.winners.drain(..) {
        releases.push((w.title, None, None));
    }
    for a in group.alternatives.drain(..) {
        releases.push((a.title, Some(a.rule), Some(a.clone_of)));
    }
    let first_checked = releases
        .iter()
        .find(|(t, _, _)| checked.contains(t))
        .map(|(t, _, _)| t.clone());
    let fallback = first_checked.unwrap_or_else(|| group.group.clone());
    let mut winners = Vec::new();
    let mut alternatives = Vec::new();
    for (title, rule, clone_of) in releases {
        if checked.contains(&title) {
            winners.push(Winner {
                title,
                rule: Rule::choice(),
                unresolved: false,
            });
            continue;
        }
        // An alternative the choice left alone keeps the engine's rule, and its winner if that is still one.
        let (rule, clone_of) = match (rule, clone_of) {
            (Some(rule), Some(clone_of)) if !rule.is_choice() => {
                let clone_of = if checked.contains(&clone_of) {
                    clone_of
                } else {
                    fallback.clone()
                };
                (rule, clone_of)
            }
            _ => (Rule::choice(), fallback.clone()),
        };
        alternatives.push(Alternative {
            title,
            clone_of,
            rule,
        });
    }
    group.winners = winners;
    group.alternatives = alternatives;
    group.unresolved = false;
}

/// Recounts the summary from the groups.
fn recount(selection: &mut Selection) {
    let summary = &mut selection.summary;
    summary.groups = selection.groups.len();
    summary.winners = selection.groups.iter().map(|g| g.winners.len()).sum();
    summary.alternatives = selection.groups.iter().map(|g| g.alternatives.len()).sum();
    summary.exclusions = selection.groups.iter().map(|g| g.exclusions.len()).sum();
    summary.unresolved = selection.groups.iter().filter(|g| g.unresolved).count();
    let status = |s: ChoiceStatus| {
        selection
            .groups
            .iter()
            .filter(|g| g.choice.as_ref().is_some_and(|c| c.status == s))
            .count()
    };
    summary.choices = status(ChoiceStatus::Applied);
    summary.conflicting = status(ChoiceStatus::Conflicting);
    summary.stale = status(ChoiceStatus::Stale) + selection.stale_choices.len();
}

#[cfg(test)]
mod tests {
    use super::*;
    use title_select_api::selection::{
        CoverageDrift, Exclusion, Role, SelectionPins, SelectionSummary,
    };

    fn rule(stage: &str) -> Rule {
        Rule {
            stage: stage.into(),
            reference: Some("REF0001".into()),
            message: "engine".into(),
        }
    }

    /// One group, `game`: USA wins, Europe and Japan are alternatives, the Demo is excluded.
    fn selection(unresolved: bool) -> Selection {
        let mut s = Selection {
            pins: SelectionPins::default(),
            dat_sha256: String::new(),
            system: "Test".into(),
            system_settings: false,
            summary: SelectionSummary::default(),
            coverage_drift: CoverageDrift {
                titles_without_metadata: vec!["Game (Japan)".into()],
                ..CoverageDrift::default()
            },
            groups: vec![SelectionGroup {
                group: "game".into(),
                unresolved,
                winners: vec![Winner {
                    title: "Game (USA)".into(),
                    rule: rule("Parent selection"),
                    unresolved,
                }],
                alternatives: vec![
                    Alternative {
                        title: "Game (Europe)".into(),
                        clone_of: "Game (USA)".into(),
                        rule: rule("Parent selection"),
                    },
                    Alternative {
                        title: "Game (Japan)".into(),
                        clone_of: "Game (USA)".into(),
                        rule: rule("Parent selection"),
                    },
                ],
                exclusions: vec![Exclusion {
                    title: "Game (USA) (Demo)".into(),
                    rule: rule("Exclusions"),
                    reason: "Demos".into(),
                }],
                choice: None,
                drift: false,
            }],
            stale_choices: BTreeMap::new(),
        };
        recount(&mut s);
        s
    }

    fn choose(group: &str, titles: &[&str]) -> BTreeMap<String, BTreeSet<String>> {
        BTreeMap::from([(
            group.to_owned(),
            titles.iter().map(|t| t.to_string()).collect(),
        )])
    }

    fn roles(s: &Selection) -> Vec<(Role, String, String)> {
        s.rows()
            .into_iter()
            .map(|r| (r.role, r.title, r.rule.stage))
            .collect()
    }

    #[test]
    fn a_choice_replaces_the_winners_and_is_recorded_as_the_rule_responsible() {
        let mut s = selection(false);
        apply(&mut s, Some(&choose("game", &["Game (Europe)"])));
        assert_eq!(
            roles(&s),
            vec![
                (Role::Winner, "Game (Europe)".into(), "choice".into()),
                (Role::Alternative, "Game (USA)".into(), "choice".into()),
                (
                    Role::Alternative,
                    "Game (Japan)".into(),
                    "Parent selection".into()
                ),
                (
                    Role::Exclusion,
                    "Game (USA) (Demo)".into(),
                    "Exclusions".into()
                ),
            ]
        );
        let rows = s.rows();
        assert!(rows[0].from_choice && rows[0].rule.reference.is_none());
        assert_eq!(rows[1].clone_of.as_deref(), Some("Game (Europe)"));
        assert_eq!(rows[2].clone_of.as_deref(), Some("Game (Europe)"));
        assert_eq!(
            s.groups[0].choice.as_ref().unwrap().status,
            ChoiceStatus::Applied
        );
        assert_eq!(
            s.groups[0].picks(),
            BTreeSet::from(["Game (USA)".to_owned()])
        );
        assert_eq!(s.summary.choices, 1);
        assert!(s.groups[0].drift, "Japan has no metadata");
    }

    #[test]
    fn a_choice_of_nothing_leaves_the_group_out() {
        let mut s = selection(false);
        apply(&mut s, Some(&choose("game", &[])));
        assert!(s.groups[0].winners.is_empty());
        assert_eq!(s.groups[0].alternatives.len(), 3);
        assert_eq!(s.summary.winners, 0);
        assert!(
            s.groups[0]
                .alternatives
                .iter()
                .all(|a| a.clone_of == "game")
        );
    }

    #[test]
    fn an_unresolved_group_needs_review_until_a_choice_confirms_it() {
        let mut s = selection(true);
        apply(&mut s, None);
        assert!(s.groups[0].needs_review());
        assert_eq!(s.summary.needs_review(), 1);

        let mut s = selection(true);
        apply(&mut s, Some(&choose("game", &["Game (USA)"])));
        let group = &s.groups[0];
        assert!(!group.needs_review());
        assert_eq!(group.choice.as_ref().unwrap().status, ChoiceStatus::Applied);
        assert!(group.engine_unresolved());
        assert!(!group.winners[0].unresolved);
        assert_eq!(s.summary.needs_review(), 0);
    }

    #[test]
    fn a_choice_the_engine_now_picks_matches_the_default() {
        let mut s = selection(false);
        apply(&mut s, Some(&choose("game", &["Game (USA)"])));
        let choice = s.groups[0].choice.as_ref().unwrap();
        assert_eq!(choice.status, ChoiceStatus::MatchesDefault);
        assert_eq!(s.groups[0].winners[0].rule.stage, "Parent selection");
        assert_eq!(s.summary.choices, 0);
    }

    #[test]
    fn a_choice_of_an_excluded_release_conflicts_and_never_wins() {
        let mut s = selection(false);
        apply(&mut s, Some(&choose("game", &["Game (USA) (Demo)"])));
        let group = &s.groups[0];
        let choice = group.choice.as_ref().unwrap();
        assert_eq!(choice.status, ChoiceStatus::Conflicting);
        assert_eq!(choice.problems, vec!["Game (USA) (Demo)".to_owned()]);
        assert_eq!(group.winners[0].title, "Game (USA)");
        assert!(group.needs_review());
        assert_eq!(s.summary.conflicting, 1);
    }

    #[test]
    fn a_choice_whose_release_or_group_is_gone_is_stale_and_kept() {
        let mut s = selection(false);
        apply(&mut s, Some(&choose("game", &["Game (Brazil)"])));
        let group = &s.groups[0];
        assert_eq!(group.choice.as_ref().unwrap().status, ChoiceStatus::Stale);
        assert_eq!(group.winners[0].title, "Game (USA)");
        assert!(group.needs_review());

        let mut s = selection(false);
        apply(&mut s, Some(&choose("gone", &["Gone (USA)"])));
        assert_eq!(
            s.stale_choices,
            BTreeMap::from([("gone".to_owned(), BTreeSet::from(["Gone (USA)".to_owned()]))])
        );
        assert_eq!(s.summary.stale, 1);
        assert_eq!(s.summary.needs_review(), 1);
    }
}
