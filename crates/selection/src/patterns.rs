//! Checks the regular expressions in a Profile's overrides and post filters with the engine's own Python `re`,
//! because Retool skips an invalid one with only a warning, which would make a silently different Selection.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use title_select_api::selection::{Overrides, SelectionProfile};

use crate::Error;

/// Compiles each pattern as Retool matches it (`re.search(pattern, name, flags=re.I)`) and prints, per pattern,
/// `null` or the error.
const COMPILE: &str = r#"
import json, re, sys
problems = []
for pattern in json.load(sys.stdin):
    try:
        re.compile(pattern, flags=re.I)
        problems.append(None)
    except Exception as err:
        problems.append(str(err))
json.dump(problems, sys.stdout)
"#;

/// One regular expression in a Profile and where it is.
struct Regex<'a> {
    /// The line as written, such as `/^Pong` or `</^Pong>`.
    line: &'a str,
    /// The expression after the `/`.
    expression: &'a str,
    place: String,
}

fn regexes(profile: &SelectionProfile) -> Vec<Regex<'_>> {
    let mut scopes: Vec<(String, &Overrides, &[String])> =
        vec![("global".into(), &profile.overrides, &profile.post_filters)];
    for (name, system) in &profile.systems {
        scopes.push((name.clone(), &system.overrides, &system.post_filters));
    }
    let mut found = Vec::new();
    for (scope, overrides, post_filters) in scopes {
        push(
            &mut found,
            &overrides.exclude,
            format!("{scope} exclude overrides"),
            true,
        );
        push(
            &mut found,
            &overrides.include,
            format!("{scope} include overrides"),
            true,
        );
        push(
            &mut found,
            post_filters,
            format!("{scope} post filters"),
            false,
        );
    }
    found
}

fn push<'a>(found: &mut Vec<Regex<'a>>, lines: &'a [String], place: String, overrides: bool) {
    for line in lines {
        // Retool strips the `<…>` that extends an override to related releases before it matches.
        let unwrapped = match line.strip_prefix('<').and_then(|l| l.strip_suffix('>')) {
            Some(inner) if overrides => inner,
            _ => line.as_str(),
        };
        if let Some(expression) = unwrapped.strip_prefix('/') {
            found.push(Regex {
                line,
                expression,
                place: place.clone(),
            });
        }
    }
}

/// Refuses a Profile whose overrides or post filters hold a regular expression `python` cannot compile.
pub(crate) fn check(python: &Path, profile: &SelectionProfile) -> Result<(), Error> {
    let regexes = regexes(profile);
    if regexes.is_empty() {
        return Ok(());
    }
    let context = || format!("checking patterns with {}", python.display());
    let io = |source| Error::Io {
        context: context(),
        source,
    };
    let mut child = Command::new(python)
        .args(["-I", "-c", COMPILE])
        .env_clear()
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(io)?;
    let input: Vec<&str> = regexes.iter().map(|r| r.expression).collect();
    let mut stdin = child.stdin.take().expect("stdin is piped");
    stdin
        .write_all(&serde_json::to_vec(&input).expect("strings serialize"))
        .map_err(io)?;
    drop(stdin);
    let output = child.wait_with_output().map_err(io)?;
    if !output.status.success() {
        return Err(Error::Install(format!(
            "{} failed: {}",
            context(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let compiled: Vec<Option<String>> = serde_json::from_slice(&output.stdout)
        .map_err(|e| Error::Install(format!("{}: {e}", context())))?;
    let problems: Vec<String> = regexes
        .iter()
        .zip(compiled)
        .filter_map(|(regex, problem)| {
            let problem = problem?;
            Some(format!(
                "{line:?} in your {place} isn't a valid regular expression ({problem}). A line that starts \
                 with / is a regular expression: fix it, or remove the / to match the text as written.",
                line = regex.line,
                place = regex.place,
            ))
        })
        .collect();
    if problems.is_empty() {
        Ok(())
    } else {
        Err(Error::Patterns(problems))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use title_select_api::selection::SystemSettings;

    #[test]
    fn regexes_are_found_where_retool_reads_them() {
        let mut profile = SelectionProfile {
            overrides: Overrides {
                exclude: vec!["/^Pong".into(), "Pong".into(), "</(Demo)>".into()],
                include: vec!["|/not a regex".into()],
            },
            post_filters: vec!["/[".into(), "<not unwrapped>".into(), "</kept>".into()],
            ..Default::default()
        };
        profile.systems.insert(
            "Test - System".into(),
            SystemSettings {
                post_filters: vec!["/(".into()],
                ..Default::default()
            },
        );

        let found: Vec<(&str, &str, String)> = regexes(&profile)
            .into_iter()
            .map(|r| (r.line, r.expression, r.place))
            .collect();

        assert_eq!(
            found,
            [
                ("/^Pong", "^Pong", "global exclude overrides".to_owned()),
                ("</(Demo)>", "(Demo)", "global exclude overrides".to_owned()),
                ("/[", "[", "global post filters".to_owned()),
                ("/(", "(", "Test - System post filters".to_owned()),
            ]
        );
    }
}
