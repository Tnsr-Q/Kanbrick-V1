//! Loading Cypher seed files into the store (issue #11).
//!
//! SparrowDB executes one statement per call, so a multi-statement `.cypher`
//! file is split into individual statements before execution. The splitter is
//! quote-aware (so a `;` inside a string literal does not end a statement) and
//! strips `//` line comments. Each statement remembers the source line it began
//! on, so an execution failure is reported with a line number.

use kanbrick_core::{Error, Result};

use crate::store::Store;

/// One executable statement extracted from a seed file, with the 1-based source
/// line it started on (for diagnostics).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// The statement text, comments stripped and trimmed, without the trailing `;`.
    pub text: String,
    /// 1-based line number where the statement began.
    pub line: usize,
}

/// Split raw Cypher source into executable statements.
///
/// Splits on top-level `;`, ignoring semicolons inside `'...'` / `"..."`
/// literals (honoring `\`-escapes) and stripping `//` line comments. Blank or
/// comment-only statements are dropped.
pub fn split_statements(source: &str) -> Vec<Statement> {
    let mut statements = Vec::new();
    let mut buf = String::new();
    let mut start_line = 1usize;
    let mut line = 1usize;
    let mut buf_has_content = false;

    let mut in_squote = false;
    let mut in_dquote = false;
    let mut escaped = false;
    let mut in_line_comment = false;

    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' {
            line += 1;
            in_line_comment = false;
            if buf_has_content {
                buf.push(' ');
            }
            continue;
        }

        if in_line_comment {
            continue;
        }

        // Detect `//` line comment outside of string literals.
        if !in_squote && !in_dquote && c == '/' && chars.peek() == Some(&'/') {
            chars.next(); // consume second '/'
            in_line_comment = true;
            continue;
        }

        if in_squote {
            buf.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '\'' {
                in_squote = false;
            }
            continue;
        }
        if in_dquote {
            buf.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_dquote = false;
            }
            continue;
        }

        match c {
            '\'' => {
                mark_start(&mut buf_has_content, &mut start_line, line);
                in_squote = true;
                buf.push(c);
            }
            '"' => {
                mark_start(&mut buf_has_content, &mut start_line, line);
                in_dquote = true;
                buf.push(c);
            }
            ';' => {
                let trimmed = buf.trim();
                if !trimmed.is_empty() {
                    statements.push(Statement {
                        text: trimmed.to_string(),
                        line: start_line,
                    });
                }
                buf.clear();
                buf_has_content = false;
            }
            _ => {
                if !c.is_whitespace() {
                    mark_start(&mut buf_has_content, &mut start_line, line);
                }
                buf.push(c);
            }
        }
    }

    // Trailing statement with no terminating `;`.
    let trimmed = buf.trim();
    if !trimmed.is_empty() {
        statements.push(Statement {
            text: trimmed.to_string(),
            line: start_line,
        });
    }

    statements
}

/// Record the start line of a statement on the first non-trivial character.
fn mark_start(has_content: &mut bool, start_line: &mut usize, line: usize) {
    if !*has_content {
        *has_content = true;
        *start_line = line;
    }
}

/// Load Cypher `source` into `store`, executing each statement in order.
///
/// Returns the number of statements executed. On failure, the error names the
/// 1-based source line of the offending statement.
pub fn load_str(store: &Store, source: &str) -> Result<usize> {
    let statements = split_statements(source);
    let total = statements.len();
    for (idx, stmt) in statements.iter().enumerate() {
        store.execute(&stmt.text).map_err(|e| {
            Error::Query(format!(
                "seed load failed at line {}: {} (statement: {})",
                stmt.line,
                e,
                truncate(&stmt.text, 80)
            ))
        })?;
        if total >= 20 && (idx + 1) % 25 == 0 {
            tracing::info!(target: "kanbrick_store::seed", "loaded {}/{} statements", idx + 1, total);
        }
    }
    Ok(total)
}

/// Read a Cypher file from `path` and load it into `store`.
pub fn load_file(store: &Store, path: impl AsRef<std::path::Path>) -> Result<usize> {
    let path = path.as_ref();
    let source = std::fs::read_to_string(path)
        .map_err(|e| Error::Store(format!("cannot read seed file {}: {e}", path.display())))?;
    load_str(store, &source)
}

/// Truncate a string for inclusion in an error message.
fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_top_level_semicolons() {
        let src = "CREATE (:A {x: 1});\nCREATE (:B {y: 2});";
        let stmts = split_statements(src);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].line, 1);
        assert_eq!(stmts[1].line, 2);
    }

    #[test]
    fn ignores_semicolons_inside_strings() {
        let src = "CREATE (:A {note: 'a; b; c'});";
        let stmts = split_statements(src);
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].text.contains("a; b; c"));
    }

    #[test]
    fn strips_line_comments_and_tracks_start_line() {
        let src = "// header comment\n// another\nMATCH (n)\nRETURN n;";
        let stmts = split_statements(src);
        assert_eq!(stmts.len(), 1);
        assert_eq!(stmts[0].line, 3);
        assert!(!stmts[0].text.contains("comment"));
    }
}
