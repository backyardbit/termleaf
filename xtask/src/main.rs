mod e2e;

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use ra_ap_rustc_lexer::{FrontmatterAllowed, TokenKind, tokenize};

const SAFETY_PREFIX: &str = "// SAFETY:";
const FORBIDDEN_NAME_FRAGMENT: &str = "shape";
const MOCKING_CRATES: &[&str] = &["mockall", "mockers", "mocktopus", "faux", "mry", "double"];
const SKIPPED_DIRS: &[&str] = &["target", ".git"];

#[derive(Debug, PartialEq, Eq)]
enum Rule {
    Comment,
    ForbiddenName,
    MockingCrate,
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Rule::Comment => "no-comments",
            Rule::ForbiddenName => "no-shape-in-names",
            Rule::MockingCrate => "no-mocking-crates",
        };
        f.write_str(name)
    }
}

#[derive(Debug)]
struct Violation {
    rule: Rule,
    line: usize,
    message: String,
}

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    match task.as_deref() {
        Some("lint") => lint(),
        Some("check") => check(),
        Some("e2e") => e2e::run(&workspace_root()),
        _ => {
            eprintln!("usage: cargo xtask <lint|check|e2e>");
            eprintln!("  lint   run the anti-slop source checks");
            eprintln!("  check  run fmt, clippy, tests and lint");
            eprintln!(
                "  e2e    drive termleaf in Ghostty and herdr on a virtual display (CI only)"
            );
            ExitCode::FAILURE
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn check() -> ExitCode {
    let steps: [&[&str]; 3] = [
        &["fmt", "--all", "--check"],
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        &["test", "--workspace", "--quiet"],
    ];
    for args in steps {
        let passed = Command::new(env!("CARGO"))
            .args(args)
            .current_dir(workspace_root())
            .status()
            .is_ok_and(|status| status.success());
        if !passed {
            eprintln!("cargo {} failed", args.join(" "));
            return ExitCode::FAILURE;
        }
    }
    lint()
}

fn lint() -> ExitCode {
    let root = workspace_root();
    let mut reports = Vec::new();

    let mut sources = Vec::new();
    collect_rust_files(&root, &mut sources);
    for path in sources {
        let Ok(source) = fs::read_to_string(&path) else {
            eprintln!("could not read {}", path.display());
            return ExitCode::FAILURE;
        };
        for violation in check_source(&source) {
            reports.push((path.clone(), violation));
        }
    }

    let lockfile = root.join("Cargo.lock");
    if let Ok(lock) = fs::read_to_string(&lockfile) {
        for violation in check_lockfile(&lock) {
            reports.push((lockfile.clone(), violation));
        }
    }

    for (path, violation) in &reports {
        let relative = path.strip_prefix(&root).unwrap_or(path);
        eprintln!(
            "{}:{}: [{}] {}",
            relative.display(),
            violation.line,
            violation.rule,
            violation.message
        );
    }
    if reports.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!("{} anti-slop violation(s)", reports.len());
        ExitCode::FAILURE
    }
}

fn collect_rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if !SKIPPED_DIRS.iter().any(|skipped| name == *skipped) {
                collect_rust_files(&path, found);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

fn check_source(source: &str) -> Vec<Violation> {
    let mut violations = Vec::new();
    let mut offset = 0;
    let mut line = 1;
    for token in tokenize(source, FrontmatterAllowed::No) {
        let end = offset + usize::try_from(token.len).unwrap_or(usize::MAX);
        let text = source.get(offset..end).unwrap_or_default();
        match token.kind {
            TokenKind::LineComment { .. } if !text.starts_with(SAFETY_PREFIX) => {
                violations.push(comment_violation(line));
            }
            TokenKind::BlockComment { .. } => violations.push(comment_violation(line)),
            TokenKind::Ident | TokenKind::RawIdent
                if text.to_lowercase().contains(FORBIDDEN_NAME_FRAGMENT) =>
            {
                violations.push(Violation {
                    rule: Rule::ForbiddenName,
                    line,
                    message: format!(
                        "rename `{text}` for its domain role; \"shape\" describes structure, not ownership"
                    ),
                });
            }
            _ => {}
        }
        line += text.matches('\n').count();
        offset = end;
    }
    violations
}

fn comment_violation(line: usize) -> Violation {
    Violation {
        rule: Rule::Comment,
        line,
        message: "remove this comment; let names, types and tests carry the explanation (only `// SAFETY:` is allowed)".to_owned(),
    }
}

fn check_lockfile(lock: &str) -> Vec<Violation> {
    lock.lines()
        .enumerate()
        .filter_map(|(index, text)| {
            let name = text.strip_prefix("name = \"")?.strip_suffix('"')?;
            MOCKING_CRATES.contains(&name).then(|| Violation {
                rule: Rule::MockingCrate,
                line: index + 1,
                message: format!(
                    "drop `{name}`; replace dependencies in tests through a real trait and a faithful test implementation"
                ),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(source: &str) -> Vec<Rule> {
        check_source(source).into_iter().map(|v| v.rule).collect()
    }

    #[test]
    fn flags_every_comment_style() {
        let source = "// line\n/* block */\n/// doc\n//! inner doc\nfn main() {}\n";
        assert_eq!(
            rules(source),
            [Rule::Comment, Rule::Comment, Rule::Comment, Rule::Comment]
        );
    }

    #[test]
    fn allows_safety_comments() {
        let source = "fn f() {\n    // SAFETY: pointer is valid\n    unsafe {}\n}\n";
        assert!(rules(source).is_empty());
    }

    #[test]
    fn ignores_comment_markers_inside_strings() {
        let source = "fn f() { let url = \"https://example.com\"; let raw = r#\"/* x */\"#; }\n";
        assert!(rules(source).is_empty());
    }

    #[test]
    fn reports_the_line_of_the_violation() {
        let source = "fn f() {}\n\nfn g() {} // trailing\n";
        let lines: Vec<usize> = check_source(source).iter().map(|v| v.line).collect();
        assert_eq!(lines, [3]);
    }

    #[test]
    fn flags_forbidden_fragment_in_identifiers_case_insensitively() {
        let source = "struct PageShape;\nfn f() { let text_shape = 1; }\n";
        assert_eq!(rules(source), [Rule::ForbiddenName, Rule::ForbiddenName]);
    }

    #[test]
    fn allows_forbidden_fragment_inside_string_literals() {
        assert!(rules("fn f() { let s = \"shape\"; }\n").is_empty());
    }

    #[test]
    fn flags_mocking_crates_in_the_lockfile() {
        let lock = "[[package]]\nname = \"mockall\"\nversion = \"0.13.0\"\n\n[[package]]\nname = \"termleaf\"\n";
        let found: Vec<usize> = check_lockfile(lock).iter().map(|v| v.line).collect();
        assert_eq!(found, [2]);
    }
}
