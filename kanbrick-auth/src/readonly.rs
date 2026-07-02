//! Read-only classifier for the guest graph channel (P16.1, ADR-0022).
//!
//! [`ensure_read_only`] refuses any Cypher statement that could mutate the
//! graph before it reaches the store. The guest `query_graph` host import is a
//! *read* channel by contract (README / docs/SECURITY.md); the host is the
//! graph's only writer.
//!
//! This is a scanner in front of the engine, so its soundness rests on its
//! tokenization agreeing with SparrowDB's lexer (see ADR-0022 "Residual risk").
//! Every rule below is chosen to eliminate a way the two could *disagree*, and
//! to fail closed when they might:
//!
//! * a **backslash** anywhere is refused. The only place `\` matters is string
//!   escaping, and whether an engine honors `\'` is exactly the ambiguity that
//!   lets a crafted literal end in one lexer and not the other (a write clause
//!   hidden inside what the scanner thinks is still a string). With no
//!   backslash present, `'…'` is simply the text between matching quotes for
//!   *any* lexer, so scanner and engine cannot desync. A read query never needs
//!   one;
//! * a **comment** (`//` or `/* … */`) is refused outright, not stripped —
//!   removing any dependence on whether the engine treats a keyword-splitting
//!   comment (`CRE/**/ATE`) as a separator or elides it;
//! * the statement must **begin with a read-only opening clause** (an
//!   allowlist: `MATCH` / `OPTIONAL` / `WITH` / `UNWIND` / `RETURN`). A verb the
//!   engine might support but this module has never heard of therefore fails
//!   closed as a leading clause, rather than slipping past the write denylist;
//! * write-clause keywords are then matched as whole word tokens outside string
//!   literals and backtick identifiers — a keyword carried as data (`'MERGE'`)
//!   passes, a keyword-shaped bare property (`n.set`) is refused (a false
//!   positive we accept over any false negative; backtick it to read it);
//! * an unterminated string literal is unclassifiable and refused too.

use kanbrick_core::{Error, Result};

/// Cypher write/DDL vocabulary. The pinned SparrowDB dialect (ADR-0001) only
/// executes a subset of these today (`CREATE`/`MERGE`/`SET`/`DELETE`); the rest
/// are refused anyway so the gate stays closed if the engine grows support.
/// This is a denylist and so cannot be exhaustive by itself — the leading-clause
/// allowlist ([`READ_OPENERS`]) is what closes an *unknown* write verb.
const WRITE_KEYWORDS: &[&str] = &[
    "CREATE", "MERGE", "SET", "DELETE", "DETACH", "REMOVE", "DROP", "FOREACH", "LOAD", "CALL",
];

/// The only clause keywords a read-only statement may **begin** with. Anything
/// else is refused before the denylist scan, so a write verb this module does
/// not know cannot lead a statement. (`CALL` is deliberately absent — a bare
/// procedure call is on the write denylist.)
const READ_OPENERS: &[&str] = &["MATCH", "OPTIONAL", "WITH", "UNWIND", "RETURN"];

/// Refuse `cypher` unless it is provably free of write clauses.
pub(crate) fn ensure_read_only(cypher: &str) -> Result<()> {
    // A backslash only matters as a string escape, which is the one lexical
    // rule most likely to differ between this scanner and the engine — refuse
    // it so string boundaries are unambiguous for both (see the module doc).
    if cypher.contains('\\') {
        return Err(refused("backslash"));
    }
    let scannable = strip_opaque_regions(cypher)?;

    let mut opener_checked = false;
    let mut word = String::new();
    for ch in scannable.chars().chain(std::iter::once(' ')) {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            word.push(ch.to_ascii_uppercase());
        } else if !word.is_empty() {
            if !opener_checked {
                if !READ_OPENERS.contains(&word.as_str()) {
                    return Err(refused(&format!(
                        "statement must open with a read clause, not {word}"
                    )));
                }
                opener_checked = true;
            }
            if WRITE_KEYWORDS.contains(&word.as_str()) {
                return Err(refused(&format!("write clause {word}")));
            }
            word.clear();
        }
    }
    if !opener_checked {
        // No clause keyword at all (empty / whitespace / only punctuation).
        return Err(refused("no read clause"));
    }
    Ok(())
}

fn refused(what: &str) -> Error {
    Error::Auth(format!("read-only guest graph channel: {what} refused"))
}

/// Blank out string literals (`'…'` / `"…"`) and backtick identifiers so the
/// keyword scan only sees executable clause positions. Backslashes are already
/// refused by [`ensure_read_only`], so a literal is unambiguously the text
/// between matching quotes — no escape handling, no scanner/engine desync. A
/// comment marker (`//` or `/*`) is **refused**, not stripped. An unterminated
/// quoted region is refused too — a statement we cannot classify never runs
/// (fail closed).
fn strip_opaque_regions(cypher: &str) -> Result<String> {
    let mut out = String::with_capacity(cypher.len());
    let mut chars = cypher.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' | '"' | '`' => {
                let quote = ch;
                if !chars.by_ref().any(|c| c == quote) {
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
        ] {
            assert!(ensure_read_only(q).is_ok(), "literal is data: {q}");
        }
    }

    #[test]
    fn a_leading_verb_that_is_not_a_read_clause_fails_closed() {
        // The denylist can never be exhaustive; the leading-clause allowlist is
        // what refuses a write verb this module has never heard of, rather than
        // letting it open a statement (ADR-0022 finding 3).
        for q in [
            "INSERT (n:X)",
            "UPSERT (n:X)",
            "GRANT read TO n",
            "  \n  ", // no clause at all → refused
            "42 RETURN n",
        ] {
            assert_eq!(
                refused_kind(q),
                ErrorKind::Unauthorized,
                "should refuse non-read opener: {q}"
            );
        }
        // Every legitimate read opener is accepted.
        for q in [
            "MATCH (n) RETURN n.email",
            "OPTIONAL MATCH (n) RETURN n.email",
            "WITH 1 AS x RETURN x",
            "UNWIND [1, 2] AS n RETURN n",
            "RETURN 1",
        ] {
            assert!(ensure_read_only(q).is_ok(), "read opener allowed: {q}");
        }
    }

    #[test]
    fn a_backslash_anywhere_is_refused() {
        // A backslash only matters as a string escape — the one lexical rule
        // most likely to differ between this scanner and the engine, which is
        // exactly the desync a crafted `'x\' DELETE n //'` would exploit
        // (ADR-0022 finding 1). Refusing it makes string boundaries unambiguous.
        for q in [
            r"MATCH (n) WHERE n.a = 'x\' DELETE n //'",
            r"MATCH (n) WHERE n.a = 'c:\temp' RETURN n",
            r"MATCH (n) RETURN n.\`weird\`",
        ] {
            assert_eq!(
                refused_kind(q),
                ErrorKind::Unauthorized,
                "should refuse backslash: {q}"
            );
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
