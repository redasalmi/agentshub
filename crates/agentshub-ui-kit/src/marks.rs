//! The letters that stand for a project wherever a layout lists it.

use std::collections::HashSet;

/// The mark of each project named in `names`, in their order: its initials,
/// or other letters of its name when a project before it has those.
pub fn marks<'a>(names: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut taken = HashSet::new();
    names
        .into_iter()
        .map(|name| {
            let mark = candidates(name)
                .into_iter()
                .find(|mark| !taken.contains(mark))
                .unwrap_or_else(|| initials(name));
            taken.insert(mark.clone());
            mark
        })
        .collect()
}

/// The marks project `name` could have, the likeliest first.
fn candidates(name: &str) -> Vec<String> {
    let letters: Vec<char> = name
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    let mut candidates = vec![initials(name)];
    if let [first, second, rest @ ..] = letters.as_slice() {
        let pairs = std::iter::once(second).chain(rest);
        candidates.extend(pairs.map(|other| [*first, *other].iter().collect::<String>()));
        candidates.extend(
            rest.iter()
                .map(|other| [*first, *second, *other].iter().collect()),
        );
    }
    candidates
}

/// The two letters a project's mark shows unless another project has them:
/// the first of its first two words, or its first two letters.
fn initials(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let letters: String = match words.as_slice() {
        [] => "?".into(),
        [word] => word.chars().take(2).collect(),
        [first, second, ..] => first
            .chars()
            .take(1)
            .chain(second.chars().take(1))
            .collect(),
    };
    letters.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{initials, marks};

    #[test]
    fn names_projects_with_two_letters() {
        assert_eq!(initials("agentshub"), "ag");
        assert_eq!(initials("simple-base"), "sb");
        assert_eq!(initials("My_Blog"), "mb");
        assert_eq!(initials("x"), "x");
        assert_eq!(initials("--"), "?");
    }

    #[test]
    fn tells_projects_with_the_same_initials_apart() {
        assert_eq!(marks(["test", "terraform", "frontend"]), ["te", "tr", "fr"]);
        // A project added twice from different folders.
        assert_eq!(marks(["notes", "notes"]), ["no", "nt"]);
        // Out of pairs, it takes three letters.
        assert_eq!(marks(["ab", "ac", "abc"]), ["ab", "ac", "abc"]);
        // Out of letters, it keeps its initials.
        assert_eq!(marks(["ab", "a-b"]), ["ab", "ab"]);
    }
}
