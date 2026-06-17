//! # kanbrick-guest-valuation
//!
//! WASM business guest: discounted-cash-flow valuation for portfolio companies
//! (issue #45, per the operator decision in ADR-0004).
//!
//! * **Hybrid financials**: the caller may supply financials in the request
//!   (authority); otherwise the guest reads the graph's `FinancialSnapshot`
//!   (default). Every report records which source was used, and warns when the
//!   data is `SYNTHETIC`.
//! * **DCF**: a 5-year FCF projection discounted at the WACC plus a Gordon-growth
//!   terminal value; presets `standard` (10% WACC / 2.5% terminal) and
//!   `conservative` (12% / 2.0%), any parameter overridable per request.
//! * **Revenue-multiple cross-check** against a segment-default EV/Revenue.
//! * Requires **L3+** ([`REQUIRED_CLEARANCE`]); financials are gated company
//!   detail, so a caller can only value a company they may see.
//!
//! The pure model ([`compute_valuation`]) is unit-tested natively; the `wasm32`
//! entrypoint fetches financials/company context via the SDK (ADR-0004).

use kanbrick_core::ClearanceLevel;
use serde::{Deserialize, Serialize};

/// Minimum clearance required to run a valuation.
pub const REQUIRED_CLEARANCE: ClearanceLevel = ClearanceLevel::L3;

/// The financial inputs a DCF needs (request payload or graph snapshot).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Financials {
    /// Annual revenue.
    pub revenue: f64,
    /// EBITDA.
    pub ebitda: f64,
    /// Free cash flow (base year).
    pub fcf: f64,
    /// Expected annual growth rate (e.g. `0.10`).
    pub growth_rate: f64,
    /// Net debt (subtracted from enterprise value for equity value).
    pub net_debt: f64,
}

/// Where the financials used for a valuation came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum FinancialsSource {
    /// Supplied by the caller in the request (authoritative).
    UserProvided {
        /// Free-text provenance (e.g. `"Q2-2026 board deck"`).
        note: String,
    },
    /// Read from the graph's `FinancialSnapshot`.
    GraphDefault {
        /// The snapshot's `source_tag` (e.g. `"SYNTHETIC"`).
        source_tag: String,
    },
}

/// A named preset of DCF assumptions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    /// 10% WACC, 2.5% terminal growth (the default).
    #[default]
    Standard,
    /// 12% WACC, 2.0% terminal growth.
    Conservative,
}

/// The resolved DCF parameters used for a run (preset + any overrides).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DcfParameters {
    /// The preset these started from.
    pub preset: Preset,
    /// Discount rate (WACC).
    pub wacc: f64,
    /// Terminal (perpetuity) growth rate.
    pub terminal_growth_rate: f64,
    /// Explicit FCF projection horizon, in years.
    pub projection_years: u32,
    /// Blended tax rate (recorded for transparency).
    pub tax_rate: f64,
    /// D&A as a fraction of revenue (recorded for transparency).
    pub d_and_a_pct_of_revenue: f64,
    /// Capex as a fraction of revenue (recorded for transparency).
    pub capex_pct_of_revenue: f64,
    /// Net-working-capital as a fraction of revenue (recorded for transparency).
    pub nwc_pct_of_revenue: f64,
}

impl DcfParameters {
    /// The base parameters for a preset.
    pub fn from_preset(preset: Preset) -> Self {
        let (wacc, terminal_growth_rate) = match preset {
            Preset::Standard => (0.10, 0.025),
            Preset::Conservative => (0.12, 0.020),
        };
        DcfParameters {
            preset,
            wacc,
            terminal_growth_rate,
            projection_years: 5,
            tax_rate: 0.25,
            d_and_a_pct_of_revenue: 0.035,
            capex_pct_of_revenue: 0.025,
            nwc_pct_of_revenue: 0.08,
        }
    }
}

/// Per-request scenario: a preset plus optional overrides for any parameter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
pub struct Scenario {
    /// Which preset to start from (default [`Preset::Standard`]).
    #[serde(default)]
    pub preset: Preset,
    /// Override the discount rate.
    pub wacc: Option<f64>,
    /// Override the terminal growth rate.
    pub terminal_growth_rate: Option<f64>,
    /// Override the projection horizon.
    pub projection_years: Option<u32>,
}

impl Scenario {
    /// Resolve to concrete [`DcfParameters`]: preset, then overrides on top.
    pub fn resolve(&self) -> DcfParameters {
        let mut p = DcfParameters::from_preset(self.preset);
        if let Some(w) = self.wacc {
            p.wacc = w;
        }
        if let Some(g) = self.terminal_growth_rate {
            p.terminal_growth_rate = g;
        }
        if let Some(y) = self.projection_years {
            p.projection_years = y;
        }
        p
    }
}

/// EV-derived multiples for a valuation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Multiples {
    /// Enterprise value / revenue.
    pub ev_revenue: f64,
    /// Enterprise value / EBITDA.
    pub ev_ebitda: f64,
}

/// The valuation result (#45).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValuationReport {
    /// Company code.
    pub company_id: String,
    /// Company name.
    pub company_name: String,
    /// Where the financials came from.
    pub financials_source: FinancialsSource,
    /// The DCF assumptions used.
    pub parameters: DcfParameters,
    /// DCF enterprise value.
    pub enterprise_value: f64,
    /// DCF equity value (enterprise value − net debt).
    pub equity_value: f64,
    /// Revenue-multiple cross-check (revenue × segment EV/Revenue).
    pub revenue_multiple_valuation: f64,
    /// EV-derived multiples.
    pub multiples: Multiples,
    /// Comparable peers (same segment) used for the cross-check.
    pub comparable_peers: Vec<String>,
    /// Confidence in the result (0–1): lower for synthetic data.
    pub confidence: f64,
    /// Any warnings (e.g. synthetic data).
    pub warnings: Vec<String>,
}

/// Default EV/Revenue multiple for a segment (revenue-multiple cross-check).
pub fn segment_revenue_multiple(segment: &str) -> f64 {
    match segment {
        "Testing & Lab Services" => 3.0,
        "Industrial Distribution" => 1.5,
        "Manufacturing" => 2.0,
        "Strategic Programs" => 2.5,
        _ => 2.0,
    }
}

/// The non-financial context a valuation needs about the subject company.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompanyContext {
    /// Company code.
    pub company_id: String,
    /// Company name.
    pub name: String,
    /// Owning segment name.
    pub segment: String,
    /// Other companies in the same segment (comparable peers).
    pub peers: Vec<String>,
}

/// Compute a DCF + revenue-multiple valuation. Pure and deterministic.
pub fn compute_valuation(
    company: &CompanyContext,
    financials: &Financials,
    source: FinancialsSource,
    params: &DcfParameters,
) -> ValuationReport {
    // Discounted explicit free-cash-flow projection.
    let mut pv_fcf = 0.0;
    let mut last_fcf = financials.fcf;
    for year in 1..=params.projection_years {
        last_fcf = financials.fcf * (1.0 + financials.growth_rate).powi(year as i32);
        pv_fcf += last_fcf / (1.0 + params.wacc).powi(year as i32);
    }

    // Gordon-growth terminal value, discounted back from the final year. Guard the
    // degenerate case where terminal growth ≥ WACC (the perpetuity diverges).
    let pv_terminal = if params.wacc > params.terminal_growth_rate {
        let terminal = last_fcf * (1.0 + params.terminal_growth_rate)
            / (params.wacc - params.terminal_growth_rate);
        terminal / (1.0 + params.wacc).powi(params.projection_years as i32)
    } else {
        0.0
    };

    let enterprise_value = pv_fcf + pv_terminal;
    let equity_value = enterprise_value - financials.net_debt;

    let multiples = Multiples {
        ev_revenue: safe_div(enterprise_value, financials.revenue),
        ev_ebitda: safe_div(enterprise_value, financials.ebitda),
    };
    let revenue_multiple_valuation =
        financials.revenue * segment_revenue_multiple(&company.segment);

    let mut warnings = Vec::new();
    let confidence = match &source {
        FinancialsSource::UserProvided { .. } => 0.85,
        FinancialsSource::GraphDefault { source_tag } => {
            if source_tag.eq_ignore_ascii_case("SYNTHETIC") {
                warnings.push(
                    "valuation uses SYNTHETIC demo financials — not for investment decisions"
                        .to_string(),
                );
                0.5
            } else {
                0.7
            }
        }
    };
    if params.terminal_growth_rate >= params.wacc {
        warnings.push(
            "terminal growth ≥ WACC: terminal value omitted (perpetuity diverges)".to_string(),
        );
    }

    ValuationReport {
        company_id: company.company_id.clone(),
        company_name: company.name.clone(),
        financials_source: source,
        parameters: *params,
        enterprise_value,
        equity_value,
        revenue_multiple_valuation,
        multiples,
        comparable_peers: company.peers.clone(),
        confidence,
        warnings,
    }
}

/// Divide, returning 0.0 for a zero denominator (avoids NaN/inf in a report).
fn safe_div(numerator: f64, denominator: f64) -> f64 {
    if denominator == 0.0 {
        0.0
    } else {
        numerator / denominator
    }
}

// ---------------------------------------------------------------------------
// WASM entrypoint (ADR-0004): resolve financials (payload override or graph),
// fetch company context, compute the valuation.
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
mod entrypoint {
    use super::*;
    use kanbrick_guest_sdk as sdk;
    use sdk::serde_json::Value;
    use sdk::{GraphQuery, GuestRequest, GuestResponse, LogLevel};

    fn num(row: &Value, key: &str) -> Option<f64> {
        row.get(key).and_then(Value::as_f64)
    }
    fn text<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
        row.get(key).and_then(|v| v.as_str())
    }

    /// Fetch the subject company's public identity + same-segment peers.
    fn company_context(company_id: &str) -> sdk::Result<Option<CompanyContext>> {
        let rows = sdk::query_graph(
            &GraphQuery::new(
                "MATCH (c:Company {company_id: $id}) RETURN c.company_id, c.name, c.segment",
            )
            .param("id", company_id.to_string()),
        )?;
        let Some(row) = rows.rows.first() else {
            return Ok(None);
        };
        let name = text(row, "name").unwrap_or_default().to_string();
        let segment = text(row, "segment").unwrap_or_default().to_string();

        // Comparable peers: other companies in the same segment (public roster).
        let peer_rows = sdk::query_graph(&GraphQuery::new(
            "MATCH (c:Company) RETURN c.company_id, c.name, c.segment",
        ))?;
        let peers = peer_rows
            .rows
            .iter()
            .filter(|r| text(r, "segment") == Some(segment.as_str()))
            .filter_map(|r| text(r, "company_id").map(String::from))
            .filter(|id| id != company_id)
            .collect();

        Ok(Some(CompanyContext {
            company_id: company_id.to_string(),
            name,
            segment,
            peers,
        }))
    }

    /// Resolve financials: payload override (authority) else the graph snapshot.
    /// The snapshot query projects `company_id`, so it is gated company detail —
    /// a caller can only read financials for a company they may see (ADR-0005).
    fn resolve_financials(
        company_id: &str,
        request: &GuestRequest,
    ) -> sdk::Result<Option<(Financials, FinancialsSource)>> {
        if let Some(f) = request.payload.get("financials") {
            if !f.is_null() {
                let financials: Financials = sdk::serde_json::from_value(f.clone())
                    .map_err(|e| sdk::Error::InvalidInput(format!("financials: {e}")))?;
                let note = request
                    .payload
                    .get("financials_note")
                    .and_then(|v| v.as_str())
                    .unwrap_or("caller-provided")
                    .to_string();
                return Ok(Some((financials, FinancialsSource::UserProvided { note })));
            }
        }

        let rows = sdk::query_graph(
            &GraphQuery::new(
                "MATCH (c:Company {company_id: $id})-[:HAS_FINANCIALS]->(f:FinancialSnapshot) \
                 RETURN c.company_id, f.revenue, f.ebitda, f.fcf, f.growth_rate, f.net_debt, \
                 f.source_tag",
            )
            .param("id", company_id.to_string()),
        )?;
        let Some(row) = rows.rows.first() else {
            return Ok(None);
        };
        let financials = Financials {
            revenue: num(row, "revenue").unwrap_or(0.0),
            ebitda: num(row, "ebitda").unwrap_or(0.0),
            fcf: num(row, "fcf").unwrap_or(0.0),
            growth_rate: num(row, "growth_rate").unwrap_or(0.0),
            net_debt: num(row, "net_debt").unwrap_or(0.0),
        };
        let source = FinancialsSource::GraphDefault {
            source_tag: text(row, "source_tag").unwrap_or("unknown").to_string(),
        };
        Ok(Some((financials, source)))
    }

    fn handle(request: GuestRequest) -> sdk::Result<GuestResponse> {
        sdk::log(LogLevel::Info, "valuation: started");
        let ctx = sdk::firm_context()?;
        if ctx.clearance < REQUIRED_CLEARANCE {
            return Err(sdk::Error::AccessDenied {
                required: REQUIRED_CLEARANCE,
                actual: ctx.clearance,
            });
        }

        let company_id = request
            .payload
            .get("company_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| sdk::Error::InvalidInput("missing company_id".to_string()))?
            .to_string();

        let Some(company) = company_context(&company_id)? else {
            return Err(sdk::Error::NotFound(format!("company {company_id}")));
        };

        let scenario: Scenario = request
            .payload
            .get("scenario")
            .and_then(|v| sdk::serde_json::from_value(v.clone()).ok())
            .unwrap_or_default();
        let params = scenario.resolve();

        let Some((financials, source)) = resolve_financials(&company_id, &request)? else {
            return Err(sdk::Error::NotFound(format!(
                "no financials for {company_id}; provide them in the request or seed the graph"
            )));
        };

        let report = compute_valuation(&company, &financials, source, &params);

        // Announce completion so subscribers (e.g. the reporting guest) can react
        // (#46). Emitting is best-effort and never fails the valuation.
        let _ = sdk::emit(&sdk::Event::with_payload(
            "valuation.completed",
            sdk::serde_json::json!({
                "company_id": report.company_id,
                "enterprise_value": report.enterprise_value,
            }),
        ));

        sdk::serde_json::to_value(&report)
            .map(GuestResponse::new)
            .map_err(|e| sdk::Error::Internal(format!("encoding report: {e}")))
    }

    sdk::guest_entrypoint!(handle);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jmts() -> CompanyContext {
        CompanyContext {
            company_id: "JMTS".into(),
            name: "JM Test Systems".into(),
            segment: "Testing & Lab Services".into(),
            peers: vec!["MCON".into(), "AAG".into()],
        }
    }

    fn jmts_financials() -> Financials {
        Financials {
            revenue: 28_000_000.0,
            ebitda: 4_200_000.0,
            fcf: 3_100_000.0,
            growth_rate: 0.08,
            net_debt: 8_000_000.0,
        }
    }

    #[test]
    fn dcf_produces_plausible_non_zero_values() {
        let params = Scenario::default().resolve();
        let report = compute_valuation(
            &jmts(),
            &jmts_financials(),
            FinancialsSource::GraphDefault {
                source_tag: "SYNTHETIC".into(),
            },
            &params,
        );

        // Non-zero, and equity = enterprise − net debt.
        assert!(report.enterprise_value > 0.0);
        assert!((report.equity_value - (report.enterprise_value - 8_000_000.0)).abs() < 1.0);
        // A growing FCF discounted at 10% should value well above one year's FCF.
        assert!(report.enterprise_value > jmts_financials().fcf);
        // Revenue multiple uses the segment default (TLS = 3.0×).
        assert!((report.revenue_multiple_valuation - 28_000_000.0 * 3.0).abs() < 1.0);
        // EV/revenue multiple is computed.
        assert!(
            (report.multiples.ev_revenue - report.enterprise_value / 28_000_000.0).abs() < 1e-6
        );
        // Synthetic data lowers confidence and raises a warning.
        assert!(report.confidence < 0.6);
        assert!(report.warnings.iter().any(|w| w.contains("SYNTHETIC")));
        assert_eq!(report.comparable_peers, vec!["MCON", "AAG"]);
    }

    #[test]
    fn presets_and_overrides_resolve() {
        let standard = Scenario::default().resolve();
        assert_eq!(standard.preset, Preset::Standard);
        assert!((standard.wacc - 0.10).abs() < 1e-9);
        assert!((standard.terminal_growth_rate - 0.025).abs() < 1e-9);

        let conservative = Scenario {
            preset: Preset::Conservative,
            ..Default::default()
        }
        .resolve();
        assert!((conservative.wacc - 0.12).abs() < 1e-9);
        assert!((conservative.terminal_growth_rate - 0.020).abs() < 1e-9);

        // An override wins over the preset.
        let custom = Scenario {
            preset: Preset::Standard,
            wacc: Some(0.09),
            projection_years: Some(7),
            ..Default::default()
        }
        .resolve();
        assert!((custom.wacc - 0.09).abs() < 1e-9);
        assert_eq!(custom.projection_years, 7);
    }

    #[test]
    fn conservative_values_below_standard() {
        let std_report = compute_valuation(
            &jmts(),
            &jmts_financials(),
            FinancialsSource::UserProvided {
                note: "deck".into(),
            },
            &Scenario::default().resolve(),
        );
        let cons_report = compute_valuation(
            &jmts(),
            &jmts_financials(),
            FinancialsSource::UserProvided {
                note: "deck".into(),
            },
            &Scenario {
                preset: Preset::Conservative,
                ..Default::default()
            }
            .resolve(),
        );
        // A higher discount rate + lower terminal growth yields a lower valuation.
        assert!(cons_report.enterprise_value < std_report.enterprise_value);
        // Caller-provided financials carry higher confidence and no synthetic warning.
        assert!(std_report.confidence > 0.8);
        assert!(std_report.warnings.is_empty());
    }

    #[test]
    fn user_provided_financials_are_authoritative_in_confidence() {
        let report = compute_valuation(
            &jmts(),
            &jmts_financials(),
            FinancialsSource::UserProvided {
                note: "Q2-2026 board deck".into(),
            },
            &Scenario::default().resolve(),
        );
        match report.financials_source {
            FinancialsSource::UserProvided { note } => assert_eq!(note, "Q2-2026 board deck"),
            _ => panic!("expected user-provided source"),
        }
        assert!(report.warnings.is_empty());
    }
}
