//! The source set registration form's pure logic: the path a Path Template proposes for a DAT, and the live preview
//! of the folder the registration would use.

use title_select_api::Dat;
use title_select_api::paths::{PathTemplate, TokenValues, relative_path};

/// The path `template` proposes for `dat`, or why it proposes none, as a sentence for the form.
pub fn propose(template: &str, dat: &Dat) -> Result<String, String> {
    PathTemplate::parse(template)
        .and_then(|t| t.render(&TokenValues::for_dat(&dat.provider, &dat.name)))
        .map_err(|e| sentence(&e.to_string()))
}

/// The path the form registers: the one you typed, else the proposal.
pub fn chosen_path(typed: Option<&str>, proposal: &Result<String, String>) -> Option<String> {
    match typed {
        Some(typed) => Some(typed.to_owned()),
        None => proposal.as_ref().ok().cloned(),
    }
}

/// The folder a registration of `path` in the Root at `root_path` would use, or why the path is not valid.
pub fn preview(root_path: &str, path: &str) -> Result<String, String> {
    let rel = relative_path(path).map_err(|e| sentence(&format!("the path {e}")))?;
    Ok(format!("{}/{rel}", root_path.trim_end_matches('/')))
}

fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => {
            let mut out: String = first.to_uppercase().collect();
            out.push_str(chars.as_str());
            if !out.ends_with('.') {
                out.push('.');
            }
            out
        }
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dat(provider: &str, name: &str) -> Dat {
        Dat {
            dat: format!("{provider}/{name}"),
            provider: provider.into(),
            name: name.into(),
            revisions: Vec::new(),
        }
    }

    #[test]
    fn the_atari_dat_is_proposed_its_provider_and_name_and_previewed_in_its_root() {
        let atari = dat("No-Intro", "Atari - Atari 2600");

        let proposal = propose("{provider}/{dat}", &atari);

        assert_eq!(proposal.as_deref(), Ok("No-Intro/Atari - Atari 2600"));
        let path = chosen_path(None, &proposal).unwrap();
        assert_eq!(
            preview("/data/sets/", &path).as_deref(),
            Ok("/data/sets/No-Intro/Atari - Atari 2600")
        );
    }

    #[test]
    fn a_typed_path_overrides_the_proposal_and_an_unmapped_system_asks_for_one() {
        let studio = dat("No-Intro", "RCA - Studio II");

        let proposal = propose("{platform}", &studio);

        let reason = proposal.clone().unwrap_err();
        assert!(reason.starts_with("No platform folder name"), "{reason}");
        assert!(reason.ends_with("type the path."), "{reason}");
        assert_eq!(chosen_path(None, &proposal), None);
        assert_eq!(
            chosen_path(Some("studio2"), &proposal).as_deref(),
            Some("studio2")
        );
        assert_eq!(
            preview("/sets", "../elsewhere"),
            Err("The path must not contain `..`.".into())
        );
    }
}
