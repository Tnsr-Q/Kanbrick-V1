// ============================================================================
// KANBRICK-V1 SEED DATA — 12-Person Firm + 9 Portfolio Companies
// ============================================================================
// Generated for SparrowDB graph layer (Cypher-compatible)
// Clearance Levels: L1 (Support) → L5 (Admin)
// 12 Named Roles across 5 tiers | 9 Portfolio Companies | 4 Segments
// ============================================================================

// ---------------------------------------------------------------------------
// CONSTRAINTS & INDEXES
// ---------------------------------------------------------------------------
// Schema DDL (uniqueness constraints + lookup indexes) is owned by migration
// v001 (`kanbrick-store::schema::schema_statements`) and applied before this
// data file is loaded. SparrowDB supports the legacy DDL grammar
// (`CREATE CONSTRAINT ON (n:Label) ASSERT n.prop IS UNIQUE`) rather than the
// newer `... IF NOT EXISTS FOR ... REQUIRE ...` form, so the statements that
// previously lived here have been moved into the migration to keep this file
// loadable as pure data. See issue #8 (HITL).

// ---------------------------------------------------------------------------
// SEGMENT NODES
// ---------------------------------------------------------------------------
CREATE (seg_testing:Segment {
  name: "Testing & Lab Services",
  code: "TLS",
  description: "Testing, laboratory, and analytical services portfolio"
});

CREATE (seg_industrial:Segment {
  name: "Industrial Distribution",
  code: "IND",
  description: "Industrial supply chain and distribution portfolio"
});

CREATE (seg_manufacturing:Segment {
  name: "Manufacturing",
  code: "MFG",
  description: "Precision manufacturing and fluid power portfolio"
});

CREATE (seg_strategic:Segment {
  name: "Strategic Programs",
  code: "STR",
  description: "Client outreach, partnerships, and growth programs"
});

// ---------------------------------------------------------------------------
// PORTFOLIO COMPANIES (9 total)
// ---------------------------------------------------------------------------

// --- Testing & Lab Services ---
CREATE (jmtest:Company {
  company_id: "JMTS",
  name: "JM Test Systems",
  legal_name: "JM Test Systems, Inc.",
  segment: "Testing & Lab Services",
  status: "active",
  acquired_year: 2021,
  hq_state: "TX",
  description: "Calibration, test equipment sales, and rental services"
});

CREATE (marine:Company {
  company_id: "MCON",
  name: "Marine Concepts",
  legal_name: "Marine Concepts, LLC",
  segment: "Testing & Lab Services",
  status: "active",
  acquired_year: 2022,
  hq_state: "FL",
  description: "Marine composite manufacturing and engineering"
});

CREATE (aag:Company {
  company_id: "AAG",
  name: "Alchemy Analytical Group",
  legal_name: "Alchemy Analytical Group, LLC",
  segment: "Testing & Lab Services",
  status: "active",
  acquired_year: 2022,
  hq_state: "CA",
  description: "Environmental and analytical laboratory services"
});

CREATE (lti:Company {
  company_id: "LTI",
  name: "Laboratory Testing Inc.",
  legal_name: "Laboratory Testing Inc.",
  segment: "Testing & Lab Services",
  status: "active",
  acquired_year: 2023,
  hq_state: "PA",
  description: "Materials testing and failure analysis laboratory"
});

CREATE (assured:Company {
  company_id: "ATS",
  name: "Assured Testing Services",
  legal_name: "Assured Testing Services, LLC",
  segment: "Testing & Lab Services",
  status: "active",
  acquired_year: 2023,
  hq_state: "OH",
  description: "Non-destructive testing and inspection services"
});

// --- Industrial Distribution ---
CREATE (keep:Company {
  company_id: "KEEP",
  name: "Keep Supply",
  legal_name: "Keep Supply Co.",
  segment: "Industrial Distribution",
  status: "active",
  acquired_year: 2022,
  hq_state: "OK",
  description: "Industrial MRO and safety supply distribution"
});

CREATE (alscale:Company {
  company_id: "ASI",
  name: "Alabama Scale & Instrument",
  legal_name: "Alabama Scale & Instrument Co., Inc.",
  segment: "Industrial Distribution",
  status: "active",
  acquired_year: 2024,
  hq_state: "AL",
  description: "Precision weighing and measurement instrument sales and service"
});

// --- Manufacturing ---
CREATE (depatie:Company {
  company_id: "DFPG",
  name: "Depatie Fluid Power Group",
  legal_name: "Depatie Fluid Power Group, Inc.",
  segment: "Manufacturing",
  status: "active",
  acquired_year: 2023,
  hq_state: "MI",
  description: "Hydraulic and pneumatic fluid power systems manufacturing"
});

// --- Strategic Programs ---
CREATE (bwk:Company {
  company_id: "BWK",
  name: "Build with Kanbrick",
  legal_name: "Build with Kanbrick Program",
  segment: "Strategic Programs",
  status: "active",
  acquired_year: 2020,
  hq_state: "MO",
  description: "Client outreach and owner-operator partnership development program"
});

// ---------------------------------------------------------------------------
// SEGMENT ← COMPANY RELATIONSHIPS
// ---------------------------------------------------------------------------
// SparrowDB supports `MATCH (a {inline}), (b {inline}) CREATE (a)-[:R]->(b)`
// but not `MATCH ... WHERE x IN [...] CREATE ...`, so the IN-list assignments
// are expanded into one inline-filtered statement per company. See issue #8.

// Testing & Lab Services (TLS): JMTS, MCON, AAG, LTI, ATS
MATCH (s:Segment {code: "TLS"}), (c:Company {company_id: "JMTS"}) CREATE (c)-[:BELONGS_TO_SEGMENT]->(s);
MATCH (s:Segment {code: "TLS"}), (c:Company {company_id: "MCON"}) CREATE (c)-[:BELONGS_TO_SEGMENT]->(s);
MATCH (s:Segment {code: "TLS"}), (c:Company {company_id: "AAG"}) CREATE (c)-[:BELONGS_TO_SEGMENT]->(s);
MATCH (s:Segment {code: "TLS"}), (c:Company {company_id: "LTI"}) CREATE (c)-[:BELONGS_TO_SEGMENT]->(s);
MATCH (s:Segment {code: "TLS"}), (c:Company {company_id: "ATS"}) CREATE (c)-[:BELONGS_TO_SEGMENT]->(s);

// Industrial Distribution (IND): KEEP, ASI
MATCH (s:Segment {code: "IND"}), (c:Company {company_id: "KEEP"}) CREATE (c)-[:BELONGS_TO_SEGMENT]->(s);
MATCH (s:Segment {code: "IND"}), (c:Company {company_id: "ASI"}) CREATE (c)-[:BELONGS_TO_SEGMENT]->(s);

// Manufacturing (MFG): DFPG
MATCH (s:Segment {code: "MFG"}), (c:Company {company_id: "DFPG"}) CREATE (c)-[:BELONGS_TO_SEGMENT]->(s);

// Strategic Programs (STR): BWK
MATCH (s:Segment {code: "STR"}), (c:Company {company_id: "BWK"}) CREATE (c)-[:BELONGS_TO_SEGMENT]->(s);


// ===========================================================================
// PERSON NODES — 12-Person Firm
// ===========================================================================

// ---------------------------------------------------------------------------
// L5 — ADMIN (Executive Leadership) — 2 people
// ---------------------------------------------------------------------------
CREATE (tracy:Person {
  full_name: "Tracy Britt Cool",
  first_name: "Tracy",
  last_name: "Britt Cool",
  email: "tracy.brittcool@kanbrick.com",
  title: "Chief Executive Officer",
  role: "CEO",
  clearance_level: "L5",
  clearance_label: "Admin",
  department: "Executive",
  status: "active"
});

CREATE (brian:Person {
  full_name: "Brian Humphrey",
  first_name: "Brian",
  last_name: "Humphrey",
  email: "brian.humphrey@kanbrick.com",
  title: "President",
  role: "President",
  clearance_level: "L5",
  clearance_label: "Admin",
  department: "Executive",
  status: "active"
});

// ---------------------------------------------------------------------------
// L4 — STRATEGIC LEADERSHIP — 4 people
// ---------------------------------------------------------------------------
CREATE (matt:Person {
  full_name: "Matt Berns",
  first_name: "Matt",
  last_name: "Berns",
  email: "matt.berns@kanbrick.com",
  title: "Chief Technology Officer",
  role: "CTO",
  clearance_level: "L4",
  clearance_label: "Strategic",
  department: "Technology",
  status: "active"
});

CREATE (andrea:Person {
  full_name: "Andrea Lewis",
  first_name: "Andrea",
  last_name: "Lewis",
  email: "andrea.lewis@kanbrick.com",
  title: "Chief Financial Officer",
  role: "CFO",
  clearance_level: "L4",
  clearance_label: "Strategic",
  department: "Finance",
  note: "Placeholder for A.L. initials",
  status: "active"
});

CREATE (marcus:Person {
  full_name: "Marcus Hall",
  first_name: "Marcus",
  last_name: "Hall",
  email: "marcus.hall@kanbrick.com",
  title: "Chief People Officer",
  role: "CPO",
  clearance_level: "L4",
  clearance_label: "Strategic",
  department: "People & Culture",
  note: "Placeholder for M.H. initials",
  status: "active"
});

CREATE (peter:Person {
  full_name: "Peter Nash",
  first_name: "Peter",
  last_name: "Nash",
  email: "peter.nash@kanbrick.com",
  title: "Chief Strategy Officer",
  role: "CSO",
  clearance_level: "L4",
  clearance_label: "Strategic",
  department: "Strategy",
  note: "Placeholder for P.N. initials",
  status: "active"
});

// ---------------------------------------------------------------------------
// L3 — OPERATIONAL LEADERS — 3 people
// ---------------------------------------------------------------------------
CREATE (tyler:Person {
  full_name: "Tyler Begemann",
  first_name: "Tyler",
  last_name: "Begemann",
  email: "tyler.begemann@kanbrick.com",
  title: "VP, Testing & Lab Services",
  role: "Segment Lead",
  clearance_level: "L3",
  clearance_label: "Operational",
  department: "Portfolio Operations",
  segment: "Testing & Lab Services",
  note: "Placeholder for T. Begemann",
  status: "active"
});

CREATE (blake:Person {
  full_name: "Blake Richardson",
  first_name: "Blake",
  last_name: "Richardson",
  email: "blake.richardson@kanbrick.com",
  title: "VP, Industrial Distribution & Manufacturing",
  role: "Segment Lead",
  clearance_level: "L3",
  clearance_label: "Operational",
  department: "Portfolio Operations",
  segment: "Industrial Distribution",
  note: "Placeholder for B.R. initials",
  status: "active"
});

CREATE (sloan:Person {
  full_name: "Sloan Allen",
  first_name: "Sloan",
  last_name: "Allen",
  email: "sloan.allen@kanbrick.com",
  title: "VP, Strategic Programs & Build with Kanbrick",
  role: "Segment Lead",
  clearance_level: "L3",
  clearance_label: "Operational",
  department: "Strategic Programs",
  segment: "Strategic Programs",
  status: "active"
});

// ---------------------------------------------------------------------------
// L2 — EXECUTION TEAM — 2 people
// ---------------------------------------------------------------------------
CREATE (samantha:Person {
  full_name: "Samantha Jordan",
  first_name: "Samantha",
  last_name: "Jordan",
  email: "samantha.jordan@kanbrick.com",
  title: "Senior Investment Analyst",
  role: "Senior Analyst",
  clearance_level: "L2",
  clearance_label: "Execution",
  department: "Business Development",
  note: "Placeholder for S.J. initials",
  status: "active"
});

CREATE (elena:Person {
  full_name: "Elena Ruiz",
  first_name: "Elena",
  last_name: "Ruiz",
  email: "elena.ruiz@kanbrick.com",
  title: "Portfolio Operations Analyst",
  role: "Analyst",
  clearance_level: "L2",
  clearance_label: "Execution",
  department: "Portfolio Operations",
  status: "active"
});

// ---------------------------------------------------------------------------
// L1 — SUPPORT STAFF — 1 person
// ---------------------------------------------------------------------------
CREATE (dana:Person {
  full_name: "Dana Prescott",
  first_name: "Dana",
  last_name: "Prescott",
  email: "dana.prescott@kanbrick.com",
  title: "Support Coordinator",
  role: "Support Coordinator",
  clearance_level: "L1",
  clearance_label: "Support",
  department: "Operations",
  status: "active"
});


// ===========================================================================
// ORG CHART — REPORTS_TO RELATIONSHIPS
// ===========================================================================

// --- L5: President reports to CEO ---
MATCH (brian:Person {email: "brian.humphrey@kanbrick.com"}),
      (tracy:Person {email: "tracy.brittcool@kanbrick.com"})
CREATE (brian)-[:REPORTS_TO {relationship: "direct"}]->(tracy);

// --- L4 → L5: All strategic leaders report to President ---
MATCH (p:Person {email: "matt.berns@kanbrick.com"}), (brian:Person {email: "brian.humphrey@kanbrick.com"}) CREATE (p)-[:REPORTS_TO {relationship: "direct"}]->(brian);
MATCH (p:Person {email: "andrea.lewis@kanbrick.com"}), (brian:Person {email: "brian.humphrey@kanbrick.com"}) CREATE (p)-[:REPORTS_TO {relationship: "direct"}]->(brian);
MATCH (p:Person {email: "marcus.hall@kanbrick.com"}), (brian:Person {email: "brian.humphrey@kanbrick.com"}) CREATE (p)-[:REPORTS_TO {relationship: "direct"}]->(brian);
MATCH (p:Person {email: "peter.nash@kanbrick.com"}), (brian:Person {email: "brian.humphrey@kanbrick.com"}) CREATE (p)-[:REPORTS_TO {relationship: "direct"}]->(brian);

// --- L3 → L4: Segment leads report to CSO (Peter Nash / P.N.) ---
MATCH (p:Person {email: "tyler.begemann@kanbrick.com"}), (peter:Person {email: "peter.nash@kanbrick.com"}) CREATE (p)-[:REPORTS_TO {relationship: "direct"}]->(peter);
MATCH (p:Person {email: "blake.richardson@kanbrick.com"}), (peter:Person {email: "peter.nash@kanbrick.com"}) CREATE (p)-[:REPORTS_TO {relationship: "direct"}]->(peter);
MATCH (p:Person {email: "sloan.allen@kanbrick.com"}), (peter:Person {email: "peter.nash@kanbrick.com"}) CREATE (p)-[:REPORTS_TO {relationship: "direct"}]->(peter);

// --- L2 → L3: Analysts report to operational leaders ---
// Samantha Jordan (S.J.) → Tyler Begemann (Testing & Lab lead, largest segment)
MATCH (samantha:Person {email: "samantha.jordan@kanbrick.com"}),
      (tyler:Person {email: "tyler.begemann@kanbrick.com"})
CREATE (samantha)-[:REPORTS_TO {relationship: "direct"}]->(tyler);

// Elena Ruiz → Blake Richardson (Industrial Distribution & Manufacturing lead)
MATCH (elena:Person {email: "elena.ruiz@kanbrick.com"}),
      (blake:Person {email: "blake.richardson@kanbrick.com"})
CREATE (elena)-[:REPORTS_TO {relationship: "direct"}]->(blake);

// --- L1 → L5: Support Coordinator reports to CEO ---
MATCH (dana:Person {email: "dana.prescott@kanbrick.com"}),
      (tracy:Person {email: "tracy.brittcool@kanbrick.com"})
CREATE (dana)-[:REPORTS_TO {relationship: "direct"}]->(tracy);


// ===========================================================================
// MANAGES — Person → Company portfolio oversight
// ===========================================================================

// CEO oversees entire portfolio
MATCH (tracy:Person {email: "tracy.brittcool@kanbrick.com"}), (c:Company)
CREATE (tracy)-[:MANAGES {scope: "executive_oversight"}]->(c);

// President oversees entire portfolio
MATCH (brian:Person {email: "brian.humphrey@kanbrick.com"}), (c:Company)
CREATE (brian)-[:MANAGES {scope: "operational_oversight"}]->(c);

// Tyler Begemann → Testing & Lab segment (5 companies)
MATCH (p:Person {email: "tyler.begemann@kanbrick.com"}), (c:Company {company_id: "JMTS"}) CREATE (p)-[:MANAGES {scope: "segment_lead"}]->(c);
MATCH (p:Person {email: "tyler.begemann@kanbrick.com"}), (c:Company {company_id: "MCON"}) CREATE (p)-[:MANAGES {scope: "segment_lead"}]->(c);
MATCH (p:Person {email: "tyler.begemann@kanbrick.com"}), (c:Company {company_id: "AAG"}) CREATE (p)-[:MANAGES {scope: "segment_lead"}]->(c);
MATCH (p:Person {email: "tyler.begemann@kanbrick.com"}), (c:Company {company_id: "LTI"}) CREATE (p)-[:MANAGES {scope: "segment_lead"}]->(c);
MATCH (p:Person {email: "tyler.begemann@kanbrick.com"}), (c:Company {company_id: "ATS"}) CREATE (p)-[:MANAGES {scope: "segment_lead"}]->(c);

// Blake Richardson → Industrial Distribution + Manufacturing segments
MATCH (p:Person {email: "blake.richardson@kanbrick.com"}), (c:Company {company_id: "KEEP"}) CREATE (p)-[:MANAGES {scope: "segment_lead"}]->(c);
MATCH (p:Person {email: "blake.richardson@kanbrick.com"}), (c:Company {company_id: "ASI"}) CREATE (p)-[:MANAGES {scope: "segment_lead"}]->(c);
MATCH (p:Person {email: "blake.richardson@kanbrick.com"}), (c:Company {company_id: "DFPG"}) CREATE (p)-[:MANAGES {scope: "segment_lead"}]->(c);

// Sloan Allen → Strategic Programs (Build with Kanbrick)
MATCH (p:Person {email: "sloan.allen@kanbrick.com"}), (c:Company {company_id: "BWK"})
CREATE (p)-[:MANAGES {scope: "program_lead"}]->(c);

// CFO → Financial oversight of all companies
MATCH (p:Person {email: "andrea.lewis@kanbrick.com"}), (c:Company)
CREATE (p)-[:MANAGES {scope: "financial_oversight"}]->(c);

// CTO → Technology oversight of all companies
MATCH (p:Person {email: "matt.berns@kanbrick.com"}), (c:Company)
CREATE (p)-[:MANAGES {scope: "technology_oversight"}]->(c);


// ===========================================================================
// VERIFICATION QUERIES (run after import to validate)
// ===========================================================================

// Count all persons by clearance level
// MATCH (p:Person) RETURN p.clearance_level AS level, count(p) AS count ORDER BY level DESC;
// Expected: L5=2, L4=4, L3=3, L2=2, L1=1 → Total 12

// Count all companies
// MATCH (c:Company) RETURN count(c);
// Expected: 9

// Validate org chart — everyone except CEO has a REPORTS_TO
// MATCH (p:Person) WHERE NOT (p)-[:REPORTS_TO]->() AND p.role <> 'CEO' RETURN p.full_name;
// Expected: empty result

// Show full org tree
// MATCH path = (leaf:Person)-[:REPORTS_TO*]->(root:Person {role: "CEO"})
// RETURN leaf.full_name, length(path) AS depth, [n IN nodes(path) | n.full_name] AS chain
// ORDER BY depth DESC;

// Verify all segments have at least one company
// MATCH (s:Segment)<-[:BELONGS_TO_SEGMENT]-(c:Company)
// RETURN s.name, count(c) AS company_count;
// Expected: TLS=5, IND=2, MFG=1, STR=1
