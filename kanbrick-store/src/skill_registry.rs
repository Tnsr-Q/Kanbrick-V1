//! Versioned skill registry (P11.1, ADR-0012).
//!
//! Persists a published skill as two node kinds: `(:Skill {name})` is the stable
//! identity (one node per skill name), and `(:SkillVersion {version_id, …})` is one
//! node per published edition, linked by `(:Skill)-[:HAS_VERSION]->(:SkillVersion)`.
//! This is the *catalogue* of publishable, versioned skill definitions; it confers
//! no access — `ScopeGrants` (ADR-0007) remains the sole authorization gate, wired
//! to this registry in P11.2.
//!
//! ## Publish trust gate (P11.8, ADR-0021)
//!
//! Each `(:SkillVersion)` carries a `review_status` (`pending`|`approved`|`rejected`,
//! reset to `pending` on every publish) plus `reviewed_by`/`reviewed_at`, and each
//! `(:Skill)` carries an `owner` (its first publisher). These are the substrate for
//! the trust gate: an eligible lead must **approve** an edition before it is bindable
//! by anyone other than its author ([`set_skill_review`], [`pending_skill_versions`]),
//! and only the owner (or an L5) may publish further editions of a name
//! ([`skill_owner`], enforced in the API). A missing/`None` `review_status` is treated
//! as `pending` (fail-closed). The gate enforcement itself lives in `kanbrick-api`.
//!
//! Writes follow the ADR-0001 dialect: a parameterized `MERGE` on the unique key,
//! then `MATCH … SET` for the mutable fields, so re-publishing a version **updates
//! in place** rather than duplicating. `min_clearance` is stored as its `Display`
//! form (`"L3"`), the exact `serde` wire form of [`ClearanceLevel`], so it
//! round-trips on read (the same pattern as [`crate::guest_policy`]). Ordering is by
//! an append-only `seq` (the node count at write time) sorted on read — strictly
//! increasing across publishes, so "latest" is the highest `seq`; a re-publish moves
//! that version to the most-recent position, which is the intended publish-recency
//! order. The `version_id` key is `"{name}@{version}"` (skill names are kebab,
//! versions semver — neither contains `@`).

use std::collections::HashMap;

use kanbrick_core::{ClearanceLevel, Result};
use serde::{Deserialize, Serialize};

use crate::store::Store;
use crate::value::Params;

/// Columns projected by the read queries, in order. Un-aliased bare-node projection
/// per ADR-0001 (the row mapper strips the `v.` prefix); aliasing would yield nulls.
const PROJECTION: &str = "RETURN v.skill_name, v.version, v.guest, v.min_clearance, \
     v.description, v.source, v.created_at, v.seq, \
     v.review_status, v.reviewed_by, v.reviewed_at";

/// A published edition of a skill — one `(:SkillVersion)` node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillVersionRecord {
    /// The skill identity this edition belongs to (the `(:Skill)` key).
    pub skill_name: String,
    /// This edition's version (e.g. `"1.2.0"`).
    pub version: String,
    /// The mesh guest that backs the skill.
    pub guest: String,
    /// Minimum clearance required to invoke the skill.
    pub min_clearance: ClearanceLevel,
    /// One-line summary (may be empty).
    pub description: String,
    /// Provenance: who/what published this edition (e.g. an author email).
    pub source: String,
    /// RFC 3339 timestamp of when this edition was last published.
    pub created_at: String,
    /// Append-only publish-order key, assigned by [`publish_skill_version`]
    /// (the value supplied on a freshly-built record is ignored on write).
    pub seq: i64,
    /// Publish trust-gate state (P11.8, ADR-0021): `"pending"` | `"approved"` |
    /// `"rejected"`. `None` (a pre-P11.8 edition with no recorded state, or an absent
    /// column) is treated as **pending** — fail-closed, so an unreviewed edition is
    /// never bindable by others. Set to `"pending"` on every (re-)publish.
    #[serde(default)]
    pub review_status: Option<String>,
    /// The eligible lead who approved/rejected this edition; `None`/empty until decided.
    #[serde(default)]
    pub reviewed_by: Option<String>,
    /// RFC 3339 timestamp of the review decision; `None`/empty until decided.
    #[serde(default)]
    pub reviewed_at: Option<String>,
}

impl SkillVersionRecord {
    /// Build a record, stamping `created_at` with the current time. `seq` is a
    /// placeholder (`0`); [`publish_skill_version`] assigns the real value at write
    /// time, and reads populate it from the store.
    pub fn new(
        skill_name: impl Into<String>,
        version: impl Into<String>,
        guest: impl Into<String>,
        min_clearance: ClearanceLevel,
        description: impl Into<String>,
        source: impl Into<String>,
    ) -> Self {
        SkillVersionRecord {
            skill_name: skill_name.into(),
            version: version.into(),
            guest: guest.into(),
            min_clearance,
            description: description.into(),
            source: source.into(),
            created_at: chrono::Utc::now().to_rfc3339(),
            seq: 0,
            // A freshly authored edition starts unreviewed (pending); the gate keeps
            // it un-bindable by others until an eligible lead approves it (ADR-0021).
            review_status: Some(REVIEW_PENDING.to_string()),
            reviewed_by: None,
            reviewed_at: None,
        }
    }
}

/// Review states for a published edition (P11.8, ADR-0021).
pub const REVIEW_PENDING: &str = "pending";
/// An edition an eligible lead has approved — bindable by others.
pub const REVIEW_APPROVED: &str = "approved";
/// An edition an eligible lead has rejected.
pub const REVIEW_REJECTED: &str = "rejected";

/// The unique node key for a skill edition: `"{name}@{version}"`.
fn version_id(skill_name: &str, version: &str) -> String {
    format!("{skill_name}@{version}")
}

/// Publish a skill edition: MERGE the `(:Skill)` identity, MERGE the
/// `(:SkillVersion)` on its `version_id`, SET its fields, and link them. Re-publishing
/// the same `(name, version)` updates the edition in place (no duplicate node).
pub fn publish_skill_version(store: &Store, record: &SkillVersionRecord) -> Result<()> {
    let version_id = version_id(&record.skill_name, &record.version);
    // The current edition count is the next append-only publish index.
    let seq = count_skill_versions(store)?;
    // Establish or preserve the skill name's **owner** (P11.8, ADR-0021): the first
    // publisher owns the name; subsequent publishes keep that owner (so a later
    // publisher can never silently claim it). The publish-time author-pin check lives
    // in the API (`publish_skill`), which rejects a non-owner before reaching here;
    // this just records/keeps the owner so it is queryable for that check.
    let owner = skill_owner(store, &record.skill_name)?.unwrap_or_else(|| record.source.clone());

    store.execute_with(
        "MERGE (s:Skill {name: $name})",
        Params::new().with("name", record.skill_name.as_str()),
    )?;
    store.execute_with(
        "MATCH (s:Skill {name: $name}) SET s.owner = $owner",
        Params::new()
            .with("name", record.skill_name.as_str())
            .with("owner", owner.as_str()),
    )?;
    store.execute_with(
        "MERGE (v:SkillVersion {version_id: $version_id})",
        Params::new().with("version_id", version_id.as_str()),
    )?;
    store.execute_with(
        "MATCH (v:SkillVersion {version_id: $version_id}) \
         SET v.skill_name = $skill_name, v.version = $version, v.guest = $guest, \
             v.min_clearance = $min_clearance, v.description = $description, \
             v.source = $source, v.created_at = $created_at, v.seq = $seq, \
             v.review_status = $review_status, v.reviewed_by = $reviewed_by, \
             v.reviewed_at = $reviewed_at",
        Params::new()
            .with("version_id", version_id.as_str())
            .with("skill_name", record.skill_name.as_str())
            .with("version", record.version.as_str())
            .with("guest", record.guest.as_str())
            .with("min_clearance", record.min_clearance.to_string())
            .with("description", record.description.as_str())
            .with("source", record.source.as_str())
            .with("created_at", record.created_at.as_str())
            .with("seq", seq)
            // Every (re-)publish resets the edition to pending and clears any prior
            // decision — re-published content must be re-reviewed (fail-closed).
            .with("review_status", REVIEW_PENDING)
            .with("reviewed_by", "")
            .with("reviewed_at", ""),
    )?;
    // Link identity → edition (graph fidelity; editions are queried by the FK
    // property, so this edge is provenance only). The relationship MERGE must use
    // the **non-parameterized** path: the pinned SparrowDB rejects a relationship
    // MERGE on the parameterized surface (ADR-0006 / SPA-233), so the two keys are
    // inlined with single-quote escaping — exactly as the `(:ProjectScope)`
    // -[:HAS_SKILL]->`(:Skill)` and code-graph edge writes do.
    store.execute(&format!(
        "MATCH (s:Skill {{name: '{}'}}), (v:SkillVersion {{version_id: '{}'}}) \
         MERGE (s)-[:HAS_VERSION]->(v)",
        escape(&record.skill_name),
        escape(&version_id),
    ))?;
    Ok(())
}

/// Escape a string for inline use in a Cypher single-quoted literal (the
/// relationship MERGE above must use the non-parameterized path). Mirrors
/// `codegraph::escape_id`.
fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Every published edition of `skill_name`, oldest→newest (by publish order).
pub fn list_skill_versions(store: &Store, skill_name: &str) -> Result<Vec<SkillVersionRecord>> {
    let mut rows = store.query::<SkillVersionRecord>(
        &format!("MATCH (v:SkillVersion {{skill_name: $name}}) {PROJECTION}"),
        Params::new().with("name", skill_name),
    )?;
    rows.sort_by_key(|r| r.seq);
    Ok(rows)
}

/// The most recently published edition of `skill_name`, or `None` if unknown.
pub fn latest_skill_version(store: &Store, skill_name: &str) -> Result<Option<SkillVersionRecord>> {
    Ok(list_skill_versions(store, skill_name)?.pop())
}

/// The latest edition of every skill, one row per skill name, sorted by name.
pub fn list_skills(store: &Store) -> Result<Vec<SkillVersionRecord>> {
    let mut all = store.query::<SkillVersionRecord>(
        &format!("MATCH (v:SkillVersion) {PROJECTION}"),
        Params::new(),
    )?;
    all.sort_by_key(|r| r.seq);
    // Higher `seq` overwrites, so each name keeps its most recent edition.
    let mut latest: HashMap<String, SkillVersionRecord> = HashMap::new();
    for record in all {
        latest.insert(record.skill_name.clone(), record);
    }
    let mut out: Vec<SkillVersionRecord> = latest.into_values().collect();
    out.sort_by(|a, b| a.skill_name.cmp(&b.skill_name));
    Ok(out)
}

/// Count published skill editions. Used to assign the next `seq` and as a
/// test/inspection helper.
pub fn count_skill_versions(store: &Store) -> Result<i64> {
    Ok(store
        .scalar_i64("MATCH (v:SkillVersion) RETURN count(v)", Params::new())?
        .unwrap_or(0))
}

/// The email that owns a skill **name** — its first publisher (P11.8, ADR-0021).
/// `None` if the name was never published (or, for a pre-P11.8 node, has no recorded
/// owner — the next publish records one). Author-pinning: the API lets only this owner
/// (or an L5 cofounder) publish further editions of the name, closing the cross-author
/// overwrite gap. Tolerates an absent/`null` column (→ `None`) for back-compat.
pub fn skill_owner(store: &Store, skill_name: &str) -> Result<Option<String>> {
    #[derive(Deserialize)]
    struct OwnerRow {
        #[serde(default)]
        owner: Option<String>,
    }
    let rows: Vec<OwnerRow> = store.query(
        "MATCH (s:Skill {name: $name}) RETURN s.owner",
        Params::new().with("name", skill_name),
    )?;
    Ok(rows
        .into_iter()
        .next()
        .and_then(|r| r.owner)
        .filter(|o| !o.is_empty()))
}

/// Record a review decision on an edition (P11.8, ADR-0021): set its `review_status`
/// (`"approved"`/`"rejected"`) plus the reviewing lead + RFC 3339 timestamp. The SET is
/// a no-op when the edition does not exist; the API resolves + `404`s the edition first.
pub fn set_skill_review(
    store: &Store,
    skill_name: &str,
    version: &str,
    status: &str,
    reviewer: &str,
    reviewed_at: &str,
) -> Result<()> {
    let vid = version_id(skill_name, version);
    store.execute_with(
        "MATCH (v:SkillVersion {version_id: $version_id}) \
         SET v.review_status = $status, v.reviewed_by = $reviewer, v.reviewed_at = $at",
        Params::new()
            .with("version_id", vid.as_str())
            .with("status", status)
            .with("reviewer", reviewer)
            .with("at", reviewed_at),
    )?;
    Ok(())
}

/// Every published edition still awaiting review (P11.8) — the reviewer queue,
/// oldest→newest by publish order. A missing/`None` status counts as **pending**
/// (fail-closed), so pre-P11.8 editions surface for review too.
pub fn pending_skill_versions(store: &Store) -> Result<Vec<SkillVersionRecord>> {
    let mut all = store.query::<SkillVersionRecord>(
        &format!("MATCH (v:SkillVersion) {PROJECTION}"),
        Params::new(),
    )?;
    all.retain(|r| r.review_status.as_deref().unwrap_or(REVIEW_PENDING) == REVIEW_PENDING);
    all.sort_by_key(|r| r.seq);
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    fn record(name: &str, version: &str, clearance: ClearanceLevel) -> SkillVersionRecord {
        SkillVersionRecord::new(
            name,
            version,
            "valuation",
            clearance,
            "desc",
            "elena@kanbrick.com",
        )
    }

    #[test]
    fn publish_then_read_round_trips() {
        let (_d, store) = store();
        assert_eq!(count_skill_versions(&store).unwrap(), 0);
        publish_skill_version(
            &store,
            &record("deal-modeling", "1.0.0", ClearanceLevel::L3),
        )
        .unwrap();

        let latest = latest_skill_version(&store, "deal-modeling")
            .unwrap()
            .unwrap();
        assert_eq!(latest.skill_name, "deal-modeling");
        assert_eq!(latest.version, "1.0.0");
        assert_eq!(latest.guest, "valuation");
        assert_eq!(latest.min_clearance, ClearanceLevel::L3);
        assert_eq!(latest.source, "elena@kanbrick.com");
        assert_eq!(count_skill_versions(&store).unwrap(), 1);
    }

    #[test]
    fn multiple_versions_order_by_publish_and_latest_wins() {
        let (_d, store) = store();
        publish_skill_version(
            &store,
            &record("deal-modeling", "1.0.0", ClearanceLevel::L3),
        )
        .unwrap();
        publish_skill_version(
            &store,
            &record("deal-modeling", "1.1.0", ClearanceLevel::L4),
        )
        .unwrap();

        let versions = list_skill_versions(&store, "deal-modeling").unwrap();
        let tags: Vec<&str> = versions.iter().map(|v| v.version.as_str()).collect();
        assert_eq!(tags, ["1.0.0", "1.1.0"], "oldest→newest by publish order");

        let latest = latest_skill_version(&store, "deal-modeling")
            .unwrap()
            .unwrap();
        assert_eq!(latest.version, "1.1.0");
        assert_eq!(latest.min_clearance, ClearanceLevel::L4);
    }

    #[test]
    fn re_publishing_a_version_updates_in_place() {
        let (_d, store) = store();
        publish_skill_version(
            &store,
            &record("deal-modeling", "1.0.0", ClearanceLevel::L3),
        )
        .unwrap();
        // Re-publish the same (name, version) with a raised clearance.
        publish_skill_version(
            &store,
            &record("deal-modeling", "1.0.0", ClearanceLevel::L5),
        )
        .unwrap();

        let versions = list_skill_versions(&store, "deal-modeling").unwrap();
        assert_eq!(versions.len(), 1, "re-publish updates, not duplicates");
        assert_eq!(
            versions[0].min_clearance,
            ClearanceLevel::L5,
            "fields updated"
        );
    }

    #[test]
    fn re_publishing_moves_an_edition_to_most_recent() {
        let (_d, store) = store();
        publish_skill_version(
            &store,
            &record("deal-modeling", "1.0.0", ClearanceLevel::L3),
        )
        .unwrap();
        publish_skill_version(
            &store,
            &record("deal-modeling", "1.1.0", ClearanceLevel::L4),
        )
        .unwrap();
        // Re-publishing the older edition makes it the most recently published.
        publish_skill_version(
            &store,
            &record("deal-modeling", "1.0.0", ClearanceLevel::L5),
        )
        .unwrap();

        let versions = list_skill_versions(&store, "deal-modeling").unwrap();
        let tags: Vec<&str> = versions.iter().map(|v| v.version.as_str()).collect();
        assert_eq!(
            tags,
            ["1.1.0", "1.0.0"],
            "a re-publish moves its edition to most-recent"
        );
        assert_eq!(versions.len(), 2, "still two distinct editions, not three");
        assert_eq!(
            latest_skill_version(&store, "deal-modeling")
                .unwrap()
                .unwrap()
                .version,
            "1.0.0",
            "latest = most recently published"
        );
    }

    #[test]
    fn list_skills_returns_latest_per_skill_sorted_by_name() {
        let (_d, store) = store();
        publish_skill_version(
            &store,
            &record("deal-modeling", "1.0.0", ClearanceLevel::L3),
        )
        .unwrap();
        publish_skill_version(
            &store,
            &record("deal-modeling", "2.0.0", ClearanceLevel::L4),
        )
        .unwrap();
        publish_skill_version(&store, &record("audit-prep", "0.1.0", ClearanceLevel::L2)).unwrap();

        let skills = list_skills(&store).unwrap();
        let names: Vec<&str> = skills.iter().map(|s| s.skill_name.as_str()).collect();
        assert_eq!(
            names,
            ["audit-prep", "deal-modeling"],
            "one row per skill, sorted"
        );
        let deal = skills
            .iter()
            .find(|s| s.skill_name == "deal-modeling")
            .unwrap();
        assert_eq!(
            deal.version, "2.0.0",
            "the latest edition represents the skill"
        );
    }

    #[test]
    fn registry_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = Store::open(dir.path()).unwrap();
            publish_skill_version(
                &store,
                &record("deal-modeling", "1.0.0", ClearanceLevel::L3),
            )
            .unwrap();
            store.checkpoint().unwrap();
        }
        let store = Store::open(dir.path()).unwrap();
        let latest = latest_skill_version(&store, "deal-modeling")
            .unwrap()
            .unwrap();
        assert_eq!(
            latest.version, "1.0.0",
            "registry survives a process restart"
        );
    }

    #[test]
    fn unknown_skill_has_no_versions() {
        let (_d, store) = store();
        assert!(list_skill_versions(&store, "ghost").unwrap().is_empty());
        assert!(latest_skill_version(&store, "ghost").unwrap().is_none());
    }

    // ── P11.8 trust gate (ADR-0021) ──────────────────────────────────────────

    fn record_by(name: &str, version: &str, source: &str) -> SkillVersionRecord {
        SkillVersionRecord::new(name, version, "valuation", ClearanceLevel::L3, "d", source)
    }

    #[test]
    fn publish_records_owner_and_starts_pending() {
        let (_d, store) = store();
        publish_skill_version(
            &store,
            &record_by("deal-modeling", "1.0.0", "elena@kanbrick.com"),
        )
        .unwrap();
        assert_eq!(
            skill_owner(&store, "deal-modeling").unwrap().as_deref(),
            Some("elena@kanbrick.com"),
            "the first publisher owns the name"
        );
        let latest = latest_skill_version(&store, "deal-modeling")
            .unwrap()
            .unwrap();
        assert_eq!(
            latest.review_status.as_deref(),
            Some(REVIEW_PENDING),
            "a freshly published edition is pending"
        );
        assert!(skill_owner(&store, "ghost").unwrap().is_none());
    }

    #[test]
    fn owner_is_preserved_across_a_republish_by_a_different_source() {
        let (_d, store) = store();
        publish_skill_version(
            &store,
            &record_by("deal-modeling", "1.0.0", "elena@kanbrick.com"),
        )
        .unwrap();
        // A later edition with a different `source` must NOT change the owner (the API
        // forbids a non-owner publish, but the store also keeps the owner stable).
        publish_skill_version(
            &store,
            &record_by("deal-modeling", "2.0.0", "mallory@kanbrick.com"),
        )
        .unwrap();
        assert_eq!(
            skill_owner(&store, "deal-modeling").unwrap().as_deref(),
            Some("elena@kanbrick.com"),
            "owner stays the first publisher"
        );
    }

    #[test]
    fn review_decision_round_trips_and_a_republish_resets_it() {
        let (_d, store) = store();
        publish_skill_version(
            &store,
            &record_by("deal-modeling", "1.0.0", "elena@kanbrick.com"),
        )
        .unwrap();
        set_skill_review(
            &store,
            "deal-modeling",
            "1.0.0",
            REVIEW_APPROVED,
            "peter@kanbrick.com",
            "2026-06-29T00:00:00+00:00",
        )
        .unwrap();
        let approved = latest_skill_version(&store, "deal-modeling")
            .unwrap()
            .unwrap();
        assert_eq!(approved.review_status.as_deref(), Some(REVIEW_APPROVED));
        assert_eq!(approved.reviewed_by.as_deref(), Some("peter@kanbrick.com"));

        // Re-publishing the same edition resets it to pending and clears the decision.
        publish_skill_version(
            &store,
            &record_by("deal-modeling", "1.0.0", "elena@kanbrick.com"),
        )
        .unwrap();
        let reset = latest_skill_version(&store, "deal-modeling")
            .unwrap()
            .unwrap();
        assert_eq!(
            reset.review_status.as_deref(),
            Some(REVIEW_PENDING),
            "a re-publish must be re-reviewed"
        );
        assert_eq!(
            reset.reviewed_by.as_deref(),
            Some(""),
            "prior reviewer cleared"
        );
    }

    #[test]
    fn pending_queue_lists_only_unreviewed_editions() {
        let (_d, store) = store();
        publish_skill_version(&store, &record_by("a", "1.0.0", "elena@kanbrick.com")).unwrap();
        publish_skill_version(&store, &record_by("b", "1.0.0", "elena@kanbrick.com")).unwrap();
        // Approve `a`; `b` stays pending.
        set_skill_review(
            &store,
            "a",
            "1.0.0",
            REVIEW_APPROVED,
            "peter@kanbrick.com",
            "t",
        )
        .unwrap();
        let pending = pending_skill_versions(&store).unwrap();
        let names: Vec<&str> = pending.iter().map(|r| r.skill_name.as_str()).collect();
        assert_eq!(names, ["b"], "only the unreviewed edition is queued");
    }
}
