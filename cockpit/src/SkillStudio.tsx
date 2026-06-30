// Skill Studio (P11.6): the create-side of the skill/loop ecosystem in the app.
// Author + publish a SKILL.md, browse the scope-filtered library (with version
// history in a drawer), bind a published edition onto one of the caller's approved
// scopes, and build a loop of ordered steps (guest · provider · mcp-tool). The
// sections are organized into tabs so the studio isn't one long wall of panels.
// Authored loops then appear in the P11.7 LoopRunner to run and watch.
//
// Identity stays host-side (ADR-0016): every call goes through the host auth bridge
// (api.ts → Tauri → the bundled kanbrick-api sidecar); the webview supplies only the
// manifest text / names / ids — never a token, credential, or identity.
import { useEffect, useState } from "react";
import {
  bindSkill,
  createLoop,
  listScopes,
  listSkillReviews,
  listSkills,
  publishSkill,
  reviewSkill,
  skillHistory,
  type GrantedScopeView,
  type LoopStepSpec,
  type ProviderKind,
  type ReviewDecision,
  type SkillVersion,
  type ToolRef,
} from "./api";
import { useToast } from "./Toast";
import { SkeletonRows } from "./Skeleton";

/** A clearance level for the author form / kind selects. */
const CLEARANCES = ["L1", "L2", "L3", "L4", "L5"] as const;
/** The embedded business guests (a convenience datalist; any guest name is allowed). */
const KNOWN_GUESTS = ["reporting", "valuation", "compliance"] as const;
/** Provider kinds the run engine accepts (mirrors `kanbrick_providers::ProviderKind`). */
const PROVIDERS: ProviderKind[] = ["anthropic", "openai", "cerebras"];

/** Suggested models per provider for the loop builder's model picker. Rendered as a
 * `<datalist>` — suggestions, not a hard list, so a custom model id still works. */
const MODELS_BY_PROVIDER: Record<ProviderKind, string[]> = {
  anthropic: ["claude-opus-4-8", "claude-sonnet-4-6", "claude-haiku-4-5-20251001"],
  openai: ["gpt-4o", "gpt-4o-mini"],
  cerebras: ["llama-3.3-70b", "llama-3.1-8b"],
};

/** The studio's tabs. `review` is only offered to eligible reviewers (P11.8). */
type Tab = "author" | "library" | "bind" | "build" | "review";

const TABS: { key: Tab; label: string }[] = [
  { key: "author", label: "Author" },
  { key: "library", label: "Library" },
  { key: "bind", label: "Bind" },
  { key: "build", label: "Build" },
];

/** A null/absent `review_status` reads as `pending` (fail-closed), matching the server. */
function reviewLabel(status?: string | null): string {
  return status ?? "pending";
}

/** The three loop-step kinds the builder can compose. */
type StepKind = "guest" | "provider" | "mcp-tool";

/** A loop-builder row — local UI state compiled into a {@link LoopStepSpec} on submit. */
type BuilderStep = {
  skillName: string;
  scopeId: string;
  kind: StepKind;
  provider: ProviderKind;
  model: string;
  tool: string;
  /** Raw JSON text for an MCP tool step's static args; "" = no args. */
  toolArgs: string;
};

/** Parse a step's tool-args text. Empty → valid with no args. Otherwise it must parse
 * to a JSON **object** (the server requires it); anything else is invalid. */
function parseToolArgs(raw: string): {
  valid: boolean;
  value?: Record<string, unknown>;
} {
  const trimmed = raw.trim();
  if (trimmed === "") return { valid: true };
  try {
    const parsed: unknown = JSON.parse(trimmed);
    if (parsed !== null && typeof parsed === "object" && !Array.isArray(parsed)) {
      return { valid: true, value: parsed as Record<string, unknown> };
    }
  } catch {
    // Not valid JSON — fall through to invalid.
  }
  return { valid: false };
}

/** Compose a SKILL.md from the frontmatter form + body — the inverse of the Rust
 * `SkillManifest::to_skill_md`, matching the `parse_skill_md` frontmatter shape. */
function composeSkillMd(f: {
  name: string;
  version: string;
  guest: string;
  clearance: string;
  description: string;
  body: string;
}): string {
  return (
    `---\n` +
    `name: ${f.name}\n` +
    `version: ${f.version}\n` +
    `guest: ${f.guest}\n` +
    `clearance: ${f.clearance}\n` +
    `description: ${f.description}\n` +
    `---\n\n` +
    `${f.body}\n`
  );
}

export default function SkillStudio() {
  const toast = useToast();
  const [tab, setTab] = useState<Tab>("author");

  // ── Library ───────────────────────────────────────────────────────────────
  const [skills, setSkills] = useState<SkillVersion[]>([]);
  const [skillsLoaded, setSkillsLoaded] = useState(false);
  const [historyFor, setHistoryFor] = useState<string | null>(null);
  const [history, setHistory] = useState<SkillVersion[]>([]);

  const refreshSkills = () => {
    listSkills()
      .then(setSkills)
      .catch((e) => toast.error(String(e)))
      .finally(() => setSkillsLoaded(true));
  };
  useEffect(refreshSkills, []);

  const onHistory = async (name: string) => {
    if (historyFor === name) {
      setHistoryFor(null);
      setHistory([]);
      return;
    }
    try {
      setHistory(await skillHistory(name));
      setHistoryFor(name);
    } catch (e) {
      toast.error(String(e));
    }
  };

  const closeHistory = () => {
    setHistoryFor(null);
    setHistory([]);
  };

  // Switching tabs also closes the history drawer — it belongs to the Library tab,
  // so it shouldn't float over the others.
  const selectTab = (next: Tab) => {
    setTab(next);
    if (historyFor) closeHistory();
  };

  // ── Review queue (P11.8) — shown only to eligible reviewers (the queue is
  // L4-gated server-side; a non-reviewer's load fails, so the tab stays hidden).
  const [reviews, setReviews] = useState<SkillVersion[]>([]);
  const [canReview, setCanReview] = useState(false);

  const refreshReviews = () => {
    listSkillReviews()
      .then((rs) => {
        setReviews(rs);
        setCanReview(true);
      })
      .catch(() => setCanReview(false));
  };
  useEffect(refreshReviews, []);

  const onReview = async (
    name: string,
    version: string,
    decision: ReviewDecision,
  ) => {
    try {
      await reviewSkill(name, version, decision);
      toast.success(
        `Skill ${name}@${version} ${decision === "approve" ? "approved" : "rejected"}.`,
      );
      refreshReviews();
      refreshSkills();
    } catch (e) {
      toast.error(String(e));
    }
  };

  // ── Author ──────────────────────────────────────────────────────────────────
  const [aName, setAName] = useState("");
  const [aVersion, setAVersion] = useState("1.0.0");
  const [aGuest, setAGuest] = useState<string>(KNOWN_GUESTS[0]);
  const [aClearance, setAClearance] = useState<string>("L1");
  const [aDescription, setADescription] = useState("");
  const [aBody, setABody] = useState("");
  const [publishing, setPublishing] = useState(false);

  const canPublish =
    aName.trim() !== "" && aVersion.trim() !== "" && aGuest.trim() !== "";

  const onPublish = async () => {
    if (!canPublish || publishing) return;
    setPublishing(true);
    try {
      const skillMd = composeSkillMd({
        name: aName.trim(),
        version: aVersion.trim(),
        guest: aGuest.trim(),
        clearance: aClearance,
        description: aDescription.trim(),
        body: aBody.trim() || `# ${aName.trim()}`,
      });
      const published = await publishSkill(skillMd);
      toast.success(
        `Published ${published.skill_name}@${published.version} ` +
          `(seq ${published.seq}, by ${published.source}).`,
      );
      refreshSkills();
    } catch (e) {
      toast.error(String(e));
    } finally {
      setPublishing(false);
    }
  };

  // ── Project scopes (shared by Bind + the loop builder) ───────────────────────
  const [project, setProject] = useState("");
  const [scopes, setScopes] = useState<GrantedScopeView[]>([]);
  const [loadingScopes, setLoadingScopes] = useState(false);

  const onLoadScopes = async () => {
    if (project.trim() === "" || loadingScopes) return;
    setLoadingScopes(true);
    try {
      const found = await listScopes(project.trim());
      setScopes(found);
      if (found.length === 0) {
        toast.info(`No active scopes for "${project.trim()}".`);
      }
    } catch (e) {
      toast.error(String(e));
    } finally {
      setLoadingScopes(false);
    }
  };

  // ── Bind ──────────────────────────────────────────────────────────────────
  const [bindName, setBindName] = useState("");
  const [bindScope, setBindScope] = useState("");
  const [bindVersion, setBindVersion] = useState("");
  const [binding, setBinding] = useState(false);

  const onBind = async () => {
    if (binding || bindName === "" || bindScope === "") return;
    setBinding(true);
    try {
      const bound = await bindSkill(
        bindScope,
        bindName,
        bindVersion.trim() || undefined,
      );
      toast.success(
        `Bound ${bound.name} (floor ${bound.required_clearance}) onto scope ${bound.scope_id}.`,
      );
    } catch (e) {
      toast.error(String(e));
    } finally {
      setBinding(false);
    }
  };

  // ── Loop builder ────────────────────────────────────────────────────────────
  const [loopName, setLoopName] = useState("");
  const [steps, setSteps] = useState<BuilderStep[]>([]);
  const [creating, setCreating] = useState(false);

  const addStep = () =>
    setSteps((s) => [
      ...s,
      {
        skillName: "",
        scopeId: "",
        kind: "guest",
        provider: "anthropic",
        model: "",
        tool: "",
        toolArgs: "",
      },
    ]);

  const updateStep = (i: number, patch: Partial<BuilderStep>) =>
    setSteps((s) => s.map((step, j) => (j === i ? { ...step, ...patch } : step)));

  const removeStep = (i: number) =>
    setSteps((s) => s.filter((_, j) => j !== i));

  const stepValid = (s: BuilderStep): boolean =>
    s.skillName !== "" &&
    s.scopeId !== "" &&
    (s.kind !== "provider" || s.model.trim() !== "") &&
    (s.kind !== "mcp-tool" ||
      (s.tool.trim() !== "" && parseToolArgs(s.toolArgs).valid));

  const canCreate =
    loopName.trim() !== "" && steps.length > 0 && steps.every(stepValid);

  const onCreateLoop = async () => {
    if (!canCreate || creating) return;
    setCreating(true);
    try {
      const specs: LoopStepSpec[] = steps.map((s) => {
        const base: LoopStepSpec = { skill_name: s.skillName, scope_id: s.scopeId };
        if (s.kind === "provider") {
          return {
            ...base,
            provider_ref: { provider: s.provider, model: s.model.trim() },
          };
        }
        if (s.kind === "mcp-tool") {
          const tool_ref: ToolRef = { tool: s.tool.trim() };
          const args = parseToolArgs(s.toolArgs).value;
          if (args) tool_ref.args = args;
          return { ...base, tool_ref };
        }
        return base;
      });
      const created = await createLoop(loopName.trim(), specs);
      toast.success(
        `Created loop "${created.name}" with ${created.steps.length} step` +
          `${created.steps.length === 1 ? "" : "s"} — run it in the Loops panel.`,
      );
      setSteps([]);
      setLoopName("");
    } catch (e) {
      toast.error(String(e));
    } finally {
      setCreating(false);
    }
  };

  // The project scope loader is shared by the Bind and Build tabs (one `scopes`
  // state); rendering it in both keeps each tab self-sufficient.
  const scopeLoader = (
    <>
      <div className="scope-loader">
        <input
          value={project}
          placeholder="project (e.g. valuation-jmts)"
          onChange={(e) => setProject(e.target.value)}
        />
        <button
          className="btn-secondary"
          onClick={onLoadScopes}
          disabled={loadingScopes || project.trim() === ""}
        >
          {loadingScopes ? "Loading…" : "Load scopes"}
        </button>
      </div>
      {scopes.length > 0 && (
        <p className="hint">
          {scopes.length} active scope{scopes.length === 1 ? "" : "s"} for{" "}
          <code>{project.trim()}</code>.
        </p>
      )}
    </>
  );

  return (
    <section className="card skill-studio">
      <h1>Skill Studio</h1>
      <p className="subtitle">Author skills, bind them onto scopes, and build loops</p>

      <div className="studio-tabs" role="tablist" aria-label="Skill Studio sections">
        {TABS.map((t) => (
          <button
            key={t.key}
            type="button"
            role="tab"
            aria-selected={tab === t.key}
            className={`studio-tab${tab === t.key ? " is-active" : ""}`}
            onClick={() => selectTab(t.key)}
          >
            {t.label}
          </button>
        ))}
        {canReview && (
          <button
            type="button"
            role="tab"
            aria-selected={tab === "review"}
            className={`studio-tab${tab === "review" ? " is-active" : ""}`}
            onClick={() => selectTab("review")}
          >
            Review{reviews.length > 0 ? ` (${reviews.length})` : ""}
          </button>
        )}
      </div>

      {/* ── Author ─────────────────────────────────────────────────────────── */}
      {tab === "author" && (
        <div className="panel">
          <h2>Author a skill</h2>
          <div className="studio-form">
            <label className="field">
              <span>Name</span>
              <input
                value={aName}
                placeholder="daily-report"
                onChange={(e) => setAName(e.target.value)}
              />
            </label>
            <label className="field">
              <span>Version</span>
              <input value={aVersion} onChange={(e) => setAVersion(e.target.value)} />
            </label>
            <label className="field">
              <span>Guest</span>
              <input
                value={aGuest}
                list="known-guests"
                onChange={(e) => setAGuest(e.target.value)}
              />
              <datalist id="known-guests">
                {KNOWN_GUESTS.map((g) => (
                  <option key={g} value={g} />
                ))}
              </datalist>
            </label>
            <label className="field">
              <span>Clearance floor</span>
              <select
                value={aClearance}
                onChange={(e) => setAClearance(e.target.value)}
              >
                {CLEARANCES.map((c) => (
                  <option key={c} value={c}>
                    {c}
                  </option>
                ))}
              </select>
            </label>
          </div>
          <label className="field">
            <span>Description</span>
            <input
              value={aDescription}
              placeholder="One-line summary"
              onChange={(e) => setADescription(e.target.value)}
            />
          </label>
          <label className="field">
            <span>Body (markdown)</span>
            <textarea
              rows={4}
              value={aBody}
              placeholder="What the skill does, when to use it…"
              onChange={(e) => setABody(e.target.value)}
            />
          </label>
          <button
            className="btn-secondary"
            onClick={onPublish}
            disabled={!canPublish || publishing}
          >
            {publishing ? "Publishing…" : "Publish skill"}
          </button>
        </div>
      )}

      {/* ── Library ────────────────────────────────────────────────────────── */}
      {tab === "library" && (
        <div className="panel">
          <div className="panel-head">
            <h2>Library</h2>
            <button className="link-btn" onClick={refreshSkills}>
              Refresh
            </button>
          </div>
          {!skillsLoaded ? (
            <SkeletonRows rows={3} />
          ) : skills.length === 0 ? (
            <p className="hint">No published skills yet — author one above.</p>
          ) : (
            <ul className="skill-list">
              {skills.map((s) => (
                <li className="skill-row" key={s.skill_name}>
                  <span className="skill-name">{s.skill_name}</span>
                  <span className="chip">{s.guest}</span>
                  <span className={`badge badge-${s.min_clearance.toLowerCase()}`}>
                    <span className="badge-level">{s.min_clearance}</span>
                  </span>
                  <span className={`chip review-${reviewLabel(s.review_status)}`}>
                    {reviewLabel(s.review_status)}
                  </span>
                  <span className="skill-version">v{s.version}</span>
                  <button
                    className="link-btn"
                    onClick={() => onHistory(s.skill_name)}
                  >
                    {historyFor === s.skill_name ? "Hide history" : "History"}
                  </button>
                  {s.description && (
                    <span className="skill-desc">{s.description}</span>
                  )}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      {/* ── Review queue (reviewers only, P11.8) ───────────────────────────── */}
      {tab === "review" && canReview && (
        <div className="panel">
          <div className="panel-head">
            <h2>Review queue</h2>
            <button className="link-btn" onClick={refreshReviews}>
              Refresh
            </button>
          </div>
          {reviews.length === 0 ? (
            <p className="hint">No skills awaiting review.</p>
          ) : (
            <ul className="skill-list">
              {reviews.map((r) => (
                <li className="skill-row" key={`${r.skill_name}@${r.version}#${r.seq}`}>
                  <span className="skill-name">{r.skill_name}</span>
                  <span className="skill-version">v{r.version}</span>
                  <span className="chip">{r.guest}</span>
                  <span className={`badge badge-${r.min_clearance.toLowerCase()}`}>
                    <span className="badge-level">{r.min_clearance}</span>
                  </span>
                  <span className="review-actions">
                    <button
                      className="btn-secondary"
                      onClick={() => onReview(r.skill_name, r.version, "approve")}
                    >
                      Approve
                    </button>
                    <button
                      className="btn-secondary danger"
                      onClick={() => onReview(r.skill_name, r.version, "reject")}
                    >
                      Reject
                    </button>
                  </span>
                  <span className="skill-desc">by {r.source}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}

      {/* ── Bind onto a scope ──────────────────────────────────────────────── */}
      {tab === "bind" && (
        <div className="panel">
          <h2>Bind onto a scope</h2>
          {scopeLoader}
          <div className="studio-form">
            <label className="field">
              <span>Skill</span>
              <select value={bindName} onChange={(e) => setBindName(e.target.value)}>
                <option value="">Select a skill…</option>
                {skills.map((s) => (
                  <option key={s.skill_name} value={s.skill_name}>
                    {s.skill_name}
                  </option>
                ))}
              </select>
            </label>
            <label className="field">
              <span>Scope</span>
              <select value={bindScope} onChange={(e) => setBindScope(e.target.value)}>
                <option value="">Select a scope…</option>
                {scopes.map((sc) => (
                  <option key={sc.id} value={sc.id}>
                    {sc.project} · {sc.status} · {sc.id.slice(0, 8)}
                  </option>
                ))}
              </select>
            </label>
            <label className="field">
              <span>Version (optional)</span>
              <input
                value={bindVersion}
                placeholder="latest"
                onChange={(e) => setBindVersion(e.target.value)}
              />
            </label>
          </div>
          <button
            className="btn-secondary"
            onClick={onBind}
            disabled={binding || bindName === "" || bindScope === ""}
          >
            {binding ? "Binding…" : "Bind skill"}
          </button>
        </div>
      )}

      {/* ── Loop builder ───────────────────────────────────────────────────── */}
      {tab === "build" && (
        <div className="panel">
          <h2>Build a loop</h2>
          {scopeLoader}
          <label className="field">
            <span>Loop name</span>
            <input
              value={loopName}
              placeholder="nightly-report"
              onChange={(e) => setLoopName(e.target.value)}
            />
          </label>

          {steps.length === 0 ? (
            <p className="hint">
              Add ordered steps; each names a bound skill + scope and is a guest,
              provider, or MCP-tool step.
              {scopes.length === 0 && " Load your scopes above first."}
            </p>
          ) : (
            <ol className="builder-list">
              {steps.map((s, i) => {
                const argsValid = parseToolArgs(s.toolArgs).valid;
                return (
                  <li className="builder-row" key={i}>
                    <span className="step-pos">{i + 1}</span>
                    <select
                      aria-label="skill"
                      value={s.skillName}
                      onChange={(e) => updateStep(i, { skillName: e.target.value })}
                    >
                      <option value="">skill…</option>
                      {skills.map((sk) => (
                        <option key={sk.skill_name} value={sk.skill_name}>
                          {sk.skill_name}
                        </option>
                      ))}
                    </select>
                    <select
                      aria-label="scope"
                      value={s.scopeId}
                      onChange={(e) => updateStep(i, { scopeId: e.target.value })}
                    >
                      <option value="">scope…</option>
                      {scopes.map((sc) => (
                        <option key={sc.id} value={sc.id}>
                          {sc.id.slice(0, 8)}
                        </option>
                      ))}
                    </select>
                    <select
                      aria-label="kind"
                      value={s.kind}
                      onChange={(e) =>
                        updateStep(i, { kind: e.target.value as StepKind })
                      }
                    >
                      <option value="guest">guest</option>
                      <option value="provider">provider</option>
                      <option value="mcp-tool">mcp-tool</option>
                    </select>
                    {s.kind === "provider" && (
                      <>
                        <select
                          aria-label="provider"
                          value={s.provider}
                          onChange={(e) => {
                            const provider = e.target.value as ProviderKind;
                            // Keep a hand-typed custom model; only swap to the new
                            // provider's default when the current model is empty or
                            // was the previous provider's suggested default.
                            const keepModel =
                              s.model.trim() !== "" &&
                              !MODELS_BY_PROVIDER[s.provider].includes(s.model);
                            updateStep(i, {
                              provider,
                              model: keepModel
                                ? s.model
                                : MODELS_BY_PROVIDER[provider][0],
                            });
                          }}
                        >
                          {PROVIDERS.map((p) => (
                            <option key={p} value={p}>
                              {p}
                            </option>
                          ))}
                        </select>
                        <input
                          aria-label="model"
                          list={`models-${i}`}
                          value={s.model}
                          placeholder="model"
                          onChange={(e) => updateStep(i, { model: e.target.value })}
                        />
                        <datalist id={`models-${i}`}>
                          {MODELS_BY_PROVIDER[s.provider].map((m) => (
                            <option key={m} value={m} />
                          ))}
                        </datalist>
                      </>
                    )}
                    {s.kind === "mcp-tool" && (
                      <>
                        <input
                          aria-label="tool"
                          value={s.tool}
                          placeholder="tool (e.g. web.search)"
                          onChange={(e) => updateStep(i, { tool: e.target.value })}
                        />
                        <input
                          aria-label="tool args (JSON object)"
                          className={argsValid ? "" : "is-invalid"}
                          value={s.toolArgs}
                          placeholder='args, e.g. {"q":"…"}'
                          onChange={(e) => updateStep(i, { toolArgs: e.target.value })}
                        />
                      </>
                    )}
                    <button
                      className="link-btn remove"
                      onClick={() => removeStep(i)}
                      aria-label="remove step"
                    >
                      ✕
                    </button>
                  </li>
                );
              })}
            </ol>
          )}

          <div className="builder-actions">
            <button className="btn-secondary" onClick={addStep}>
              + Add step
            </button>
            <button
              className="btn-secondary"
              onClick={onCreateLoop}
              disabled={!canCreate || creating}
            >
              {creating ? "Creating…" : "Create loop"}
            </button>
          </div>
        </div>
      )}

      {/* Skill version history — a drawer that slides in from the right. */}
      {historyFor && (
        <>
          <div className="drawer-overlay" onClick={closeHistory} />
          <aside
            className="drawer"
            role="dialog"
            aria-label={`Version history for ${historyFor}`}
          >
            <div className="drawer-head">
              <h2 className="drawer-title">{historyFor} · history</h2>
              <button
                className="link-btn"
                onClick={closeHistory}
                aria-label="Close history"
              >
                ✕
              </button>
            </div>
            {history.length === 0 ? (
              <p className="hint">No prior versions.</p>
            ) : (
              <ol className="drawer-history">
                {history.map((h) => (
                  <li key={`${h.skill_name}@${h.version}#${h.seq}`}>
                    <span className="skill-version">v{h.version}</span>
                    <span className="drawer-history-meta">
                      seq {h.seq} · {h.source}
                    </span>
                  </li>
                ))}
              </ol>
            )}
          </aside>
        </>
      )}
    </section>
  );
}
