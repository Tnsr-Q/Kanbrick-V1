//! # kanbrick-loops
//!
//! The skill/loop ecosystem's domain model (L5 Cockpit, Phase 11).
//!
//! This first slice (P11.1, ADR-0012) defines the **skill manifest**: the
//! `SKILL.md` authoring format (a YAML-style frontmatter block plus a Markdown
//! body) and its parser. A skill is a *versioned, grant-gated wrapper over a WASM
//! guest* — the frontmatter names the backing `guest`, the minimum `clearance`, and
//! the `version`; the body is the human-facing instructions.
//!
//! This crate is deliberately **pure domain logic** — it depends only on
//! [`kanbrick_core`] for the [`ClearanceLevel`] vocabulary and carries no store,
//! HTTP, or async stack. That keeps it out of the SparrowDB dependency graph, so
//! the parser builds and tests standalone. Persistence of the parsed manifest as
//! `(:Skill)` + `(:SkillVersion)` graph nodes lives in `kanbrick-store`
//! (`skill_registry`); `kanbrick-api` composes the two (P11.2). The loop run-engine
//! and compiler land in later P11 slices (ADR-0013).

use kanbrick_core::ClearanceLevel;
use serde::{Deserialize, Serialize};

/// The `---` fence that delimits a `SKILL.md` frontmatter block.
const FENCE: &str = "---";

/// A parsed `SKILL.md` skill definition.
///
/// Mirrors the frontmatter keys (`name`, `version`, `guest`, `clearance`,
/// `description`) plus the Markdown `body`. A skill is backed by exactly one mesh
/// `guest`; the loop *step* (a later slice) is the polymorphic unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillManifest {
    /// Skill identity (the registry key, e.g. `"deal-modeling"`).
    pub name: String,
    /// Self-reported version (e.g. `"1.0.0"`).
    pub version: String,
    /// The mesh guest that backs this skill.
    pub guest: String,
    /// Minimum clearance required to invoke the skill.
    pub clearance: ClearanceLevel,
    /// One-line summary (optional in the source; empty if absent).
    pub description: String,
    /// The Markdown body: the skill's human-facing instructions.
    pub body: String,
}

impl SkillManifest {
    /// Render this manifest back to canonical `SKILL.md` text. The inverse of
    /// [`parse_skill_md`] up to frontmatter-key ordering and body whitespace
    /// normalization (so `parse(to_skill_md(m)) == m`).
    pub fn to_skill_md(&self) -> String {
        format!(
            "{FENCE}\nname: {}\nversion: {}\nguest: {}\nclearance: {}\ndescription: {}\n{FENCE}\n\n{}\n",
            self.name, self.version, self.guest, self.clearance, self.description, self.body
        )
    }
}

/// A failure parsing a `SKILL.md` document.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SkillParseError {
    /// The document does not open with a `---` frontmatter fence.
    #[error("SKILL.md is missing its `---` frontmatter block")]
    MissingFrontmatter,
    /// The opening fence is never closed by a second `---`.
    #[error("SKILL.md frontmatter is not terminated by a closing `---`")]
    UnterminatedFrontmatter,
    /// A frontmatter line is not in `key: value` form.
    #[error("SKILL.md frontmatter line {line} is not `key: value`: {content:?}")]
    BadLine {
        /// 1-based line number within the document.
        line: usize,
        /// The offending line's content.
        content: String,
    },
    /// A required frontmatter key is absent.
    #[error("SKILL.md frontmatter is missing required key `{0}`")]
    MissingKey(&'static str),
    /// A required frontmatter key is present but empty.
    #[error("SKILL.md frontmatter key `{0}` is empty")]
    EmptyValue(&'static str),
    /// The `clearance` value is not a valid `L1`..`L5`.
    #[error("SKILL.md has an invalid `clearance`: {0:?} (expected L1..=L5)")]
    BadClearance(String),
}

/// Parse a `SKILL.md` document into a [`SkillManifest`].
///
/// The document must open with a `---` fence (leading blank lines are tolerated),
/// a frontmatter block of `key: value` lines (blank lines and `#` comments
/// ignored, values may be single- or double-quoted), a closing `---` fence, and a
/// Markdown body. The keys `name`, `version`, `guest`, and `clearance` are
/// required and must be non-empty; `description` is optional.
pub fn parse_skill_md(input: &str) -> Result<SkillManifest, SkillParseError> {
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    let lines: Vec<&str> = input.lines().collect();

    // The opening fence is the first non-blank line and must be exactly `---`.
    let mut open = 0;
    while open < lines.len() && lines[open].trim().is_empty() {
        open += 1;
    }
    if open >= lines.len() || lines[open].trim() != FENCE {
        return Err(SkillParseError::MissingFrontmatter);
    }
    let body_start = lines[open + 1..]
        .iter()
        .position(|l| l.trim() == FENCE)
        .map(|rel| open + 1 + rel)
        .ok_or(SkillParseError::UnterminatedFrontmatter)?;

    // Parse the frontmatter `key: value` lines.
    let mut fields: Vec<(String, String)> = Vec::new();
    for (offset, line) in lines[open + 1..body_start].iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| SkillParseError::BadLine {
                line: open + 1 + offset + 1,
                content: (*line).to_string(),
            })?;
        fields.push((
            key.trim().to_ascii_lowercase(),
            unquote(value.trim()).to_string(),
        ));
    }

    let require = |key: &'static str| -> Result<String, SkillParseError> {
        let value = field(&fields, key).ok_or(SkillParseError::MissingKey(key))?;
        if value.is_empty() {
            return Err(SkillParseError::EmptyValue(key));
        }
        Ok(value.to_string())
    };

    let name = require("name")?;
    let version = require("version")?;
    let guest = require("guest")?;
    let clearance_raw = require("clearance")?;
    let clearance = clearance_raw
        .parse::<ClearanceLevel>()
        .map_err(|_| SkillParseError::BadClearance(clearance_raw))?;
    let description = field(&fields, "description").unwrap_or("").to_string();

    let body = lines[body_start + 1..].join("\n").trim().to_string();

    Ok(SkillManifest {
        name,
        version,
        guest,
        clearance,
        description,
        body,
    })
}

/// The value of frontmatter `key` (last wins on a duplicate), if present.
fn field<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    fields
        .iter()
        .rev()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Strip a single matching pair of surrounding single or double quotes.
fn unquote(s: &str) -> &str {
    let bytes = s.as_bytes();
    let n = bytes.len();
    if n >= 2 && (bytes[0] == b'"' || bytes[0] == b'\'') && bytes[n - 1] == bytes[0] {
        &s[1..n - 1]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "---\n\
        name: deal-modeling\n\
        version: 1.2.0\n\
        guest: valuation\n\
        clearance: L3\n\
        description: Model a deal from financials\n\
        ---\n\
        \n\
        # Deal modeling\n\
        \n\
        Run the valuation guest over a company's financials.\n";

    #[test]
    fn parses_a_valid_skill_md() {
        let m = parse_skill_md(VALID).unwrap();
        assert_eq!(m.name, "deal-modeling");
        assert_eq!(m.version, "1.2.0");
        assert_eq!(m.guest, "valuation");
        assert_eq!(m.clearance, ClearanceLevel::L3);
        assert_eq!(m.description, "Model a deal from financials");
        assert!(m.body.starts_with("# Deal modeling"));
        assert!(m.body.ends_with("financials."));
    }

    #[test]
    fn round_trips_through_to_skill_md() {
        let m = parse_skill_md(VALID).unwrap();
        assert_eq!(parse_skill_md(&m.to_skill_md()).unwrap(), m);
    }

    #[test]
    fn frontmatter_keys_and_clearance_are_case_insensitive() {
        let src = "---\nNAME: x\nVersion: 0.1.0\nGUEST: g\nclearance: l4\n---\nbody";
        let m = parse_skill_md(src).unwrap();
        assert_eq!(m.name, "x");
        assert_eq!(m.clearance, ClearanceLevel::L4);
    }

    #[test]
    fn quoted_values_are_unquoted() {
        let src = "---\nname: \"quoted name\"\nversion: '0.1.0'\nguest: g\nclearance: L1\n---\n";
        let m = parse_skill_md(src).unwrap();
        assert_eq!(m.name, "quoted name");
        assert_eq!(m.version, "0.1.0");
    }

    #[test]
    fn leading_blank_lines_and_comments_are_tolerated() {
        let src = "\n\n---\n# a comment\nname: x\nversion: 1\nguest: g\nclearance: L2\n---\nbody";
        let m = parse_skill_md(src).unwrap();
        assert_eq!(m.name, "x");
        assert_eq!(m.clearance, ClearanceLevel::L2);
    }

    #[test]
    fn missing_frontmatter_is_an_error() {
        assert_eq!(
            parse_skill_md("no fence here\njust text").unwrap_err(),
            SkillParseError::MissingFrontmatter
        );
    }

    #[test]
    fn unterminated_frontmatter_is_an_error() {
        assert_eq!(
            parse_skill_md("---\nname: x\nversion: 1\nguest: g\nclearance: L1\n").unwrap_err(),
            SkillParseError::UnterminatedFrontmatter
        );
    }

    #[test]
    fn a_missing_required_key_is_reported() {
        let src = "---\nname: x\nversion: 1\nclearance: L1\n---\n"; // no guest
        assert_eq!(
            parse_skill_md(src).unwrap_err(),
            SkillParseError::MissingKey("guest")
        );
    }

    #[test]
    fn an_empty_required_value_is_reported() {
        let src = "---\nname:   \nversion: 1\nguest: g\nclearance: L1\n---\n";
        assert_eq!(
            parse_skill_md(src).unwrap_err(),
            SkillParseError::EmptyValue("name")
        );
    }

    #[test]
    fn a_bad_clearance_is_reported() {
        let src = "---\nname: x\nversion: 1\nguest: g\nclearance: L9\n---\n";
        assert_eq!(
            parse_skill_md(src).unwrap_err(),
            SkillParseError::BadClearance("L9".to_string())
        );
    }

    #[test]
    fn description_is_optional() {
        let src = "---\nname: x\nversion: 1\nguest: g\nclearance: L1\n---\nbody";
        assert_eq!(parse_skill_md(src).unwrap().description, "");
    }

    #[test]
    fn an_empty_body_parses_as_empty() {
        let src = "---\nname: x\nversion: 1\nguest: g\nclearance: L1\n---\n";
        assert_eq!(parse_skill_md(src).unwrap().body, "");
    }

    #[test]
    fn a_value_keeps_text_after_the_first_colon() {
        let src = "---\nname: x\nversion: 1\nguest: g\nclearance: L1\ndescription: a: b\n---\n";
        assert_eq!(parse_skill_md(src).unwrap().description, "a: b");
    }

    #[test]
    fn crlf_line_endings_parse() {
        let src = "---\r\nname: x\r\nversion: 1\r\nguest: g\r\nclearance: L2\r\n---\r\nbody\r\n";
        let m = parse_skill_md(src).unwrap();
        assert_eq!(m.name, "x");
        assert_eq!(m.clearance, ClearanceLevel::L2);
        assert_eq!(m.body, "body");
    }

    #[test]
    fn a_duplicate_key_takes_the_last_value() {
        let src = "---\nname: first\nname: second\nversion: 1\nguest: g\nclearance: L1\n---\n";
        assert_eq!(parse_skill_md(src).unwrap().name, "second");
    }
}
