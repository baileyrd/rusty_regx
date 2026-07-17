//! Executable backing for `docs/COOKBOOK.md`: every cookbook pattern is
//! asserted against its stated matches and rejects, so the cookbook
//! cannot rot. Keep the table and this file in sync.

use rusty_regx::Regex;

/// (pattern, matches, rejects) — one row per cookbook entry.
const RECIPES: &[(&str, &[&str], &[&str])] = &[
    (r"^[+-]?[0-9]+$", &["42", "-7"], &["1.5", ""]),
    (r"^[+-]?[0-9]+(\.[0-9]+)?$", &["3.14", "-2"], &[".5", "3."]),
    (r"^0[xX][[:xdigit:]]+$", &["0xCAFE"], &["0x", "CAFE"]),
    (r"^[A-Za-z_]\w*$", &["_foo1"], &["1foo"]),
    (r"\bword\b", &["a word."], &["password"]),
    (
        r"^[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}$",
        &["bob@x.com"],
        &["bob@x", "@x.com"],
    ),
    (r"^([0-9]{1,3}\.){3}[0-9]{1,3}$", &["10.0.0.1"], &["1.2.3"]),
    (
        r"^[0-9]{4}-[0-9]{2}-[0-9]{2}$",
        &["2026-07-17"],
        &["17/07/2026"],
    ),
    (r#""[^"]*""#, &[r#"say "hi" now"#], &["say hi"]),
    (r"^([A-Za-z_]\w*)=(.*)$", &["PATH=/bin"], &["=x"]),
    (r"^([0-9]+)\.([0-9]+)\.([0-9]+)$", &["1.2.3"], &["1.2"]),
];

#[test]
fn every_cookbook_recipe_works_as_documented() {
    for (pattern, matches, rejects) in RECIPES {
        let re = Regex::new_posix(pattern).unwrap_or_else(|e| panic!("{pattern}: {e}"));
        for text in *matches {
            assert!(re.is_match(text), "{pattern:?} should match {text:?}");
        }
        for text in *rejects {
            assert!(!re.is_match(text), "{pattern:?} should reject {text:?}");
        }
    }
}

#[test]
fn cookbook_capture_examples() {
    // Trim: capture the core of a padded string.
    let re = Regex::new_posix(r"^[[:space:]]*(.*[^[:space:]])[[:space:]]*$").unwrap();
    assert_eq!(re.captures(" hi there ").unwrap().get(1), Some("hi there"));
    // key=value splits into name and value.
    let re = Regex::new_posix(r"^([A-Za-z_]\w*)=(.*)$").unwrap();
    let caps = re.captures("PATH=/bin").unwrap();
    assert_eq!((caps.get(1), caps.get(2)), (Some("PATH"), Some("/bin")));
    // Version components.
    let re = Regex::new_posix(r"^([0-9]+)\.([0-9]+)\.([0-9]+)$").unwrap();
    let caps = re.captures("1.2.3").unwrap();
    assert_eq!(&caps[1], "1");
    assert_eq!(&caps[3], "3");
}
