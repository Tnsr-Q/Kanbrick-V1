// ============================================================================
// KANBRICK-V1 SYNTHETIC FINANCIALS — valuation guest input (#45, ADR-0004)
// ============================================================================
// An OPTIONAL, demo-only dataset: one (:FinancialSnapshot) per portfolio company,
// linked (c)-[:HAS_FINANCIALS]->(f). Every snapshot is tagged
// `source_tag: "SYNTHETIC"` so it can never be mistaken for real financials — the
// valuation guest surfaces a warning when it values off synthetic data, and a
// caller can always override with real figures in the request payload.
//
// Kept separate from `kanbrick_seed_data.cypher` so the canonical firm graph's
// node/edge counts (Phase 1/4) are unchanged; loaded only where valuation is used.
//
// Numbers are plausible for each segment but invented. All amounts in USD.
// ----------------------------------------------------------------------------

// --- Testing & Lab Services (TLS) ---
CREATE (f_jmts:FinancialSnapshot {company_ref: "JMTS", quarter: "2026-Q1", revenue: 28000000, ebitda: 4200000, fcf: 3100000, growth_rate: 0.08, net_debt: 8000000, source_tag: "SYNTHETIC"});
CREATE (f_mcon:FinancialSnapshot {company_ref: "MCON", quarter: "2026-Q1", revenue: 16000000, ebitda: 2400000, fcf: 1700000, growth_rate: 0.06, net_debt: 4500000, source_tag: "SYNTHETIC"});
CREATE (f_aag:FinancialSnapshot {company_ref: "AAG", quarter: "2026-Q1", revenue: 21000000, ebitda: 3600000, fcf: 2600000, growth_rate: 0.10, net_debt: 5200000, source_tag: "SYNTHETIC"});
CREATE (f_lti:FinancialSnapshot {company_ref: "LTI", quarter: "2026-Q1", revenue: 45000000, ebitda: 8100000, fcf: 5400000, growth_rate: 0.12, net_debt: 12000000, source_tag: "SYNTHETIC"});
CREATE (f_ats:FinancialSnapshot {company_ref: "ATS", quarter: "2026-Q1", revenue: 19000000, ebitda: 3000000, fcf: 2100000, growth_rate: 0.07, net_debt: 4800000, source_tag: "SYNTHETIC"});

// --- Industrial Distribution (IND) ---
CREATE (f_keep:FinancialSnapshot {company_ref: "KEEP", quarter: "2026-Q1", revenue: 62000000, ebitda: 6200000, fcf: 4100000, growth_rate: 0.05, net_debt: 15000000, source_tag: "SYNTHETIC"});
CREATE (f_asi:FinancialSnapshot {company_ref: "ASI", quarter: "2026-Q1", revenue: 24000000, ebitda: 2900000, fcf: 1900000, growth_rate: 0.04, net_debt: 6000000, source_tag: "SYNTHETIC"});

// --- Manufacturing (MFG) ---
CREATE (f_dfpg:FinancialSnapshot {company_ref: "DFPG", quarter: "2026-Q1", revenue: 38000000, ebitda: 5700000, fcf: 3800000, growth_rate: 0.09, net_debt: 11000000, source_tag: "SYNTHETIC"});

// --- Strategic Programs (STR) ---
CREATE (f_bwk:FinancialSnapshot {company_ref: "BWK", quarter: "2026-Q1", revenue: 5000000, ebitda: 600000, fcf: 350000, growth_rate: 0.15, net_debt: 0, source_tag: "SYNTHETIC"});

// ----------------------------------------------------------------------------
// (c)-[:HAS_FINANCIALS]->(f) links, joined on the snapshot's company_ref.
// ----------------------------------------------------------------------------
MATCH (c:Company {company_id: "JMTS"}), (f:FinancialSnapshot {company_ref: "JMTS"}) CREATE (c)-[:HAS_FINANCIALS]->(f);
MATCH (c:Company {company_id: "MCON"}), (f:FinancialSnapshot {company_ref: "MCON"}) CREATE (c)-[:HAS_FINANCIALS]->(f);
MATCH (c:Company {company_id: "AAG"}), (f:FinancialSnapshot {company_ref: "AAG"}) CREATE (c)-[:HAS_FINANCIALS]->(f);
MATCH (c:Company {company_id: "LTI"}), (f:FinancialSnapshot {company_ref: "LTI"}) CREATE (c)-[:HAS_FINANCIALS]->(f);
MATCH (c:Company {company_id: "ATS"}), (f:FinancialSnapshot {company_ref: "ATS"}) CREATE (c)-[:HAS_FINANCIALS]->(f);
MATCH (c:Company {company_id: "KEEP"}), (f:FinancialSnapshot {company_ref: "KEEP"}) CREATE (c)-[:HAS_FINANCIALS]->(f);
MATCH (c:Company {company_id: "ASI"}), (f:FinancialSnapshot {company_ref: "ASI"}) CREATE (c)-[:HAS_FINANCIALS]->(f);
MATCH (c:Company {company_id: "DFPG"}), (f:FinancialSnapshot {company_ref: "DFPG"}) CREATE (c)-[:HAS_FINANCIALS]->(f);
MATCH (c:Company {company_id: "BWK"}), (f:FinancialSnapshot {company_ref: "BWK"}) CREATE (c)-[:HAS_FINANCIALS]->(f);
