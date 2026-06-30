//! `/me/skills` + `/me/scopes/{id}/skills` — the skill-registry ⇄ grant bridge
//! (P11.2b). Connects the versioned skill catalogue (P11.1,
//! [`kanbrick_store::skill_registry`]) to the per-scope skill primitive of the
//! grant gate ([`kanbrick_discovery::ScopeGrants`]).
//!
//! Two surfaces:
//!
//! * **Catalogue** (`/me/skills`) — publish a `SKILL.md` edition into the
//!   versioned registry and browse it. Publishing parses the manifest with
//!   [`parse_skill_md`] and **host-stamps** the `source` from the authenticated
//!   identity (ADR-0002/0016) — never a body field. Malformed `SKILL.md` is a
//!   `400` (`invalid_skill_md`). Publishing is L3-gated; the catalogue confers no
//!   access (it is just the firm's skill library).
//!
//! * **Bridge** (`/me/scopes/{id}/skills`) — bind a published edition onto an
//!   approved [`ProjectScope`](kanbrick_discovery::ProjectScope) via
//!   [`ScopeGrants::define_skill`], picking up the edition's `min_clearance` as the
//!   skill's run-time clearance floor. Binding is **define, not run**: it is gated
//!   on scope *ownership* (or an L5 cofounder), and it deliberately does **not**
//!   re-check the binder's own clearance against the skill — that is the run gate
//!   ([`ScopeGrants::authorize_skill`], wired in P11.3). So an L2 scope owner may
//!   bind an L4-requiring skill; whether they can actually *run* it is decided
//!   later, at invocation.
//!
//! The registry record ([`SkillVersionRecord`]) is `serde`-serializable and is
//! returned directly; the grant [`Skill`] is not, so it crosses the wire as the
//! thin [`SkillDto`] defined here.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use kanbrick_auth::{require_clearance, AuditLog};
use kanbrick_core::ClearanceLevel;
use kanbrick_discovery::{DiscoveryGraph, ScopeGrants, Skill};
use kanbrick_loops::{parse_skill_md, SkillParseError};
use kanbrick_store::{
    get_skill_version, latest_skill_version, list_skill_versions, list_skills,
    pending_skill_versions, publish_skill_version, set_skill_review, skill_owner,
    SkillVersionRecord, REVIEW_APPROVED, REVIEW_REJECTED,
};
use serde::{Deserialize, Serialize};

use crate::{ApiError, AppState, AuthedContext};

/// Minimum clearance to publish a skill edition into the firm catalogue. Authoring
/// a reusable skill is an operational act (L3); browsing the catalogue and binding
/// an edition onto a scope you own are not gated by this floor.
const PUBLISH_SKILL_CLEARANCE: ClearanceLevel = ClearanceLevel::L3;

/// Minimum clearance to browse the skill catalogue. The floor (L1) — any
/// authenticated employee may see the firm's published skills to pick ones for
/// their workstation (Req 2.3). The catalogue is metadata only; it confers no
/// access (the grant gate stays the sole authorization).
const BROWSE_SKILLS_CLEARANCE: ClearanceLevel = ClearanceLevel::L1;

/// Minimum clearance for a non-owner to read the skills bound to a scope. A scope's
/// owner always may; otherwise an L4 strategic reviewer can inspect any scope's
/// bound skills (mirrors the grantor floor used elsewhere in the grant surface).
const INSPECT_SCOPE_CLEARANCE: ClearanceLevel = ClearanceLevel::L4;

/// Minimum clearance to see the publish-review queue and decide a review (P11.8). The
/// dual-gate's clearance floor (L4); `review_skill` additionally re-checks that the
/// reviewer is an eligible grantor over the *skill's author* (management chain or L5).
const REVIEW_SKILL_CLEARANCE: ClearanceLevel = ClearanceLevel::L4;

// ── DTO (the grant `Skill` is not serializable) ─────────────────────────────

/// Serializable mirror of [`Skill`] — a skill edition bound to a scope.
#[derive(Debug, Serialize)]
pub(crate) struct SkillDto {
    id: String,
    name: String,
    scope_id: String,
    guest: String,
    required_clearance: ClearanceLevel,
}

impl From<Skill> for SkillDto {
    fn from(s: Skill) -> Self {
        SkillDto {
            id: s.id,
            name: s.name,
            scope_id: s.scope_id,
            guest: s.guest,
            required_clearance: s.required_clearance,
        }
    }
}

// ── Request bodies ───────────────────────────────────────────────────────────

/// `POST /me/skills` body — the raw `SKILL.md` source. The `source`/author is
/// host-stamped from the authenticated identity, never carried in the body.
#[derive(Debug, Deserialize)]
pub(crate) struct PublishSkillBody {
    skill_md: String,
}

/// `POST /me/scopes/{id}/skills` body — which published edition to bind. `version`
/// is optional; absent binds the latest published edition.
#[derive(Debug, Deserialize)]
pub(crate) struct BindSkillBody {
    skill_name: String,
    #[serde(default)]
    version: Option<String>,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// `POST /me/skills` — publish a `SKILL.md` edition into the versioned catalogue.
/// **L3-gated.** Malformed `SKILL.md` is a `400` (`invalid_skill_md`).
pub(crate) async fn publish_skill(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Json(body): Json<PublishSkillBody>,
) -> Result<Json<SkillVersionRecord>, ApiError> {
    require_clearance(&ctx, PUBLISH_SKILL_CLEARANCE)?;
    let manifest = parse_skill_md(&body.skill_md).map_err(invalid_skill_md)?;
    // Author-pin (P11.8, ADR-0021): a skill name is firm-global, and
    // `publish_skill_version` MERGEs by `name@version`. To close the cross-author
    // overwrite gap, only the name's **owner** (its first publisher) — or an L5
    // cofounder — may publish further editions of an existing name; a different L3
    // author is refused, so they can no longer re-stamp another author's
    // `source`/`guest`/`min_clearance`. A brand-new name is open to any L3+ (the owner
    // is recorded on first publish). The complementary half of the trust gate — an
    // edition is not bindable by others until an eligible lead approves it — is
    // enforced at bind time (`bind_skill`) and reviewed via `/me/skill-reviews`.
    if let Some(owner) = skill_owner(&state.store, &manifest.name)? {
        if owner != ctx.email && !ctx.clearance.satisfies(ClearanceLevel::L5) {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "forbidden",
                format!(
                    "skill {} is owned by {owner}; only its author (or an L5) may publish new editions",
                    manifest.name
                ),
            ));
        }
    }
    // Provenance is host-authoritative: the author is the authenticated caller, not
    // anything in the request body (ADR-0002/0016).
    let record = SkillVersionRecord::new(
        manifest.name,
        manifest.version,
        manifest.guest,
        manifest.clearance,
        manifest.description,
        ctx.email.clone(),
    );
    publish_skill_version(&state.store, &record)?;
    AuditLog::new(&state.store).record(
        &ctx,
        &format!("skill:publish:{}@{}", record.skill_name, record.version),
    )?;
    // `new()` stamps a placeholder `seq`; re-read the persisted edition so the
    // response carries the store-assigned publish-order `seq` (publishing makes
    // this edition the most recent, so `latest` resolves to exactly it).
    let published = latest_skill_version(&state.store, &record.skill_name)?.ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "published skill edition could not be read back",
        )
    })?;
    Ok(Json(published))
}

/// `GET /me/skills` — browse the catalogue (the latest edition of every skill).
pub(crate) async fn browse_skills(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
) -> Result<Json<Vec<SkillVersionRecord>>, ApiError> {
    require_clearance(&ctx, BROWSE_SKILLS_CLEARANCE)?;
    Ok(Json(list_skills(&state.store)?))
}

/// `GET /me/skills/{name}` — every published edition of one skill, oldest→newest.
/// An unknown skill is an empty list (not a `404`).
pub(crate) async fn skill_history(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Path(name): Path<String>,
) -> Result<Json<Vec<SkillVersionRecord>>, ApiError> {
    require_clearance(&ctx, BROWSE_SKILLS_CLEARANCE)?;
    Ok(Json(list_skill_versions(&state.store, &name)?))
}

/// `POST /me/scopes/{id}/skills` — bind a published edition onto a scope. Gated on
/// scope ownership (or an L5 cofounder). Does **not** re-check the binder's
/// clearance against the skill (that is the run gate).
pub(crate) async fn bind_skill(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Path(scope_id): Path<String>,
    Json(body): Json<BindSkillBody>,
) -> Result<Json<SkillDto>, ApiError> {
    let grants = ScopeGrants::new(&state.store);
    let scope = grants
        .scope(&scope_id)?
        .ok_or_else(|| not_found_scope(&scope_id))?;
    // define ≠ run: only the scope's grantee (or an L5) may bind skills onto it.
    // The skill's own clearance floor is enforced at run time, never here.
    if scope.requester != ctx.email && !ctx.clearance.satisfies(ClearanceLevel::L5) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "not your scope",
        ));
    }
    // Resolve the edition: a specific version (single-row key lookup) if asked, else
    // the latest published.
    let edition = match body.version.as_deref() {
        Some(v) => get_skill_version(&state.store, &body.skill_name, v)?,
        None => latest_skill_version(&state.store, &body.skill_name)?,
    }
    .ok_or_else(|| {
        let what = match &body.version {
            Some(v) => format!("skill {}@{v}", body.skill_name),
            None => format!("skill {}", body.skill_name),
        };
        ApiError::new(StatusCode::NOT_FOUND, "not_found", what)
    })?;
    // Trust gate (P11.8, ADR-0021): an edition is bindable by *others* only once an
    // eligible lead has approved it. The author may bind/run their own skill freely
    // (solo iteration), and an L5 cofounder may always bind; otherwise an unreviewed
    // (or rejected) edition is refused here, before it can be invoked through a loop.
    // A missing `review_status` (a pre-P11.8 edition) is treated as pending
    // (fail-closed). Already-bound grants are unaffected — the run gate reads the
    // `(:Skill)` snapshot, not the registry.
    let approved = edition.review_status.as_deref() == Some(REVIEW_APPROVED);
    let is_author = edition.source == ctx.email;
    if !approved && !is_author && !ctx.clearance.satisfies(ClearanceLevel::L5) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            format!(
                "skill {}@{} is not yet approved for use by others",
                edition.skill_name, edition.version
            ),
        ));
    }
    // Bind with the edition's clearance as the skill's run-time floor.
    let skill = grants.define_skill(
        &scope_id,
        &edition.skill_name,
        &edition.guest,
        edition.min_clearance,
    )?;
    AuditLog::new(&state.store).record(
        &ctx,
        &format!("skill:bind:{}:{scope_id}", edition.skill_name),
    )?;
    Ok(Json(skill.into()))
}

/// `GET /me/scopes/{id}/skills` — the skills bound to a scope. Visible to the
/// scope's owner or an L4 reviewer.
pub(crate) async fn list_scope_skills(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Path(scope_id): Path<String>,
) -> Result<Json<Vec<SkillDto>>, ApiError> {
    let grants = ScopeGrants::new(&state.store);
    let scope = grants
        .scope(&scope_id)?
        .ok_or_else(|| not_found_scope(&scope_id))?;
    if scope.requester != ctx.email && !ctx.clearance.satisfies(INSPECT_SCOPE_CLEARANCE) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "not your scope",
        ));
    }
    let dtos: Vec<SkillDto> = grants
        .skills_for_scope(&scope_id)?
        .into_iter()
        .map(SkillDto::from)
        .collect();
    Ok(Json(dtos))
}

// ── Publish trust gate: review queue + decision (P11.8, ADR-0021) ────────────

/// `POST /me/skill-reviews/{name}/{version}` body — a lead's decision on an edition.
#[derive(Debug, Deserialize)]
pub(crate) struct ReviewBody {
    /// `"approve"` | `"reject"`.
    decision: String,
    /// Optional free-text reason (audited).
    #[serde(default)]
    reason: String,
}

/// `GET /me/skill-reviews` — the queue of editions awaiting review. **L4-gated** (a
/// reviewer-facing surface). Returns every pending edition; the per-skill eligibility
/// (the reviewer must be in the author's chain, or an L5) is enforced on the decision.
pub(crate) async fn list_skill_reviews(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
) -> Result<Json<Vec<SkillVersionRecord>>, ApiError> {
    require_clearance(&ctx, REVIEW_SKILL_CLEARANCE)?;
    Ok(Json(pending_skill_versions(&state.store)?))
}

/// `POST /me/skill-reviews/{name}/{version}` — approve or reject a published edition,
/// the dual-gate lead review that makes an authored skill invocable by others (P11.8).
///
/// The reviewer must clear the L4 floor **and** be an eligible grantor over the
/// edition's *author* (in the author's management chain, or an L5 cofounder — reusing
/// [`ScopeGrants::eligible_grantor`]), and may **not** review their own skill. The
/// org-graph is built fresh per decision (as the scope-grant approve path does).
pub(crate) async fn review_skill(
    State(state): State<AppState>,
    AuthedContext(ctx): AuthedContext,
    Path((name, version)): Path<(String, String)>,
    Json(body): Json<ReviewBody>,
) -> Result<Json<SkillVersionRecord>, ApiError> {
    require_clearance(&ctx, REVIEW_SKILL_CLEARANCE)?;
    let status = match body.decision.as_str() {
        "approve" => REVIEW_APPROVED,
        "reject" => REVIEW_REJECTED,
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "decision must be \"approve\" or \"reject\"",
            ))
        }
    };
    // Resolve the edition (must exist) — also gives us its author for the eligibility
    // check (the host-stamped `source`, never a body field). Single-row key lookup.
    let edition = get_skill_version(&state.store, &name, &version)?.ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            format!("skill {name}@{version}"),
        )
    })?;
    if edition.source == ctx.email {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "cannot review your own skill",
        ));
    }
    let graph = DiscoveryGraph::from_store(&state.store)?;
    let grants = ScopeGrants::new(&state.store);
    if !grants.eligible_grantor(&graph, &edition.source, &ctx) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "not an eligible reviewer for this skill's author",
        ));
    }
    set_skill_review(
        &state.store,
        &name,
        &version,
        status,
        &ctx.email,
        &chrono::Utc::now().to_rfc3339(),
    )?;
    AuditLog::new(&state.store).record(
        &ctx,
        &format!("skill:review:{status}:{name}@{version}:{}", body.reason),
    )?;
    let updated = get_skill_version(&state.store, &name, &version)?.ok_or_else(|| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "reviewed edition could not be read back",
        )
    })?;
    Ok(Json(updated))
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// A malformed `SKILL.md` is a client error, surfaced with a stable `kind`.
fn invalid_skill_md(err: SkillParseError) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_skill_md", err.to_string())
}

/// A `404` for an unknown project scope.
fn not_found_scope(scope_id: &str) -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "not_found",
        format!("project scope {scope_id}"),
    )
}
