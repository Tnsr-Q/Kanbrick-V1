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
use kanbrick_discovery::{ScopeGrants, Skill};
use kanbrick_loops::{parse_skill_md, SkillParseError};
use kanbrick_store::{
    latest_skill_version, list_skill_versions, list_skills, publish_skill_version,
    SkillVersionRecord,
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
    // SECURITY (deferred to P11.8): a skill name is firm-global with no per-author
    // namespace, and `publish_skill_version` MERGEs by `name@version`, re-stamping
    // `source`/`guest`/`min_clearance` in place. So an L3 caller can re-publish an
    // existing name and overwrite another author's edition (provenance stays honest —
    // the new `source` is the host-stamped caller — and *existing* binds are
    // unaffected since `define_skill` snapshots the clearance floor). The publish
    // **trust gate** (dual-gate lead review before a name is authoritative/invocable
    // by others) is the explicit subject of P11.8 [HITL]; this slice keeps publishing
    // open and gated only by the L3 floor.
    let manifest = parse_skill_md(&body.skill_md).map_err(invalid_skill_md)?;
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
    // Resolve the edition: a specific version if asked, else the latest published.
    let edition = match body.version.as_deref() {
        Some(v) => list_skill_versions(&state.store, &body.skill_name)?
            .into_iter()
            .find(|r| r.version == v),
        None => latest_skill_version(&state.store, &body.skill_name)?,
    }
    .ok_or_else(|| {
        let what = match &body.version {
            Some(v) => format!("skill {}@{v}", body.skill_name),
            None => format!("skill {}", body.skill_name),
        };
        ApiError::new(StatusCode::NOT_FOUND, "not_found", what)
    })?;
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
