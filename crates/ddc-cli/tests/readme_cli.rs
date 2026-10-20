//! The README's command-line guide against what `--help` prints (story
//! "Distribution: the 0.2.0 cask with the app"): every visible command and
//! option is in the guide, and the guide names no option that does not exist.

use std::path::Path;
use std::process::Command;

const GUIDE: &str = "## The command line";

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap()
}

fn readme() -> String {
    std::fs::read_to_string(root().join("README.md")).unwrap()
}

/// The text from the line `heading` to the next heading of its level or above.
fn section<'a>(text: &'a str, heading: &str) -> &'a str {
    let level = heading.chars().take_while(|c| *c == '#').count();
    let start = text.find(&format!("\n{heading}")).unwrap_or_else(|| panic!("no section {heading:?}")) + 1;
    let rest = &text[start..];
    let end = rest
        .match_indices('\n')
        .map(|(i, _)| i + 1)
        .find(|&i| {
            let hashes = rest[i..].chars().take_while(|c| *c == '#').count();
            (1..=level).contains(&hashes) && rest[i + hashes..].starts_with(' ')
        })
        .unwrap_or(rest.len());
    &rest[..end]
}

/// One command's help: its visible subcommands, long options and possible values.
struct Help {
    commands: Vec<String>,
    options: Vec<String>,
    values: Vec<String>,
}

fn help(path: &[&str]) -> Help {
    let out =
        Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner")).env_clear().args(path).arg("--help").output().unwrap();
    assert!(out.status.success(), "{path:?}: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    let mut h = Help { commands: Vec::new(), options: Vec::new(), values: Vec::new() };
    let mut in_commands = false;
    for line in text.lines() {
        if line.ends_with(':') && !line.starts_with(' ') {
            in_commands = line == "Commands:";
            continue;
        }
        let trimmed = line.trim_start();
        if in_commands && line.starts_with("  ") && !trimmed.is_empty() {
            let name = trimmed.split_whitespace().next().unwrap();
            if name != "help" {
                h.commands.push(name.to_owned());
            }
        } else if trimmed.starts_with('-') {
            let long = trimmed.split_whitespace().find(|w| w.starts_with("--")).unwrap();
            let long = long.trim_end_matches(',');
            if long != "--help" && long != "--version" {
                h.options.push(long.to_owned());
            }
            if let Some((_, values)) = line.split_once("[possible values: ") {
                h.values.extend(values.trim_end_matches(']').split(", ").map(str::to_owned));
            }
        }
    }
    h
}

/// `word` as a whole name: not part of a longer one such as `--min-age` of `--min`.
fn names(text: &str, word: &str) -> bool {
    let part_of_name = |c: char| c.is_ascii_alphanumeric() || c == '-';
    text.match_indices(word)
        .any(|(i, _)| !text[..i].ends_with(part_of_name) && !text[i + word.len()..].starts_with(part_of_name))
}

/// Every `--option` the text names.
fn options_named(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (i, _) in text.match_indices("--") {
        if text[..i].ends_with(|c: char| c.is_ascii_alphanumeric() || c == '-') {
            continue;
        }
        let name: String = text[i + 2..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        if name.starts_with(|c: char| c.is_ascii_lowercase()) {
            found.push(format!("--{name}"));
        }
    }
    found
}

#[test]
fn the_guide_covers_every_command_and_option_of_the_help() {
    let readme = readme();
    let guide = section(&readme, GUIDE);
    assert!(names(guide, "--help") && names(guide, "--version"), "the guide names --help and --version");

    let top = help(&[]);
    assert!(top.options.is_empty(), "top-level options beyond --help and --version: {:?}", top.options);
    assert!(top.commands.contains(&"scan".to_owned()) && top.commands.contains(&"schedule".to_owned()));
    for command in &top.commands {
        let part = section(guide, &format!("### `ddc {command}`"));
        let h = help(&[command]);
        // A command with subcommands (`schedule`): each is shown, with its options.
        let leaves: Vec<(String, Help)> = if h.commands.is_empty() {
            vec![(command.clone(), h)]
        } else {
            h.commands.iter().map(|sub| (format!("{command} {sub}"), help(&[command, sub]))).collect()
        };
        let mut known: Vec<String> = vec!["--help".into(), "--version".into()];
        for (leaf, h) in &leaves {
            assert!(
                part.lines().any(|l| l.starts_with(&format!("ddc {leaf}"))),
                "### `ddc {command}` shows `ddc {leaf}` in an example"
            );
            for option in &h.options {
                assert!(names(part, option), "### `ddc {command}` does not name {option} of `ddc {leaf}`");
            }
            for value in &h.values {
                assert!(names(part, value), "### `ddc {command}` does not name {value} of `ddc {leaf}`");
            }
            known.extend(h.options.iter().cloned());
        }
        for option in options_named(part) {
            assert!(known.contains(&option), "### `ddc {command}` names {option}, which `ddc {command}` does not have");
        }
    }
}

#[test]
fn the_disclaimer_sits_beside_the_license_and_the_readme_links_it() {
    assert!(root().join("DISCLAIMER.md").is_file());
    assert!(root().join("LICENSE.md").is_file());
    let readme = readme();
    let disclaimer = section(&readme, "## Disclaimer");
    assert!(disclaimer.contains("(DISCLAIMER.md)"), "{disclaimer}");
    let text = std::fs::read_to_string(root().join("DISCLAIMER.md")).unwrap();
    assert!(text.contains("(LICENSE.md)") && text.contains("own risk"));
}
