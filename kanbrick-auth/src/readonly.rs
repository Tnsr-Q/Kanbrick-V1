//! Read-only classifier for the guest graph channel (P16.1, ADR-0022).
//!
//! [`ensure_read_only`] refuses any Cypher statement that could mutate the
//! graph before it reaches the store. The guest `query_graph` host import is a
//! *read* channel by contract (README / docs/SECURITY.md); the host is the
//! graph's only writer. The classifier is deliberately **fail-closed**:
//!
//! * write-clause keywords are matched as whole word tokens anywhere outside
//!   string literals and backtick-quoted identifiers — a keyword carried as
//!   data (`'MERGE'`) passes, while a keyword-shaped bare property access
//!   (`n.set`) is refused: a false positive we accept over any false negative
//!   (backtick the identifier to read such a property);
//! * a **comment** (`//` or `/* … */`) in a guest statement is refused
//!   outright, not stripped. A host-generated read query never needs one, and
//!   refusing them removes any dependence on how the pinned engine's lexer
//!   treats a comment that splits a keyword (`CRE/**/ATE`) — the whole class is
//!   simply rejected;
//! * an unterminated string literal is unclassifiable and is refused too.

use kanbrick_core::{Error, Result};

/// Cypher write/DDL vocabulary. The pinned SparrowDB dialect (ADR-0001) only
/// executes a subset of these today (`CREATE`/`MERGE`/`SET`/`DELETE`); the rest
/// are refused anyway so the gate stays closed if the engine grows support.
const WRITE_KEYWORDS: &[&str] = &[
    "CREATE", "MERGE", "SET", "DELETE", "DETACH", "REMOVE", "DROP", "FOREACH", "LOAD", "CALL",
];

/// Refuse `cypher` unless it is provably free of write clauses.
pub(crate) fn ensure_read_only(cypher: &str) -> Result<()> {
    let scannable = strip_opaque_regions(cypher)?;
    let mut word = String::new();
    for ch in scannable.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            word.push(ch.to_ascii_uppercase());
        } else if !word.is_empty() {
            if WRITE_KEYWORDS.contains(&word.as_str()) {
                return Err(refused(&format!("write clause {word}")));
            }
            word.clear();
        }
    }
    Ok(())
}

fn refused(what: &str) -> Error {
    Error::Auth(format!("read-only guest graph channel: {what} refused"))
}

/// Blank out string literals (`'…'` / `"…"` with backslash escapes) and
/// backtick identifiers so the keyword scan only sees executable clause
/// positions. A comment marker (`//` or `/*`) is **refused**, not stripped
/// (see the module doc). An unterminated string literal is refused too — a
/// statement we cannot classify never runs (fail closed).
fn strip_opaque_regions(cypher: &str) -> Result<String> {
    let mut out = String::with_capacity(cypher.len());
    let mut chars = cypher.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' | '"' | '`' => {
                let quote = ch;
                let mut terminated = false;
                let mut escaped = false;
                for c in chars.by_ref() {
                    if escaped {
                        escaped = false;
                    } else if c == '\\' && quote != '`' {
                        escaped = true;
                    } else if c == quote {
                        terminated = true;
                        break;
                    }
                }
                if !terminated {
                    return Err(refused("unterminated quoted region"));
                }
                out.push(' ');
            }
            '/' if matches!(chars.peek(), Some('/') | Some('*')) => {
                return Err(refused("comment"));
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanbrick_core::ErrorKind;

    fn refused_kind(query: &str) -> ErrorKind {
        ensure_read_only(query).unwrap_err().kind()
    }

    #[test]
    fn allows_plain_reads() {
        for q in [
            "MATCH (c:Company) RETURN c.company_id, c.name, c.segment",
            "MATCH (p:Person {email: $email}) RETURN p.full_name",
            "MATCH (a)-[:REPORTS_TO]->(b) WITH b RETURN b.email",
            "UNWIND [1, 2, 3] AS n RETURN n",
            "MATCH (c:Company) RETURN c.name ORDER BY c.name LIMIT 3",
        ] {
            assert!(ensure_read_only(q).is_ok(), "should allow: {q}");
        }
    }

    #[test]
    fn refuses_every_write_clause_in_any_case() {
        for q in [
            "CREATE (n:X {a: 1})",
            "merge (c:Company {company_id: 'EVIL'})",
            "MATCH (c:Company) sEt c.name = 'pwned'",
            "MATCH (n) DELETE n",
            "MATCH (n) DETACH DELETE n",
            "MATCH (n) REMOVE n.email",
            "DROP INDEX company_id",
            "FOREACH (x IN [1] | CREATE (:Y))",
            "LOAD CSV FROM 'file:///x' AS row RETURN row",
            "CALL db.labels()",
        ] {
            assert_eq!(
                refused_kind(q),
                ErrorKind::Unauthorized,
                "should refuse: {q}"
            );
        }
    }

    #[test]
    fn keyword_inside_a_string_literal_is_data_not_a_clause() {
        for q in [
            "MATCH (c:Company) WHERE c.note = 'please MERGE this later' RETURN c.company_id",
            "MATCH (c:Company) WHERE c.note = \"SET for review\" RETURN c.company_id",
            "MATCH (c:Company) WHERE c.note = 'it\\'s DELETE season' RETURN c.company_id",
        ] {
            assert!(ensure_read_only(q).is_ok(), "literal is data: {q}");
        }
    }

    #[test]
    fn backticked_identifier_is_opaque_but_a_bare_one_fails_closed() {
        // The documented workaround for a keyword-shaped property name…
        assert!(ensure_read_only("MATCH (n:X) RETURN n.`set`").is_ok());
        // …because the bare spelling is refused: a false positive we keep,
        // fail-closed, rather than risk any false negative.
        assert_eq!(
            refused_kind("MATCH (n:X) RETURN n.set"),
            ErrorKind::Unauthorized
        );
    }

    #[test]
    fn similar_words_do_not_false_positive() {
        assert!(ensure_read_only(
            "MATCH (n:X) WHERE n.reset = true \
             RETURN n.settlement, n.created_at, n.merged_from, n.dropbox"
        )
        .is_ok());
    }

    #[test]
    fn unterminated_regions_are_unclassifiable_and_refused() {
        for q in [
            "MATCH (n) WHERE n.a = 'oops RETURN n",
            "MATCH (n) WHERE n.a = \"oops RETURN n",
            "MATCH (n) RETURN n.`oops",
        ] {
            assert_eq!(
                refused_kind(q),
                ErrorKind::Unauthorized,
                "should refuse: {q}"
            );
        }
    }

    #[test]
    fn any_comment_is_refused_outright() {
        // Comments are refused, not stripped — this moots keyword-splitting
        // tricks (`CRE/**/ATE`) regardless of the engine's lexer, and a
        // host-generated read query never carries a comment anyway.
        for q in [
            "MATCH (n:X) // trailing line comment\nRETURN n.company_id",
            "MATCH (n:X) /* block comment */ RETURN n.company_id",
            "MATCH (n:X) /* unclosed RETURN n",
            "CRE/**/ATE (n:X)",
        ] {
            assert_eq!(
                refused_kind(q),
                ErrorKind::Unauthorized,
                "should refuse: {q}"
            );
        }
        // A lone slash (division) is not a comment and stays allowed.
        assert!(ensure_read_only("MATCH (n:X) RETURN n.a / n.b").is_ok());
    }
}
