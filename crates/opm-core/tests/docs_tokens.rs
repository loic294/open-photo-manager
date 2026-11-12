use std::fmt::Write;
use std::path::Path;

use chrono::NaiveDate;
use opm_core::pattern::{self, FileContext, Pattern, TokenGroup};

const START: &str = "<!-- tokens:start -->";
const END: &str = "<!-- tokens:end -->";
const RENAME_START: &str = "<!-- rename-tokens:start -->";
const RENAME_END: &str = "<!-- rename-tokens:end -->";

fn token_table() -> Result<String, std::fmt::Error> {
    let mut table =
        String::from("\n| Group | Token | Meaning | Sample value |\n|---|---|---|---|\n");
    for entry in pattern::describe() {
        let group = match entry.group {
            TokenGroup::Date => "Date",
            TokenGroup::File => "File",
            TokenGroup::Other => "Other",
        };
        let spellings = std::iter::once(entry.name)
            .chain(entry.aliases.iter().copied())
            .map(|name| format!("`{{{name}}}`"))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            table,
            "| {group} | {spellings} | {} | `{}` |",
            entry.description, entry.example
        )?;
    }
    Ok(table)
}

fn rename_token_table() -> Result<String, std::fmt::Error> {
    let mut table =
        String::from("\n| Group | Token | Meaning | Sample value |\n|---|---|---|---|\n");
    for entry in opm_core::rename::tokens().tokens {
        let group = match entry.group.as_str() {
            "date" => "Date",
            "file" => "File",
            "other" => "Other",
            _ => entry.group.as_str(),
        };
        let spellings = std::iter::once(entry.name)
            .chain(entry.aliases)
            .map(|name| format!("`{{{name}}}`"))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            table,
            "| {group} | {spellings} | {} | `{}` |",
            entry.description, entry.example
        )?;
    }
    Ok(table)
}

#[test]
fn usage_token_table_matches_registry() -> Result<(), Box<dyn std::error::Error>> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/usage.md");
    let text = std::fs::read_to_string(&path).expect("read docs/usage.md");
    check_group_examples(&text)?;
    assert_eq!(text.matches(START).count(), 1, "expected one start marker");
    assert_eq!(text.matches(END).count(), 1, "expected one end marker");
    assert_eq!(
        text.matches(RENAME_START).count(),
        1,
        "expected one rename start marker"
    );
    assert_eq!(
        text.matches(RENAME_END).count(),
        1,
        "expected one rename end marker"
    );
    let start = text.find(START).unwrap() + START.len();
    let end = text.find(END).unwrap();
    assert!(start <= end, "token markers must be in order");
    let expected = token_table()?;
    let rename_start = text.find(RENAME_START).unwrap() + RENAME_START.len();
    let rename_end = text.find(RENAME_END).unwrap();
    assert!(
        rename_start <= rename_end,
        "rename token markers must be in order"
    );
    let rename_expected = rename_token_table()?;
    if std::env::var_os("UPDATE_DOCS_TOKENS").is_some() {
        let updated = format!("{}{expected}{}", &text[..start], &text[end..]);
        let rename_start = updated.find(RENAME_START).unwrap() + RENAME_START.len();
        let rename_end = updated.find(RENAME_END).unwrap();
        std::fs::write(
            &path,
            format!(
                "{}{rename_expected}{}",
                &updated[..rename_start],
                &updated[rename_end..]
            ),
        )
        .expect("regenerate the marked token tables");
    } else {
        assert_eq!(
            &text[start..end],
            expected,
            "Regenerate from the repository root: UPDATE_DOCS_TOKENS=1 cargo test -p opm-core --test docs_tokens usage_token_table_matches_registry"
        );
        assert_eq!(
            &text[rename_start..rename_end],
            rename_expected,
            "Regenerate from the repository root: UPDATE_DOCS_TOKENS=1 cargo test -p opm-core --test docs_tokens usage_token_table_matches_registry"
        );
    }
    Ok(())
}

fn check_group_examples(text: &str) -> Result<(), Box<dyn std::error::Error>> {
    let context = FileContext {
        file_name: "IMG_0042.CR3".to_owned(),
        captured: NaiveDate::from_ymd_opt(2026, 10, 5)
            .ok_or("invalid sample date")?
            .and_hms_opt(14, 7, 9)
            .ok_or("invalid sample time")?,
        camera: Some("Canon EOS R5".to_owned()),
        folder: "Holiday".to_owned(),
    };
    for (pattern, destination) in [
        ("{YYYY}/{mm}", "2026/10/IMG_0042.CR3"),
        ("{name}_copy.{ext}", "IMG_0042_copy.CR3"),
        (
            "{camera}/{folder}/{filename}",
            "Canon EOS R5/Holiday/IMG_0042.CR3",
        ),
    ] {
        assert_eq!(
            pattern.parse::<Pattern>()?.render(&context).as_str(),
            destination
        );
        assert!(
            text.contains(&format!("`{pattern}` produces `{destination}`")),
            "missing worked example for {pattern}"
        );
    }
    Ok(())
}
