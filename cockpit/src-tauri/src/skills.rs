//! Skill-authoring + library + scope-binding IPC (P11.6).
//!
//! The create-side of the skill/loop ecosystem, mirroring [`crate::loops`]: publish a
//! `SKILL.md` edition into the versioned catalogue (`POST /me/skills`), browse it
//! (`GET /me/skills`, `GET /me/skills/{name}`), bind a published edition onto one of
//! the caller's approved scopes (`POST /me/scopes/{id}/skills`), and list those scopes
//! (`GET /me/scopes?project=…`) to pick one to bind onto or reference in a loop step.
//!
//! Identity stays host-authoritative (ADR-0016): every call attaches the Bearer from
//! the host-held [`Session`](crate::auth::Session) via the auth bridge; the webview
//! supplies only the `SKILL.md` text / names / ids. The `source`/author of a published
//! skill is host-stamped server-side from the authenticated identity, never the body.
//! The DTOs mirror `kanbrick-api`'s `SkillVersionRecord` / `SkillDto` / `GrantedScopeDto`
//! 1:1 (and the TS types in `src/api.ts`).

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::auth::{authed_get, authed_post, Session};

/// A published skill edition, mirroring `kanbrick-api`'s `SkillVersionRecord`.
/// `min_clearance` is the serialized `ClearanceLevel` (`"L1"`..`"L5"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillVersion {
    pub skill_name: String,
    pub version: String,
    pub guest: String,
    pub min_clearance: String,
    pub description: String,
    pub source: String,
    pub created_at: String,
    pub seq: i64,
    /// Publish trust-gate state (P11.8): `"pending"|"approved"|"rejected"`; `None`
    /// (a pre-P11.8 edition / absent) is treated as pending by the UI.
    #[serde(default)]
    pub review_status: Option<String>,
    /// The lead who decided the review; `None`/empty until decided.
    #[serde(default)]
    pub reviewed_by: Option<String>,
    /// When the review was decided; `None`/empty until decided.
    #[serde(default)]
    pub reviewed_at: Option<String>,
}

/// A skill edition bound onto a scope, mirroring `kanbrick-api`'s `SkillDto`.
/// `required_clearance` is the serialized `ClearanceLevel` run-time floor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundSkill {
    pub id: String,
    pub name: String,
    pub scope_id: String,
    pub guest: String,
    pub required_clearance: String,
}

/// One of the caller's granted scopes, mirroring `kanbrick-api`'s `GrantedScopeDto`.
/// `status` is `"pending"|"active"|"expired"|"revoked"`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantedScopeView {
    pub id: String,
    pub project: String,
    pub requester: String,
    pub granted_by: String,
    pub granted_persons: Vec<String>,
    pub granted_companies: Vec<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    pub status: String,
}

/// Body for `POST /me/skills` — the raw `SKILL.md` source. The author/`source` is
/// host-stamped server-side; the webview supplies only the manifest text.
#[derive(Serialize)]
struct PublishBody {
    skill_md: String,
}

/// Body for `POST /me/scopes/{id}/skills` — which published edition to bind. `version`
/// is omitted to bind the latest published edition.
#[derive(Serialize)]
struct BindBody {
    skill_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

/// Body for `POST /me/skill-reviews/{name}/{version}` — a lead's decision (P11.8).
#[derive(Serialize)]
struct ReviewBody {
    decision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

/// A 401 clears the host session so the UI falls back to login.
fn session_expired(app: &AppHandle) -> String {
    app.state::<Session>().clear();
    "session expired — please sign in again".to_string()
}

/// `invoke('publish_skill', { skillMd })` — publish a `SKILL.md` edition into the
/// versioned catalogue via `POST /me/skills`. The webview supplies only the manifest
/// text; the host injects the Bearer and the server host-stamps the author. A
/// malformed manifest surfaces the server's `400 invalid_skill_md` message.
#[tauri::command]
pub async fn publish_skill(app: AppHandle, skill_md: String) -> Result<SkillVersion, String> {
    let response = authed_post(&app, "/me/skills", &PublishBody { skill_md }).await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(session_expired(&app));
    }
    if !response.status().is_success() {
        // Surface the server's structured message (e.g. invalid_skill_md) when present.
        let status = response.status();
        let message = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
            .unwrap_or_else(|| format!("could not publish skill ({status})"));
        return Err(message);
    }
    response
        .json::<SkillVersion>()
        .await
        .map_err(|e| format!("unexpected publish response: {e}"))
}

/// `invoke('list_skills')` — browse the catalogue (latest edition of every skill) via
/// `GET /me/skills`.
#[tauri::command]
pub async fn list_skills(app: AppHandle) -> Result<Vec<SkillVersion>, String> {
    let response = authed_get(&app, "/me/skills").await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(session_expired(&app));
    }
    if !response.status().is_success() {
        return Err(format!("could not load skills ({})", response.status()));
    }
    response
        .json::<Vec<SkillVersion>>()
        .await
        .map_err(|e| format!("unexpected skills response: {e}"))
}

/// `invoke('skill_history', { name })` — every published edition of one skill,
/// oldest→newest, via `GET /me/skills/{name}`.
#[tauri::command]
pub async fn skill_history(app: AppHandle, name: String) -> Result<Vec<SkillVersion>, String> {
    let response = authed_get(&app, &format!("/me/skills/{name}")).await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(session_expired(&app));
    }
    if !response.status().is_success() {
        return Err(format!(
            "could not load skill history ({})",
            response.status()
        ));
    }
    response
        .json::<Vec<SkillVersion>>()
        .await
        .map_err(|e| format!("unexpected skill-history response: {e}"))
}

/// `invoke('bind_skill', { scopeId, skillName, version })` — bind a published edition
/// onto a scope via `POST /me/scopes/{id}/skills`. `version` is optional (omit to bind
/// the latest). Gated server-side on scope ownership; the webview supplies only ids.
#[tauri::command]
pub async fn bind_skill(
    app: AppHandle,
    scope_id: String,
    skill_name: String,
    version: Option<String>,
) -> Result<BoundSkill, String> {
    let body = BindBody {
        skill_name,
        version,
    };
    let response = authed_post(&app, &format!("/me/scopes/{scope_id}/skills"), &body).await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(session_expired(&app));
    }
    if !response.status().is_success() {
        let status = response.status();
        let message = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
            .unwrap_or_else(|| format!("could not bind skill ({status})"));
        return Err(message);
    }
    response
        .json::<BoundSkill>()
        .await
        .map_err(|e| format!("unexpected bind response: {e}"))
}

/// `invoke('list_scopes', { project })` — the caller's active grants for a project via
/// `GET /me/scopes?project=…`, to pick a scope to bind onto or reference in a loop
/// step. `project` is a server-validated kebab identifier, interpolated like the other
/// path/query params in this crate.
#[tauri::command]
pub async fn list_scopes(app: AppHandle, project: String) -> Result<Vec<GrantedScopeView>, String> {
    let response = authed_get(&app, &format!("/me/scopes?project={project}")).await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(session_expired(&app));
    }
    if !response.status().is_success() {
        return Err(format!("could not load scopes ({})", response.status()));
    }
    response
        .json::<Vec<GrantedScopeView>>()
        .await
        .map_err(|e| format!("unexpected scopes response: {e}"))
}

/// `invoke('list_skill_reviews')` — the pending publish-review queue (P11.8) via
/// `GET /me/skill-reviews`. L4-gated server-side; a non-reviewer gets a `403`, which
/// the UI uses to hide the reviewer panel.
#[tauri::command]
pub async fn list_skill_reviews(app: AppHandle) -> Result<Vec<SkillVersion>, String> {
    let response = authed_get(&app, "/me/skill-reviews").await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(session_expired(&app));
    }
    if !response.status().is_success() {
        return Err(format!(
            "could not load the review queue ({})",
            response.status()
        ));
    }
    response
        .json::<Vec<SkillVersion>>()
        .await
        .map_err(|e| format!("unexpected reviews response: {e}"))
}

/// `invoke('review_skill', { name, version, decision, reason })` — approve or reject a
/// published edition via `POST /me/skill-reviews/{name}/{version}` (P11.8). The host
/// injects the Bearer; the eligibility check (reviewer over the author, no self-review)
/// is enforced server-side. Returns the updated edition.
#[tauri::command]
pub async fn review_skill(
    app: AppHandle,
    name: String,
    version: String,
    decision: String,
    reason: Option<String>,
) -> Result<SkillVersion, String> {
    let body = ReviewBody { decision, reason };
    let response = authed_post(&app, &format!("/me/skill-reviews/{name}/{version}"), &body).await?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(session_expired(&app));
    }
    if !response.status().is_success() {
        let status = response.status();
        let message = response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
            .unwrap_or_else(|| format!("could not record the review ({status})"));
        return Err(message);
    }
    response
        .json::<SkillVersion>()
        .await
        .map_err(|e| format!("unexpected review response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_version_mirrors_the_api_json() {
        // The exact shape kanbrick-api's `GET /me/skills` emits for one edition.
        let json = serde_json::json!({
            "skill_name": "daily-report",
            "version": "1.0.0",
            "guest": "reporting",
            "min_clearance": "L1",
            "description": "a loop step",
            "source": "elena.ruiz@kanbrick.com",
            "created_at": "2026-06-29T00:00:00+00:00",
            "seq": 3,
            "review_status": "approved",
            "reviewed_by": "peter.nash@kanbrick.com",
            "reviewed_at": "2026-06-29T01:00:00+00:00"
        });
        let s: SkillVersion = serde_json::from_value(json).unwrap();
        assert_eq!(s.skill_name, "daily-report");
        assert_eq!(s.min_clearance, "L1");
        assert_eq!(s.source, "elena.ruiz@kanbrick.com");
        assert_eq!(s.seq, 3);
        assert_eq!(s.review_status.as_deref(), Some("approved"));
        assert_eq!(s.reviewed_by.as_deref(), Some("peter.nash@kanbrick.com"));
    }

    #[test]
    fn skill_version_tolerates_an_absent_review_status() {
        // A pre-P11.8 edition omits the review fields; they default to None.
        let json = serde_json::json!({
            "skill_name": "legacy",
            "version": "1.0.0",
            "guest": "valuation",
            "min_clearance": "L3",
            "description": "",
            "source": "elena.ruiz@kanbrick.com",
            "created_at": "2026-06-29T00:00:00+00:00",
            "seq": 1
        });
        let s: SkillVersion = serde_json::from_value(json).unwrap();
        assert_eq!(s.review_status, None);
        assert_eq!(s.reviewed_by, None);
    }

    #[test]
    fn bound_skill_mirrors_the_api_json() {
        let json = serde_json::json!({
            "id": "SK1",
            "name": "daily-report",
            "scope_id": "S1",
            "guest": "reporting",
            "required_clearance": "L1"
        });
        let b: BoundSkill = serde_json::from_value(json).unwrap();
        assert_eq!(b.name, "daily-report");
        assert_eq!(b.scope_id, "S1");
        assert_eq!(b.required_clearance, "L1");
    }

    #[test]
    fn granted_scope_mirrors_the_api_json_including_optional_expiry() {
        // An active grant with an expiry; granted_persons may be empty.
        let json = serde_json::json!({
            "id": "S1",
            "project": "valuation-jmts",
            "requester": "elena.ruiz@kanbrick.com",
            "granted_by": "peter.nash@kanbrick.com",
            "granted_persons": [],
            "granted_companies": ["JMTS"],
            "expires_at": "2026-07-29T00:00:00+00:00",
            "status": "active"
        });
        let g: GrantedScopeView = serde_json::from_value(json).unwrap();
        assert_eq!(g.id, "S1");
        assert_eq!(g.project, "valuation-jmts");
        assert_eq!(g.status, "active");
        assert_eq!(g.granted_companies, vec!["JMTS".to_string()]);
        assert_eq!(g.expires_at.as_deref(), Some("2026-07-29T00:00:00+00:00"));
    }

    #[test]
    fn bind_body_omits_an_absent_version() {
        let latest = serde_json::to_value(BindBody {
            skill_name: "daily-report".to_string(),
            version: None,
        })
        .unwrap();
        assert_eq!(latest, serde_json::json!({ "skill_name": "daily-report" }));
        let pinned = serde_json::to_value(BindBody {
            skill_name: "daily-report".to_string(),
            version: Some("1.0.0".to_string()),
        })
        .unwrap();
        assert_eq!(pinned["version"], "1.0.0");
    }
}
