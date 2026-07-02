//! Project-scope grant lifecycle (#57) — employee-requestable, additive access
//! grants and per-project skills, persisted in SparrowDB.
//!
//! [`ProjectScope`](crate::scope::ProjectScope) is the *enforcement* primitive
//! (additive, composable [`VisibilityScope`](crate::scope::VisibilityScope)).
//! This module is the *lifecycle* around it: an employee **requests** extra
//! project-scoped visibility, an eligible **grantor** approves or denies, an
//! approval **persists** a project scope, and discovery/skills then run under the
//! employee's base clearance **composed with** the granted scope. Grants expire
//! or are revoked; the whole chain is audited.
//!
//! ## Design decisions (operator-directed; ADR-0007)
//!
//! * **SparrowDB is the source of truth.** Scopes are business state (they change
//!   daily, must be queryable and revocable at runtime), not configuration —
//!   persisted as `(:ScopeRequest)`, `(:ProjectScope)`, `(:Skill)` nodes with
//!   `(:ProjectScope)-[:HAS_SKILL]->(:Skill)` edges.
//! * **Dual-gate grantor.** A grantor must hold clearance ≥ L4 **and** be in the
//!   requester's management chain — unless they are an L5 cofounder (firm-wide
//!   override). See [`ScopeGrants::eligible_grantor`].
//! * **Additive only.** A grant unions extra `granted_persons`/`granted_companies`
//!   onto the base scope; it never reduces the base and never yields "sees all"
//!   (guaranteed by `ProjectScope`).
//! * **Expiry + revocation.** Scopes carry an `expires_at`; [`expire_due`] sweeps
//!   past-due ACTIVE scopes to EXPIRED, [`revoke`] terminates one immediately and
//!   cascades its granted request to EXPIRED. Both invalidate the discovery cache.
//!
//! ## SparrowDB-dialect adaptations (ADR-0001)
//!
//! The operator's reference design used `datetime()`, `OPTIONAL MATCH`,
//! `WHERE`-filtered variable-length paths, `LIST<>` properties and
//! `CALL`-procedures — none reliable in the pinned SparrowDB. So: timestamps are
//! RFC3339 **strings** compared in Rust; status/expiry are filtered in Rust over
//! inline-matched rows; the management-chain check reuses the in-memory
//! [`DiscoveryGraph`](crate::graph::DiscoveryGraph); granted id-lists are stored
//! as `|`-joined string properties; writes use parameterized node `MERGE` /
//! `MATCH … SET` and inline relationship `MERGE` (the blessed write paths).
//!
//! ## Identity stays host-authoritative
//!
//! Unlike the reference snippet, a caller's identity is **never** injected into a
//! payload. [`authorize_skill`](ScopeGrants::authorize_skill) verifies the caller
//! (a host-authoritative [`FirmContext`]) and *returns* the composed scope the
//! skill must run under; the host applies it (discovery already filters every
//! answer through a `VisibilityScope`).

use chrono::{DateTime, Utc};
use kanbrick_auth::AuditLog;
use kanbrick_core::{ClearanceLevel, Error, FirmContext, Result};
use kanbrick_store::{Params, Store};
use serde::Deserialize;

use crate::cache::DiscoveryCache;
use crate::graph::DiscoveryGraph;
use crate::scope::{ProjectScope, VisibilityScope};

/// Minimum clearance a grantor must hold (the dual-gate clearance threshold).
pub const MIN_GRANTOR_CLEARANCE: ClearanceLevel = ClearanceLevel::L4;

/// Lifecycle state of a [`ScopeRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestStatus {
    /// Submitted, awaiting a decision.
    Requested,
    /// Approved — a [`ProjectScope`] was created.
    Granted,
    /// Rejected by a grantor.
    Denied,
    /// Superseded (e.g. its granted scope was revoked).
    Expired,
}

impl RequestStatus {
    fn as_str(self) -> &'static str {
        match self {
            RequestStatus::Requested => "REQUESTED",
            RequestStatus::Granted => "GRANTED",
            RequestStatus::Denied => "DENIED",
            RequestStatus::Expired => "EXPIRED",
        }
    }

    fn parse(s: &str) -> RequestStatus {
        match s {
            "GRANTED" => RequestStatus::Granted,
            "DENIED" => RequestStatus::Denied,
            "EXPIRED" => RequestStatus::Expired,
            _ => RequestStatus::Requested,
        }
    }
}

/// Lifecycle state of a granted [`ProjectScope`] record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeStatus {
    /// Created, not yet usable (reserved for future multi-step approval).
    Pending,
    /// Active and enforceable.
    Active,
    /// Past its `expires_at`.
    Expired,
    /// Terminated early by a grantor / L5.
    Revoked,
}

impl ScopeStatus {
    fn as_str(self) -> &'static str {
        match self {
            ScopeStatus::Pending => "PENDING",
            ScopeStatus::Active => "ACTIVE",
            ScopeStatus::Expired => "EXPIRED",
            ScopeStatus::Revoked => "REVOKED",
        }
    }

    fn parse(s: &str) -> ScopeStatus {
        match s {
            "ACTIVE" => ScopeStatus::Active,
            "EXPIRED" => ScopeStatus::Expired,
            "REVOKED" => ScopeStatus::Revoked,
            _ => ScopeStatus::Pending,
        }
    }
}

/// A submitted request for extra, project-scoped visibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRequest {
    /// Request id (UUID).
    pub id: String,
    /// The project this request is for.
    pub project: String,
    /// The requesting employee's email.
    pub requester: String,
    /// Free-text justification.
    pub justification: String,
    /// Requested person emails.
    pub persons: Vec<String>,
    /// Requested company codes.
    pub companies: Vec<String>,
    /// Current lifecycle state.
    pub status: RequestStatus,
}

/// A persisted, granted project scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantedScope {
    /// Scope id (UUID).
    pub id: String,
    /// The project the scope is bound to.
    pub project: String,
    /// The employee the scope grants access to.
    pub requester: String,
    /// The grantor who approved it.
    pub granted_by: String,
    /// Granted person emails (additive over the base scope).
    pub granted_persons: Vec<String>,
    /// Granted company codes (additive over the base scope).
    pub granted_companies: Vec<String>,
    /// Expiry instant (RFC3339), if any.
    pub expires_at: Option<String>,
    /// Current lifecycle state.
    pub status: ScopeStatus,
}

/// A finite, per-project skill bound to a scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// Skill id (UUID).
    pub id: String,
    /// Skill name (e.g. `"deal-modeling"`).
    pub name: String,
    /// The scope this skill is bound to.
    pub scope_id: String,
    /// The mesh guest that hosts the skill.
    pub guest: String,
    /// Minimum clearance required to invoke it.
    pub required_clearance: ClearanceLevel,
}

// ---- persistence row shapes ------------------------------------------------

#[derive(Debug, Deserialize)]
struct RequestRow {
    id: String,
    project: String,
    requester: String,
    #[serde(default)]
    justification: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    persons: String,
    #[serde(default)]
    companies: String,
}

#[derive(Debug, Deserialize)]
struct ScopeRow {
    id: String,
    project: String,
    requester: String,
    #[serde(default)]
    granted_by: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    granted_persons: String,
    #[serde(default)]
    granted_companies: String,
    #[serde(default)]
    expires_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SkillRow {
    id: String,
    name: String,
    scope_id: String,
    #[serde(default)]
    guest: String,
    #[serde(default)]
    required_clearance: String,
}

/// The scope-grant lifecycle service over a [`Store`].
#[derive(Debug)]
pub struct ScopeGrants<'a> {
    store: &'a Store,
}

impl<'a> ScopeGrants<'a> {
    /// Build a service over `store`.
    pub fn new(store: &'a Store) -> Self {
        ScopeGrants { store }
    }

    // ---- request -----------------------------------------------------------

    /// Submit a project-scope request on behalf of `requester`.
    pub fn request_scope(
        &self,
        requester: &FirmContext,
        project: &str,
        persons: &[String],
        companies: &[String],
        justification: &str,
    ) -> Result<ScopeRequest> {
        let id = new_id();
        let params = Params::new()
            .with("id", id.as_str())
            .with("project", project)
            .with("requester", requester.email.as_str())
            .with("justification", justification)
            .with("status", RequestStatus::Requested.as_str())
            .with("requested_at", now_str())
            .with("persons", join_ids(persons))
            .with("companies", join_ids(companies));
        self.store.execute_with(
            "MERGE (sr:ScopeRequest {id: $id, project: $project, requester: $requester, \
             justification: $justification, status: $status, requested_at: $requested_at, \
             persons: $persons, companies: $companies})",
            params,
        )?;
        self.audit(requester, &format!("scope-request:created:{id}"))?;
        Ok(ScopeRequest {
            id,
            project: project.to_string(),
            requester: requester.email.clone(),
            justification: justification.to_string(),
            persons: persons.to_vec(),
            companies: companies.to_vec(),
            status: RequestStatus::Requested,
        })
    }

    /// Read a request by id.
    pub fn request(&self, id: &str) -> Result<Option<ScopeRequest>> {
        let rows: Vec<RequestRow> = self.store.query(
            "MATCH (sr:ScopeRequest {id: $id}) RETURN sr.id, sr.project, sr.requester, \
             sr.justification, sr.status, sr.persons, sr.companies",
            Params::new().with("id", id),
        )?;
        Ok(rows.into_iter().next().map(|r| ScopeRequest {
            id: r.id,
            project: r.project,
            requester: r.requester,
            justification: r.justification,
            persons: split_ids(&r.persons),
            companies: split_ids(&r.companies),
            status: RequestStatus::parse(&r.status),
        }))
    }

    // ---- approve / deny ----------------------------------------------------

    /// Whether `grantor` may approve a request from `requester_email`.
    ///
    /// Dual gate: clearance ≥ [`MIN_GRANTOR_CLEARANCE`] **and** the grantor is in
    /// the requester's management chain — unless the grantor is an L5 cofounder
    /// (firm-wide override). The management chain is read from `graph`.
    pub fn eligible_grantor(
        &self,
        graph: &DiscoveryGraph,
        requester_email: &str,
        grantor: &FirmContext,
    ) -> bool {
        let clearance_ok = grantor.clearance >= MIN_GRANTOR_CLEARANCE;
        let is_cofounder = grantor.clearance >= ClearanceLevel::L5;
        let in_chain = graph
            .ancestors(requester_email)
            .iter()
            .any(|m| m == &grantor.email);
        clearance_ok && (in_chain || is_cofounder)
    }

    /// Approve a pending request: persist an ACTIVE [`ProjectScope`] granting
    /// exactly what was requested, bound to (requester, project), expiring after
    /// `ttl_days` (when set). Fails unless `grantor` is eligible.
    pub fn approve(
        &self,
        request_id: &str,
        grantor: &FirmContext,
        graph: &DiscoveryGraph,
        ttl_days: Option<i64>,
    ) -> Result<GrantedScope> {
        let request = self
            .request(request_id)?
            .ok_or_else(|| Error::NotFound(format!("scope request {request_id}")))?;
        if request.status != RequestStatus::Requested {
            return Err(Error::InvalidInput(format!(
                "request {request_id} is {:?}, not pending",
                request.status
            )));
        }
        if !self.eligible_grantor(graph, &request.requester, grantor) {
            return Err(Error::AccessDenied {
                required: MIN_GRANTOR_CLEARANCE,
                actual: grantor.clearance,
            });
        }

        let scope_id = new_id();
        let expires_at = ttl_days.map(|d| (Utc::now() + chrono::Duration::days(d)).to_rfc3339());
        let mut params = Params::new()
            .with("id", scope_id.as_str())
            .with("project", request.project.as_str())
            .with("requester", request.requester.as_str())
            .with("granted_by", grantor.email.as_str())
            .with("status", ScopeStatus::Active.as_str())
            .with("created_at", now_str())
            .with("granted_persons", join_ids(&request.persons))
            .with("granted_companies", join_ids(&request.companies));
        params.insert("expires_at", expires_at.clone().unwrap_or_default());
        self.store.execute_with(
            "MERGE (ps:ProjectScope {id: $id, project: $project, requester: $requester, \
             granted_by: $granted_by, status: $status, created_at: $created_at, \
             granted_persons: $granted_persons, granted_companies: $granted_companies, \
             expires_at: $expires_at})",
            params,
        )?;

        // Mark the request granted and link it to its scope.
        self.store.execute_with(
            "MATCH (sr:ScopeRequest {id: $id}) SET sr.status = $status, sr.granted_by = $by, \
             sr.scope_id = $scope_id, sr.decided_at = $at",
            Params::new()
                .with("id", request_id)
                .with("status", RequestStatus::Granted.as_str())
                .with("by", grantor.email.as_str())
                .with("scope_id", scope_id.as_str())
                .with("at", now_str()),
        )?;

        self.audit(
            grantor,
            &format!("scope:granted:{scope_id}:to:{}", request.requester),
        )?;
        Ok(GrantedScope {
            id: scope_id,
            project: request.project,
            requester: request.requester,
            granted_by: grantor.email.clone(),
            granted_persons: request.persons,
            granted_companies: request.companies,
            expires_at,
            status: ScopeStatus::Active,
        })
    }

    /// Deny a pending request (records the reason and a decision). Requires an
    /// eligible grantor.
    pub fn deny(
        &self,
        request_id: &str,
        grantor: &FirmContext,
        graph: &DiscoveryGraph,
        reason: &str,
    ) -> Result<()> {
        let request = self
            .request(request_id)?
            .ok_or_else(|| Error::NotFound(format!("scope request {request_id}")))?;
        if request.status != RequestStatus::Requested {
            return Err(Error::InvalidInput(format!(
                "request {request_id} is {:?}, not pending",
                request.status
            )));
        }
        if !self.eligible_grantor(graph, &request.requester, grantor) {
            return Err(Error::AccessDenied {
                required: MIN_GRANTOR_CLEARANCE,
                actual: grantor.clearance,
            });
        }
        self.store.execute_with(
            "MATCH (sr:ScopeRequest {id: $id}) SET sr.status = $status, sr.granted_by = $by, \
             sr.denied_reason = $reason, sr.decided_at = $at",
            Params::new()
                .with("id", request_id)
                .with("status", RequestStatus::Denied.as_str())
                .with("by", grantor.email.as_str())
                .with("reason", reason)
                .with("at", now_str()),
        )?;
        self.audit(grantor, &format!("scope-request:denied:{request_id}"))?;
        Ok(())
    }

    // ---- read / compose ----------------------------------------------------

    /// Read a persisted scope by id.
    pub fn scope(&self, id: &str) -> Result<Option<GrantedScope>> {
        let rows: Vec<ScopeRow> = self.store.query(
            "MATCH (ps:ProjectScope {id: $id}) RETURN ps.id, ps.project, ps.requester, \
             ps.granted_by, ps.status, ps.granted_persons, ps.granted_companies, ps.expires_at",
            Params::new().with("id", id),
        )?;
        Ok(rows.into_iter().next().map(scope_from_row))
    }

    /// All ACTIVE, unexpired scopes bound to (`requester_email`, `project`) as of
    /// `now`. (A scope past its `expires_at` is treated as expired even before a
    /// sweep has run, so enforcement never depends on the cron.)
    pub fn active_scopes_for(
        &self,
        requester_email: &str,
        project: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<GrantedScope>> {
        let rows: Vec<ScopeRow> = self.store.query(
            "MATCH (ps:ProjectScope {requester: $requester, project: $project}) \
             RETURN ps.id, ps.project, ps.requester, ps.granted_by, ps.status, \
             ps.granted_persons, ps.granted_companies, ps.expires_at",
            Params::new()
                .with("requester", requester_email)
                .with("project", project),
        )?;
        Ok(rows
            .into_iter()
            .map(scope_from_row)
            .filter(|s| s.status == ScopeStatus::Active && !is_expired(&s.expires_at, now))
            .collect())
    }

    /// Compose `base` with every active grant for (requester, project) into a
    /// single [`ProjectScope`]. Returns `None` when there is no active grant.
    pub fn active_scope_for(
        &self,
        base: impl VisibilityScope + 'static,
        requester_email: &str,
        project: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<ProjectScope>> {
        let active = self.active_scopes_for(requester_email, project, now)?;
        if active.is_empty() {
            return Ok(None);
        }
        let mut scope = ProjectScope::new(base, project);
        for grant in active {
            scope = scope
                .grant_persons(grant.granted_persons)
                .grant_companies(grant.granted_companies);
        }
        Ok(Some(scope))
    }

    // ---- revoke / expire ---------------------------------------------------

    /// Revoke an active scope immediately. The actor must be the granting
    /// grantor or an L5. Cascades its granted request to EXPIRED and invalidates
    /// the discovery cache.
    pub fn revoke(
        &self,
        scope_id: &str,
        actor: &FirmContext,
        reason: &str,
        cache: Option<&DiscoveryCache>,
    ) -> Result<()> {
        let scope = self
            .scope(scope_id)?
            .ok_or_else(|| Error::NotFound(format!("project scope {scope_id}")))?;
        let authorized = actor.email == scope.granted_by || actor.clearance >= ClearanceLevel::L5;
        if !authorized {
            return Err(Error::AccessDenied {
                required: ClearanceLevel::L5,
                actual: actor.clearance,
            });
        }
        self.store.execute_with(
            "MATCH (ps:ProjectScope {id: $id}) SET ps.status = $status, ps.revoked_by = $by, \
             ps.revoked_reason = $reason, ps.revoked_at = $at",
            Params::new()
                .with("id", scope_id)
                .with("status", ScopeStatus::Revoked.as_str())
                .with("by", actor.email.as_str())
                .with("reason", reason)
                .with("at", now_str()),
        )?;
        // Cascade: the request that produced this scope is now spent.
        self.store.execute_with(
            "MATCH (sr:ScopeRequest {scope_id: $scope_id}) SET sr.status = $status",
            Params::new()
                .with("scope_id", scope_id)
                .with("status", RequestStatus::Expired.as_str()),
        )?;
        if let Some(cache) = cache {
            cache.invalidate_all();
        }
        self.audit(actor, &format!("scope:revoked:{scope_id}"))?;
        Ok(())
    }

    /// Sweep ACTIVE scopes whose `expires_at` is before `now` into EXPIRED.
    /// Invalidates the discovery cache when anything expired. Returns the count.
    pub fn expire_due(&self, now: DateTime<Utc>, cache: Option<&DiscoveryCache>) -> Result<usize> {
        let rows: Vec<ScopeRow> = self.store.query(
            "MATCH (ps:ProjectScope {status: $status}) RETURN ps.id, ps.project, ps.requester, \
             ps.granted_by, ps.status, ps.granted_persons, ps.granted_companies, ps.expires_at",
            Params::new().with("status", ScopeStatus::Active.as_str()),
        )?;
        let mut expired = 0usize;
        for row in rows {
            let scope = scope_from_row(row);
            if is_expired(&scope.expires_at, now) {
                self.store.execute_with(
                    "MATCH (ps:ProjectScope {id: $id}) SET ps.status = $status, ps.expired_at = $at",
                    Params::new()
                        .with("id", scope.id.as_str())
                        .with("status", ScopeStatus::Expired.as_str())
                        .with("at", now.to_rfc3339()),
                )?;
                expired += 1;
            }
        }
        if expired > 0 {
            if let Some(cache) = cache {
                cache.invalidate_all();
            }
        }
        Ok(expired)
    }

    // ---- skills ------------------------------------------------------------

    /// Define a per-project skill bound to `scope_id`.
    pub fn define_skill(
        &self,
        scope_id: &str,
        name: &str,
        guest: &str,
        required_clearance: ClearanceLevel,
    ) -> Result<Skill> {
        if self.scope(scope_id)?.is_none() {
            return Err(Error::NotFound(format!("project scope {scope_id}")));
        }
        let id = new_id();
        self.store.execute_with(
            "MERGE (s:Skill {id: $id, name: $name, scope_id: $scope_id, guest: $guest, \
             required_clearance: $clearance})",
            Params::new()
                .with("id", id.as_str())
                .with("name", name)
                .with("scope_id", scope_id)
                .with("guest", guest)
                .with("clearance", required_clearance.to_string()),
        )?;
        // Bind skill → scope (idempotent inline relationship MERGE; ids are UUIDs).
        self.store.execute(&format!(
            "MATCH (ps:ProjectScope {{id: '{}'}}), (s:Skill {{id: '{}'}}) MERGE (ps)-[:HAS_SKILL]->(s)",
            escape(scope_id),
            escape(&id),
        ))?;
        Ok(Skill {
            id,
            name: name.to_string(),
            scope_id: scope_id.to_string(),
            guest: guest.to_string(),
            required_clearance,
        })
    }

    /// All skills bound to `scope_id`.
    pub fn skills_for_scope(&self, scope_id: &str) -> Result<Vec<Skill>> {
        let rows: Vec<SkillRow> = self.store.query(
            "MATCH (ps:ProjectScope {id: $id})-[:HAS_SKILL]->(s:Skill) \
             RETURN s.id, s.name, s.scope_id, s.guest, s.required_clearance",
            Params::new().with("id", scope_id),
        )?;
        Ok(rows.into_iter().map(skill_from_row).collect())
    }

    /// Authorize `caller` to invoke the named skill on a scope, returning the
    /// skill and the composed [`ProjectScope`] it must run under.
    ///
    /// The runtime gate, adapted to keep identity host-authoritative: the scope
    /// must be ACTIVE and unexpired, the caller must be the scope's grantee, and
    /// the caller's clearance must meet the skill's minimum. The returned
    /// `ProjectScope` is what the host runs the skill (and its discovery queries)
    /// under — identity is never injected into a payload.
    pub fn authorize_skill(
        &self,
        caller: &FirmContext,
        base: impl VisibilityScope + 'static,
        scope_id: &str,
        skill_name: &str,
        now: DateTime<Utc>,
    ) -> Result<(Skill, ProjectScope)> {
        let scope = self
            .scope(scope_id)?
            .ok_or_else(|| Error::NotFound(format!("project scope {scope_id}")))?;
        if scope.status != ScopeStatus::Active || is_expired(&scope.expires_at, now) {
            return Err(Error::InvalidInput(format!(
                "scope {scope_id} is not active"
            )));
        }
        if caller.email != scope.requester {
            return Err(Error::AccessDenied {
                required: caller.clearance,
                actual: caller.clearance,
            });
        }
        let skill = self
            .skills_for_scope(scope_id)?
            .into_iter()
            .find(|s| s.name == skill_name)
            .ok_or_else(|| Error::NotFound(format!("skill {skill_name} on scope {scope_id}")))?;
        if caller.clearance < skill.required_clearance {
            return Err(Error::AccessDenied {
                required: skill.required_clearance,
                actual: caller.clearance,
            });
        }
        let composed = ProjectScope::new(base, scope.project.as_str())
            .grant_persons(scope.granted_persons.clone())
            .grant_companies(scope.granted_companies.clone());
        self.audit(caller, &format!("skill:authorized:{skill_name}:{scope_id}"))?;
        Ok((skill, composed))
    }

    fn audit(&self, ctx: &FirmContext, marker: &str) -> Result<()> {
        AuditLog::new(self.store).record(ctx, marker).map(|_| ())
    }
}

// ---- helpers ---------------------------------------------------------------

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn now_str() -> String {
    Utc::now().to_rfc3339()
}

/// Join ids with `|` (neither emails nor company codes contain `|`).
fn join_ids(ids: &[String]) -> String {
    ids.join("|")
}

fn split_ids(s: &str) -> Vec<String> {
    if s.is_empty() {
        Vec::new()
    } else {
        s.split('|').map(str::to_string).collect()
    }
}

/// Whether `expires_at` (RFC3339) is strictly before `now`. A missing/blank
/// expiry never expires (an explicitly open-ended grant); an **unparseable**
/// expiry reads as expired — fail closed (P16.1, ADR-0022), because a grant
/// whose sunset cannot be read must not confer standing authority (autonomy
/// promotion rides grant TTLs from P16.4 on).
fn is_expired(expires_at: &Option<String>, now: DateTime<Utc>) -> bool {
    match expires_at.as_deref() {
        Some(s) if !s.is_empty() => DateTime::parse_from_rfc3339(s)
            .map(|exp| exp.with_timezone(&Utc) < now)
            .unwrap_or(true),
        _ => false,
    }
}

fn escape(id: &str) -> String {
    id.replace('\\', "\\\\").replace('\'', "\\'")
}

fn scope_from_row(r: ScopeRow) -> GrantedScope {
    GrantedScope {
        id: r.id,
        project: r.project,
        requester: r.requester,
        granted_by: r.granted_by,
        granted_persons: split_ids(&r.granted_persons),
        granted_companies: split_ids(&r.granted_companies),
        expires_at: r.expires_at.filter(|s| !s.is_empty()),
        status: ScopeStatus::parse(&r.status),
    }
}

fn skill_from_row(r: SkillRow) -> Skill {
    Skill {
        id: r.id,
        name: r.name,
        scope_id: r.scope_id,
        guest: r.guest,
        required_clearance: parse_clearance(&r.required_clearance),
    }
}

fn parse_clearance(s: &str) -> ClearanceLevel {
    match s {
        "L5" => ClearanceLevel::L5,
        "L4" => ClearanceLevel::L4,
        "L3" => ClearanceLevel::L3,
        "L2" => ClearanceLevel::L2,
        _ => ClearanceLevel::L1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{ctx, seeded_store};
    use crate::DiscoveryEngine;
    use kanbrick_auth::ClearanceScope;

    fn graph_of(store: &Store) -> DiscoveryGraph {
        DiscoveryGraph::from_store(store).unwrap()
    }

    fn base_scope(store: &Store, email: &str, level: ClearanceLevel) -> ClearanceScope {
        ClearanceScope::resolve(store, &ctx(email, level)).unwrap()
    }

    #[test]
    fn dual_gate_grantor_eligibility() {
        let (_d, store) = seeded_store();
        let g = graph_of(&store);
        let svc = ScopeGrants::new(&store);
        let requester = "samantha.jordan@kanbrick.com";

        // Peter (CSO, L4) is in Samantha's chain → eligible.
        assert!(svc.eligible_grantor(
            &g,
            requester,
            &ctx("peter.nash@kanbrick.com", ClearanceLevel::L4)
        ));
        // Tracy (CEO, L5) — cofounder override, eligible regardless of chain.
        assert!(svc.eligible_grantor(
            &g,
            requester,
            &ctx("tracy.brittcool@kanbrick.com", ClearanceLevel::L5)
        ));
        // Tyler (L3) is in the chain but below the clearance threshold → no.
        assert!(!svc.eligible_grantor(
            &g,
            requester,
            &ctx("tyler.begemann@kanbrick.com", ClearanceLevel::L3)
        ));
        // Andrea (CFO, L4) has the clearance but is NOT in Samantha's chain → no.
        assert!(!svc.eligible_grantor(
            &g,
            requester,
            &ctx("andrea.lewis@kanbrick.com", ClearanceLevel::L4)
        ));
    }

    #[test]
    fn full_lifecycle_request_approve_use_revoke() {
        let (_d, store) = seeded_store();
        let g = graph_of(&store);
        let engine = DiscoveryEngine::from_store(&store).unwrap();
        let svc = ScopeGrants::new(&store);
        let now = Utc::now();

        let elena = ctx("elena.ruiz@kanbrick.com", ClearanceLevel::L2);
        // 1. Elena requests JMTS + Tyler for the valuation project.
        let req = svc
            .request_scope(
                &elena,
                "valuation-jmts",
                &["tyler.begemann@kanbrick.com".to_string()],
                &["JMTS".to_string()],
                "Need JMTS + the segment lead for the valuation.",
            )
            .unwrap();
        assert_eq!(req.status, RequestStatus::Requested);
        assert_eq!(
            svc.request(&req.id).unwrap().unwrap().status,
            RequestStatus::Requested
        );

        // 2. A low-clearance grantor cannot approve.
        let err = svc.approve(&req.id, &elena, &g, Some(30)).unwrap_err();
        assert_eq!(err.kind(), kanbrick_core::ErrorKind::Unauthorized);

        // 2b. Peter (L4, in chain) approves.
        let peter = ctx("peter.nash@kanbrick.com", ClearanceLevel::L4);
        let granted = svc.approve(&req.id, &peter, &g, Some(30)).unwrap();
        assert_eq!(granted.status, ScopeStatus::Active);
        assert_eq!(
            svc.request(&req.id).unwrap().unwrap().status,
            RequestStatus::Granted
        );

        // 3 & 4. Under the composed scope Elena now sees JMTS + Tyler — and
        // nothing more (the grant is additive over her L2 base).
        let scope = svc
            .active_scope_for(
                base_scope(&store, "elena.ruiz@kanbrick.com", ClearanceLevel::L2),
                "elena.ruiz@kanbrick.com",
                "valuation-jmts",
                now,
            )
            .unwrap()
            .expect("an active scope");
        assert!(!scope.sees_all());
        let jmts = engine.scoped_company_stakeholders(&scope, "JMTS").unwrap();
        assert_eq!(jmts.len(), 1);
        assert_eq!(jmts[0].person.email(), "tyler.begemann@kanbrick.com");
        // A company she was not granted stays invisible.
        assert!(engine
            .scoped_company_stakeholders(&scope, "KEEP")
            .unwrap()
            .is_empty());

        // 5. A skill defined on the scope authorizes under the composed scope.
        let skill = svc
            .define_skill(
                &granted.id,
                "deal-modeling",
                "valuation",
                ClearanceLevel::L2,
            )
            .unwrap();
        assert_eq!(
            svc.skills_for_scope(&granted.id).unwrap(),
            vec![skill.clone()]
        );
        let (auth_skill, composed) = svc
            .authorize_skill(
                &elena,
                base_scope(&store, "elena.ruiz@kanbrick.com", ClearanceLevel::L2),
                &granted.id,
                "deal-modeling",
                now,
            )
            .unwrap();
        assert_eq!(auth_skill, skill);
        // The skill runs under the grant: it sees JMTS' granted stakeholder.
        assert_eq!(
            engine
                .scoped_company_stakeholders(&composed, "JMTS")
                .unwrap()
                .len(),
            1
        );

        // 6. The whole chain was audited under the actors' identities.
        let audit = AuditLog::new(&store);
        assert!(audit.count_for_user(elena.user_id).unwrap() >= 2); // request + skill auth
        assert!(audit.count_for_user(peter.user_id).unwrap() >= 1); // grant

        // 7. Revocation removes the added visibility immediately.
        svc.revoke(&granted.id, &peter, "valuation complete", None)
            .unwrap();
        assert!(svc
            .active_scope_for(
                base_scope(&store, "elena.ruiz@kanbrick.com", ClearanceLevel::L2),
                "elena.ruiz@kanbrick.com",
                "valuation-jmts",
                now
            )
            .unwrap()
            .is_none());
        // The granting request is cascaded to EXPIRED.
        assert_eq!(
            svc.request(&req.id).unwrap().unwrap().status,
            RequestStatus::Expired
        );
    }

    #[test]
    fn expiry_is_fail_closed_on_unparseable_timestamps() {
        let now = Utc::now();
        // Missing/blank = an explicitly open-ended grant: never expires.
        assert!(!is_expired(&None, now));
        assert!(!is_expired(&Some(String::new()), now));
        // A readable future expiry is not expired; a past one is.
        let future = (now + chrono::Duration::days(1)).to_rfc3339();
        assert!(!is_expired(&Some(future), now));
        let past = (now - chrono::Duration::days(1)).to_rfc3339();
        assert!(is_expired(&Some(past), now));
        // An unreadable expiry can no longer mean "never expires" (P16.1,
        // ADR-0022): a sunset that cannot be parsed reads as already past.
        assert!(is_expired(&Some("not-a-timestamp".to_string()), now));
        assert!(is_expired(&Some("2026-13-45T99:99:99Z".to_string()), now));
    }

    #[test]
    fn a_revoked_scope_stops_authorize_skill_at_the_next_gate() {
        let (_d, store) = seeded_store();
        let g = graph_of(&store);
        let svc = ScopeGrants::new(&store);
        let now = Utc::now();

        let elena = ctx("elena.ruiz@kanbrick.com", ClearanceLevel::L2);
        let req = svc
            .request_scope(&elena, "p2", &[], &["JMTS".to_string()], "j")
            .unwrap();
        let tracy = ctx("tracy.brittcool@kanbrick.com", ClearanceLevel::L5);
        let granted = svc.approve(&req.id, &tracy, &g, Some(30)).unwrap();
        svc.define_skill(
            &granted.id,
            "deal-modeling",
            "valuation",
            ClearanceLevel::L2,
        )
        .unwrap();

        // The gate passes while the scope is active…
        svc.authorize_skill(
            &elena,
            base_scope(&store, "elena.ruiz@kanbrick.com", ClearanceLevel::L2),
            &granted.id,
            "deal-modeling",
            now,
        )
        .unwrap();

        // …and fails at the very next call after a revoke: authorize_skill
        // re-reads scope status from the store (no cache sits in this path),
        // which is what propagates a revocation into an in-flight loop at its
        // next step boundary (P16.1 verification).
        svc.revoke(&granted.id, &tracy, "pulled", None).unwrap();
        let err = svc
            .authorize_skill(
                &elena,
                base_scope(&store, "elena.ruiz@kanbrick.com", ClearanceLevel::L2),
                &granted.id,
                "deal-modeling",
                now,
            )
            .unwrap_err();
        assert_eq!(err.kind(), kanbrick_core::ErrorKind::ValidationError);
    }

    #[test]
    fn expiry_sweep_removes_visibility_and_invalidates_cache() {
        let (_d, store) = seeded_store();
        let g = graph_of(&store);
        let svc = ScopeGrants::new(&store);
        let cache = DiscoveryCache::new(std::time::Duration::from_secs(60));

        let elena = ctx("elena.ruiz@kanbrick.com", ClearanceLevel::L2);
        let req = svc
            .request_scope(&elena, "p1", &[], &["JMTS".to_string()], "j")
            .unwrap();
        let tracy = ctx("tracy.brittcool@kanbrick.com", ClearanceLevel::L5);
        // Grant with a 1-day TTL, then sweep with a clock 2 days later.
        let granted = svc.approve(&req.id, &tracy, &g, Some(1)).unwrap();
        assert!(granted.expires_at.is_some());

        let later = Utc::now() + chrono::Duration::days(2);
        // Already-expired by clock: enforcement does not wait for the sweep.
        assert!(svc
            .active_scopes_for("elena.ruiz@kanbrick.com", "p1", later)
            .unwrap()
            .is_empty());

        // The sweep flips status and invalidates the cache.
        let n = svc.expire_due(later, Some(&cache)).unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            svc.scope(&granted.id).unwrap().unwrap().status,
            ScopeStatus::Expired
        );
        // A second sweep is a no-op (status is no longer ACTIVE).
        assert_eq!(svc.expire_due(later, Some(&cache)).unwrap(), 0);
    }

    #[test]
    fn skill_clearance_gate_and_grantee_only() {
        let (_d, store) = seeded_store();
        let g = graph_of(&store);
        let svc = ScopeGrants::new(&store);
        let now = Utc::now();

        let elena = ctx("elena.ruiz@kanbrick.com", ClearanceLevel::L2);
        let req = svc
            .request_scope(&elena, "p", &[], &["JMTS".to_string()], "j")
            .unwrap();
        let tracy = ctx("tracy.brittcool@kanbrick.com", ClearanceLevel::L5);
        let granted = svc.approve(&req.id, &tracy, &g, None).unwrap();

        // A skill that needs L4 cannot be invoked by L2 Elena.
        svc.define_skill(&granted.id, "lp-reporting", "reporting", ClearanceLevel::L4)
            .unwrap();
        let err = svc
            .authorize_skill(
                &elena,
                base_scope(&store, "elena.ruiz@kanbrick.com", ClearanceLevel::L2),
                &granted.id,
                "lp-reporting",
                now,
            )
            .unwrap_err();
        assert_eq!(err.kind(), kanbrick_core::ErrorKind::Unauthorized);

        // Someone who is not the grantee cannot invoke a skill on Elena's scope.
        let samantha = ctx("samantha.jordan@kanbrick.com", ClearanceLevel::L2);
        svc.define_skill(&granted.id, "ok-skill", "valuation", ClearanceLevel::L1)
            .unwrap();
        let err = svc
            .authorize_skill(
                &samantha,
                base_scope(&store, "samantha.jordan@kanbrick.com", ClearanceLevel::L2),
                &granted.id,
                "ok-skill",
                now,
            )
            .unwrap_err();
        assert_eq!(err.kind(), kanbrick_core::ErrorKind::Unauthorized);
    }
}
