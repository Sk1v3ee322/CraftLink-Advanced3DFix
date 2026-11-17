//! A unified-diff reader and applier that refuses to guess.
//!
//! The important difference from `patch(1)`: every context and removed line is
//! compared against the file on disk. If a single line does not match, the file is
//! left untouched and the caller is told which line and what was expected. Silent
//! misapplication onto a different release is the one failure mode that would cost a
//! recipient their afternoon, so it is made impossible.

use std::fmt;

/// One `--- old / +++ new` block of a patch.
pub struct Section {
    pub old: String,
    pub new: String,
    pub hunks: Vec<Hunk>,
}

pub struct Hunk {
    pub old_start: usize,
    pub old_count: usize,
    /// `(tag, text)` with tag one of `' '`, `'-'`, `'+'`, or `'\\'` for the
    /// "no newline at end of file" marker.
    pub body: Vec<(char, String)>,
}

#[derive(Debug)]
pub enum Failure {
    /// A context or removed line does not match the target file.
    Context {
        file: String,
        line: usize,
        expected: String,
        found: Option<String>,
    },
    /// Hunks are out of order or overlap.
    Order { file: String, line: usize },
    /// A hunk declares more context than the file has.
    Truncated { file: String, line: usize },
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Context {
                file,
                line,
                expected,
                found,
            } => {
                write!(
                    f,
                    "{file}:{line}: the file does not match the release this patch was made against\n    \
                     expected: {expected:?}\n    found   : {found:?}"
                )
            }
            Failure::Order { file, line } => write!(
                f,
                "{file}:{line}: hunks are out of order; the file has been edited above this point"
            ),
            Failure::Truncated { file, line } => write!(
                f,
                "{file}:{line}: the file is shorter than this patch expects.\n    \
                 This folder is not the release the patch was made for, or the file was edited."
            ),
        }
    }
}

impl std::error::Error for Failure {}

/// Parse a whole patch. Tolerates the trailing empty element of `split('\n')`.
pub fn parse(text: &str) -> Result<Vec<Section>, String> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut sections = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        if !lines[i].starts_with("--- ") {
            i += 1;
            continue;
        }
        if i + 1 >= lines.len() || !lines[i + 1].starts_with("+++ ") {
            return Err(format!("line {}: `---` without a following `+++`", i + 1));
        }
        let old = lines[i][4..].trim().to_string();
        let new = lines[i + 1][4..].trim().to_string();
        i += 2;

        let mut hunks = Vec::new();
        while i < lines.len() {
            let Some(hunk) = parse_hunk_header(lines[i]) else {
                break;
            };
            let (old_start, old_count) = hunk;
            i += 1;

            let mut body = Vec::new();
            let mut old_left = old_count;
            let mut new_left = hunk_new_count(lines[i - 1]);

            while i < lines.len() && (old_left > 0 || new_left > 0) {
                let raw = lines[i];
                let tag = raw.chars().next().unwrap_or(' ');
                let body_text = if raw.is_empty() { "" } else { &raw[1..] };
                match tag {
                    ' ' => {
                        old_left = old_left.saturating_sub(1);
                        new_left = new_left.saturating_sub(1);
                    }
                    '-' => old_left = old_left.saturating_sub(1),
                    '+' => new_left = new_left.saturating_sub(1),
                    '\\' => {
                        // "\ No newline at end of file"
                        body.push(('\\', String::new()));
                        i += 1;
                        continue;
                    }
                    _ => break,
                }
                body.push((tag, body_text.to_string()));
                i += 1;
            }

            if i < lines.len() && lines[i].starts_with('\\') {
                body.push(('\\', String::new()));
                i += 1;
            }

            hunks.push(Hunk {
                old_start,
                old_count,
                body,
            });
        }

        if hunks.is_empty() {
            return Err(format!("{new}: a file section with no hunks"));
        }
        sections.push(Section { old, new, hunks });
    }

    if sections.is_empty() {
        return Err("the patch contains no file sections".to_string());
    }
    Ok(sections)
}

fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    let rest = line.strip_prefix("@@ -")?;
    let (range, _) = rest.split_once(" +")?;
    let (start, count) = match range.split_once(',') {
        Some((s, c)) => (s, c),
        None => (range, "1"),
    };
    Some((start.parse().ok()?, count.parse().ok()?))
}

fn hunk_new_count(header: &str) -> usize {
    header
        .split(" +")
        .nth(1)
        .and_then(|tail| tail.split(" @@").next())
        .map(|range| match range.split_once(',') {
            Some((_, c)) => c.parse().unwrap_or(1),
            None => 1,
        })
        .unwrap_or(1)
}

/// The path a section writes to, without the `a/` or `b/` prefix.
pub fn destination(section: &Section) -> Option<&str> {
    let path = section.new.as_str();
    if path == "/dev/null" {
        None
    } else {
        Some(path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")).unwrap_or(path))
    }
}

/// The path a section reads from, or `None` for a new file.
pub fn source(section: &Section) -> Option<&str> {
    let path = section.old.as_str();
    if path == "/dev/null" {
        None
    } else {
        Some(path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")).unwrap_or(path))
    }
}

/// Apply a patch section to `original` text and return the new text.
///
/// `label` is only used in error messages.
pub fn apply(original: Option<&str>, hunks: &[Hunk], label: &str) -> Result<String, Failure> {
    let original_lines: Vec<&str> = match original {
        Some(text) => {
            let mut v: Vec<&str> = text.split('\n').collect();
            // A final newline produces one trailing empty element; drop it so line
            // numbers agree with what an editor shows.
            if v.last() == Some(&"") {
                v.pop();
            }
            v
        }
        None => Vec::new(),
    };

    let mut result: Vec<String> = Vec::new();
    let mut cursor = 0usize;
    let mut no_final_newline = false;

    for hunk in hunks {
        let offset = if hunk.old_count > 0 {
            hunk.old_start.saturating_sub(1)
        } else {
            hunk.old_start
        };
        if offset < cursor {
            return Err(Failure::Order {
                file: label.to_string(),
                line: offset + 1,
            });
        }
        if offset > original_lines.len() {
            return Err(Failure::Truncated {
                file: label.to_string(),
                line: offset + 1,
            });
        }

        // Everything between the previous hunk and this one is unchanged.
        for line in &original_lines[cursor..offset] {
            result.push((*line).to_string());
        }
        let mut pos = offset;

        for (tag, text) in &hunk.body {
            match tag {
                ' ' => {
                    let found = original_lines.get(pos);
                    if found != Some(&text.as_str()) {
                        return Err(Failure::Context {
                            file: label.to_string(),
                            line: pos + 1,
                            expected: text.clone(),
                            found: found.map(|s| (*s).to_string()),
                        });
                    }
                    result.push(text.clone());
                    pos += 1;
                }
                '-' => {
                    let found = original_lines.get(pos);
                    if found != Some(&text.as_str()) {
                        return Err(Failure::Context {
                            file: label.to_string(),
                            line: pos + 1,
                            expected: text.clone(),
                            found: found.map(|s| (*s).to_string()),
                        });
                    }
                    pos += 1;
                }
                '+' => result.push(text.clone()),
                _ => no_final_newline = true,
            }
        }

        cursor = pos;
    }

    for line in &original_lines[cursor..] {
        result.push((*line).to_string());
    }

    let mut text = result.join("\n");
    if !no_final_newline {
        text.push('\n');
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "--- a/greet.txt\n+++ b/greet.txt\n@@ -1,3 +1,4 @@\n hello\n-world\n+world!\n+again\n end\n";

    #[test]
    fn applies_a_simple_patch() {
        let sections = parse(SAMPLE).expect("the sample patch must parse");
        assert_eq!(sections.len(), 1);
        let original = "hello\nworld\nend\n";
        let out = apply(Some(original), &sections[0].hunks, "greet.txt").expect("context matches");
        assert_eq!(out, "hello\nworld!\nagain\nend\n");
    }

    #[test]
    fn refuses_a_file_from_another_version() {
        let sections = parse(SAMPLE).expect("the sample patch must parse");
        let original = "hello\nplanet\nend\n";
        let err = apply(Some(original), &sections[0].hunks, "greet.txt")
            .expect_err("a different file must not be patched");
        match err {
            Failure::Context { line, .. } => assert_eq!(line, 2, "the wrong line is named"),
            other => panic!("expected a context failure, got {other}"),
        }
    }

    #[test]
    fn creates_a_new_file() {
        let text = "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+a\n+b\n";
        let sections = parse(text).expect("the sample patch must parse");
        let out = apply(None, &sections[0].hunks, "new.txt").expect("a new file always applies");
        assert_eq!(out, "a\nb\n");
        assert_eq!(destination(&sections[0]), Some("new.txt"));
        assert_eq!(source(&sections[0]), None);
    }

    #[test]
    fn keeps_a_missing_final_newline() {
        let text = "--- a/x.txt\n+++ b/x.txt\n@@ -1 +1 @@\n-old\n+new\n\\ No newline at end of file\n";
        let sections = parse(text).expect("the sample patch must parse");
        let out = apply(Some("old"), &sections[0].hunks, "x.txt").expect("context matches");
        assert!(!out.ends_with('\n'), "the result must not gain a newline");
        assert_eq!(out, "new");
    }
}