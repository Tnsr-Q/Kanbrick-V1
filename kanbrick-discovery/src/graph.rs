//! [`DiscoveryGraph`] — the firm graph loaded into graphify-rs (issue #30).
//!
//! `from_store` reads every Person/Company/Segment and their REPORTS_TO /
//! MANAGES / BELONGS_TO_SEGMENT edges out of SparrowDB and builds **one**
//! [`KnowledgeGraph`] (the in-memory copy decided in ADR-0003) plus firm-typed
//! indices the analysis modules compute over. graphify-rs's backing graph is
//! undirected, but each edge stores its own direction, so we also derive the
//! directed reporting/management adjacency here.
//!
//! Node ids are namespaced (`person:<email>`, `company:<code>`,
//! `segment:<code>`) so the graphify-id ↔ firm-id mapping is total and
//! unambiguous, and the firm kind is carried in each node's `extra["kind"]`
//! (the firm has no matching `NodeType` variant).

use std::collections::{BTreeMap, HashMap};

use graphify_core::confidence::Confidence;
use graphify_core::graph::KnowledgeGraph;
use graphify_core::model::{GraphEdge, GraphNode, NodeType};
use kanbrick_core::{Error, Result};
use kanbrick_store::{CompanyNode, Params, PersonNode, SegmentNode, Store};
use serde::Deserialize;

use crate::model::ManageScope;

/// Namespaced graphify node id for a person.
pub(crate) fn person_node_id(email: &str) -> String {
    format!("person:{email}")
}

/// Namespaced graphify node id for a company.
pub(crate) fn company_node_id(code: &str) -> String {
    format!("company:{code}")
}

/// Namespaced graphify node id for a segment.
pub(crate) fn segment_node_id(code: &str) -> String {
    format!("segment:{code}")
}

/// One `MANAGES` edge: a person managing a company with a given scope.
#[derive(Debug, Clone)]
pub(crate) struct ManagesEdge {
    pub person_email: String,
    pub company_id: String,
    pub scope: ManageScope,
}

/// The firm graph, loaded into graphify-rs and indexed by firm type.
#[derive(Debug)]
pub struct DiscoveryGraph {
    /// The graphify knowledge graph (canonical node/edge container).
    graph: KnowledgeGraph,
    /// Persons by email (sorted for deterministic iteration).
    persons: BTreeMap<String, PersonNode>,
    /// Companies by `company_id`.
    companies: BTreeMap<String, CompanyNode>,
    /// Segments by `code`.
    segments: BTreeMap<String, SegmentNode>,
    /// Each person's direct manager (email → manager email).
    manager_of: HashMap<String, String>,
    /// Each manager's direct reports (email → report emails, sorted).
    direct_reports_of: HashMap<String, Vec<String>>,
    /// All `MANAGES` edges.
    manages: Vec<ManagesEdge>,
    /// Each company's segment (company_id → segment code).
    company_segment: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct EmailRow {
    email: String,
}

#[derive(Debug, Deserialize)]
struct CodeRow {
    code: String,
}

#[derive(Debug, Deserialize)]
struct ManageRow {
    company_id: String,
    #[serde(default)]
    scope: Option<String>,
}

impl DiscoveryGraph {
    /// Load the firm graph out of `store` (a privileged, full-graph read).
    pub fn from_store(store: &Store) -> Result<Self> {
        let persons = load_persons(store)?;
        let companies = load_companies(store)?;
        let segments = load_segments(store)?;

        let mut manager_of = HashMap::new();
        let mut direct_reports_of: HashMap<String, Vec<String>> = HashMap::new();
        let mut manages = Vec::new();
        let mut company_segment = HashMap::new();

        // REPORTS_TO + MANAGES are read per person (single- or distinct-named
        // columns), sidestepping the row-mapper's prefix-strip collision that a
        // two-`email`-column edge query would hit (ADR-0001, handoff §4).
        for email in persons.keys() {
            if let Some(mgr) = load_manager(store, email)? {
                direct_reports_of
                    .entry(mgr.clone())
                    .or_default()
                    .push(email.clone());
                manager_of.insert(email.clone(), mgr);
            }
            for row in load_manages(store, email)? {
                let scope = row
                    .scope
                    .as_deref()
                    .map(ManageScope::from_raw)
                    .unwrap_or_else(|| ManageScope::Other("unknown".into()));
                manages.push(ManagesEdge {
                    person_email: email.clone(),
                    company_id: row.company_id,
                    scope,
                });
            }
        }
        for code in companies.keys() {
            if let Some(seg) = load_company_segment(store, code)? {
                company_segment.insert(code.clone(), seg);
            }
        }
        for reports in direct_reports_of.values_mut() {
            reports.sort();
        }

        let graph = build_knowledge_graph(
            &persons,
            &companies,
            &segments,
            &manager_of,
            &manages,
            &company_segment,
        )?;

        Ok(DiscoveryGraph {
            graph,
            persons,
            companies,
            segments,
            manager_of,
            direct_reports_of,
            manages,
            company_segment,
        })
    }

    // ---- structural counts -------------------------------------------------

    /// Number of nodes in the graphify representation.
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    /// Number of edges in the graphify representation.
    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }

    /// Borrow the underlying graphify graph (used by the influence module).
    pub fn knowledge_graph(&self) -> &KnowledgeGraph {
        &self.graph
    }

    // ---- firm accessors ----------------------------------------------------

    /// Look up a person by exact email.
    pub fn person(&self, email: &str) -> Option<&PersonNode> {
        self.persons.get(email)
    }

    /// Look up a company by exact `company_id`.
    pub fn company(&self, company_id: &str) -> Option<&CompanyNode> {
        self.companies.get(company_id)
    }

    /// Look up a segment by exact `code`.
    pub fn segment(&self, code: &str) -> Option<&SegmentNode> {
        self.segments.get(code)
    }

    /// All persons, sorted by email.
    pub fn persons(&self) -> impl Iterator<Item = &PersonNode> {
        self.persons.values()
    }

    /// All companies, sorted by `company_id`.
    pub fn companies(&self) -> impl Iterator<Item = &CompanyNode> {
        self.companies.values()
    }

    /// All segments, sorted by `code`.
    pub fn segments(&self) -> impl Iterator<Item = &SegmentNode> {
        self.segments.values()
    }

    /// The email of a person's direct manager, if any.
    pub fn manager_of(&self, email: &str) -> Option<&str> {
        self.manager_of.get(email).map(String::as_str)
    }

    /// A manager's direct reports (emails, sorted). Empty for a leaf.
    pub fn direct_reports(&self, email: &str) -> &[String] {
        self.direct_reports_of
            .get(email)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The chain of managers strictly above `email`, lowest first (their
    /// manager, then their manager's manager, … up to the CEO). Empty for the
    /// CEO. Guarded against cycles.
    pub fn ancestors(&self, email: &str) -> Vec<String> {
        let mut chain = Vec::new();
        let mut current = email.to_string();
        while let Some(mgr) = self.manager_of.get(&current) {
            if chain.iter().any(|m| m == mgr) {
                break; // defensive: never loop on a malformed cycle
            }
            chain.push(mgr.clone());
            current = mgr.clone();
        }
        chain
    }

    /// All `MANAGES` edges.
    pub(crate) fn manages(&self) -> &[ManagesEdge] {
        &self.manages
    }

    /// The segment code a company belongs to, if known.
    pub fn segment_of_company(&self, company_id: &str) -> Option<&str> {
        self.company_segment.get(company_id).map(String::as_str)
    }

    // ---- flexible identifier resolution ------------------------------------

    /// Resolve a loose person identifier — exact email, the email's local part
    /// (e.g. `"samantha.jordan"`), or a case-insensitive full-name match — to a
    /// loaded person. Returns the canonical email.
    pub fn resolve_person(&self, ident: &str) -> Option<&PersonNode> {
        if let Some(p) = self.persons.get(ident) {
            return Some(p);
        }
        let needle = ident.trim();
        self.persons.values().find(|p| {
            let local = p.email.split('@').next().unwrap_or(&p.email);
            local.eq_ignore_ascii_case(needle) || p.full_name.eq_ignore_ascii_case(needle)
        })
    }

    /// Resolve a loose company identifier — `company_id` (case-insensitive) or
    /// trading name (case-insensitive) — to a loaded company.
    pub fn resolve_company(&self, ident: &str) -> Option<&CompanyNode> {
        if let Some(c) = self.companies.get(ident) {
            return Some(c);
        }
        let needle = ident.trim();
        self.companies.values().find(|c| {
            c.company_id.eq_ignore_ascii_case(needle) || c.name.eq_ignore_ascii_case(needle)
        })
    }

    /// Resolve a loose segment identifier — `code` or name (both
    /// case-insensitive) — to a loaded segment.
    pub fn resolve_segment(&self, ident: &str) -> Option<&SegmentNode> {
        if let Some(s) = self.segments.get(ident) {
            return Some(s);
        }
        let needle = ident.trim();
        self.segments
            .values()
            .find(|s| s.code.eq_ignore_ascii_case(needle) || s.name.eq_ignore_ascii_case(needle))
    }
}

// ---- SparrowDB reads -------------------------------------------------------

fn load_persons(store: &Store) -> Result<BTreeMap<String, PersonNode>> {
    let rows: Vec<PersonNode> = store.query(
        "MATCH (p:Person) RETURN p.full_name, p.first_name, p.last_name, p.email, p.title, \
         p.role, p.clearance_level, p.clearance_label, p.department, p.status, p.segment, p.note",
        Params::new(),
    )?;
    Ok(rows.into_iter().map(|p| (p.email.clone(), p)).collect())
}

fn load_companies(store: &Store) -> Result<BTreeMap<String, CompanyNode>> {
    let rows: Vec<CompanyNode> = store.query(
        "MATCH (c:Company) RETURN c.company_id, c.name, c.legal_name, c.segment, c.status, \
         c.acquired_year, c.hq_state, c.description",
        Params::new(),
    )?;
    Ok(rows
        .into_iter()
        .map(|c| (c.company_id.clone(), c))
        .collect())
}

fn load_segments(store: &Store) -> Result<BTreeMap<String, SegmentNode>> {
    let rows: Vec<SegmentNode> = store.query(
        "MATCH (s:Segment) RETURN s.name, s.code, s.description",
        Params::new(),
    )?;
    Ok(rows.into_iter().map(|s| (s.code.clone(), s)).collect())
}

fn load_manager(store: &Store, email: &str) -> Result<Option<String>> {
    let rows: Vec<EmailRow> = store.query(
        "MATCH (p:Person {email: $email})-[:REPORTS_TO]->(m:Person) RETURN m.email",
        Params::new().with("email", email),
    )?;
    Ok(rows.into_iter().next().map(|r| r.email))
}

fn load_manages(store: &Store, email: &str) -> Result<Vec<ManageRow>> {
    store.query(
        "MATCH (p:Person {email: $email})-[r:MANAGES]->(c:Company) RETURN c.company_id, r.scope",
        Params::new().with("email", email),
    )
}

fn load_company_segment(store: &Store, company_id: &str) -> Result<Option<String>> {
    let rows: Vec<CodeRow> = store.query(
        "MATCH (c:Company {company_id: $id})-[:BELONGS_TO_SEGMENT]->(s:Segment) RETURN s.code",
        Params::new().with("id", company_id),
    )?;
    Ok(rows.into_iter().next().map(|r| r.code))
}

// ---- graphify graph construction -------------------------------------------

fn firm_node(id: String, label: String, node_type: NodeType, kind: &str) -> GraphNode {
    let mut extra = HashMap::new();
    extra.insert(
        "kind".to_string(),
        serde_json::Value::String(kind.to_string()),
    );
    GraphNode {
        id,
        label,
        source_file: "firm-graph".to_string(),
        source_location: None,
        node_type,
        community: None,
        extra,
    }
}

fn firm_edge(source: String, target: String, relation: &str) -> GraphEdge {
    GraphEdge {
        source,
        target,
        relation: relation.to_string(),
        confidence: Confidence::Extracted,
        confidence_score: 1.0,
        source_file: "firm-graph".to_string(),
        source_location: None,
        weight: 1.0,
        provenance: None,
        extra: HashMap::new(),
    }
}

fn build_knowledge_graph(
    persons: &BTreeMap<String, PersonNode>,
    companies: &BTreeMap<String, CompanyNode>,
    segments: &BTreeMap<String, SegmentNode>,
    manager_of: &HashMap<String, String>,
    manages: &[ManagesEdge],
    company_segment: &HashMap<String, String>,
) -> Result<KnowledgeGraph> {
    let mut graph = KnowledgeGraph::new();

    for p in persons.values() {
        graph
            .add_node(firm_node(
                person_node_id(&p.email),
                p.full_name.clone(),
                NodeType::Concept,
                "Person",
            ))
            .map_err(graphify_err)?;
    }
    for c in companies.values() {
        graph
            .add_node(firm_node(
                company_node_id(&c.company_id),
                c.name.clone(),
                NodeType::Module,
                "Company",
            ))
            .map_err(graphify_err)?;
    }
    for s in segments.values() {
        graph
            .add_node(firm_node(
                segment_node_id(&s.code),
                s.name.clone(),
                NodeType::Namespace,
                "Segment",
            ))
            .map_err(graphify_err)?;
    }

    // REPORTS_TO: subordinate -> manager.
    for (sub, mgr) in manager_of {
        graph
            .add_edge(firm_edge(
                person_node_id(sub),
                person_node_id(mgr),
                "REPORTS_TO",
            ))
            .map_err(graphify_err)?;
    }
    // MANAGES: person -> company.
    for m in manages {
        graph
            .add_edge(firm_edge(
                person_node_id(&m.person_email),
                company_node_id(&m.company_id),
                "MANAGES",
            ))
            .map_err(graphify_err)?;
    }
    // BELONGS_TO_SEGMENT: company -> segment.
    for (company, segment) in company_segment {
        graph
            .add_edge(firm_edge(
                company_node_id(company),
                segment_node_id(segment),
                "BELONGS_TO_SEGMENT",
            ))
            .map_err(graphify_err)?;
    }

    Ok(graph)
}

fn graphify_err(e: graphify_core::error::GraphifyError) -> Error {
    Error::Internal(format!("graphify graph construction failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::seeded_store;

    #[test]
    fn loads_all_firm_nodes_and_edges() {
        let (_d, store) = seeded_store();
        let g = DiscoveryGraph::from_store(&store).unwrap();

        // 12 persons + 9 companies + 4 segments = 25 nodes.
        assert_eq!(g.persons().count(), 12);
        assert_eq!(g.companies().count(), 9);
        assert_eq!(g.segments().count(), 4);
        assert_eq!(g.node_count(), 25);

        // Node counts match SparrowDB.
        let store_persons = store
            .scalar_i64("MATCH (p:Person) RETURN count(p)", Params::new())
            .unwrap()
            .unwrap();
        assert_eq!(store_persons, 12);
        assert_eq!(
            g.node_count() as i64,
            store_persons
                + store
                    .scalar_i64("MATCH (c:Company) RETURN count(c)", Params::new())
                    .unwrap()
                    .unwrap()
                + store
                    .scalar_i64("MATCH (s:Segment) RETURN count(s)", Params::new())
                    .unwrap()
                    .unwrap()
        );

        // 11 REPORTS_TO + 45 MANAGES + 9 BELONGS_TO_SEGMENT = 65 edges.
        assert_eq!(g.edge_count(), 65);
    }

    #[test]
    fn reporting_adjacency_matches_seed() {
        let (_d, store) = seeded_store();
        let g = DiscoveryGraph::from_store(&store).unwrap();

        // Samantha reports to Tyler; Tyler reports to Peter (CSO).
        assert_eq!(
            g.manager_of("samantha.jordan@kanbrick.com"),
            Some("tyler.begemann@kanbrick.com")
        );
        assert_eq!(
            g.manager_of("tyler.begemann@kanbrick.com"),
            Some("peter.nash@kanbrick.com")
        );
        // The CEO has no manager.
        assert_eq!(g.manager_of("tracy.brittcool@kanbrick.com"), None);

        // The CEO's direct reports are the President and the Support Coordinator.
        let ceo_reports = g.direct_reports("tracy.brittcool@kanbrick.com");
        assert_eq!(ceo_reports.len(), 2);
        assert!(ceo_reports.contains(&"brian.humphrey@kanbrick.com".to_string()));
        assert!(ceo_reports.contains(&"dana.prescott@kanbrick.com".to_string()));

        // Ancestors of Samantha, lowest first.
        assert_eq!(
            g.ancestors("samantha.jordan@kanbrick.com"),
            vec![
                "tyler.begemann@kanbrick.com",
                "peter.nash@kanbrick.com",
                "brian.humphrey@kanbrick.com",
                "tracy.brittcool@kanbrick.com",
            ]
        );
    }

    #[test]
    fn manages_and_segments_match_seed() {
        let (_d, store) = seeded_store();
        let g = DiscoveryGraph::from_store(&store).unwrap();

        // JMTS belongs to Testing & Lab Services.
        assert_eq!(g.segment_of_company("JMTS"), Some("TLS"));
        // Tyler manages JMTS with the segment_lead scope.
        let tyler_jmts = g
            .manages()
            .iter()
            .find(|m| m.person_email == "tyler.begemann@kanbrick.com" && m.company_id == "JMTS")
            .expect("Tyler manages JMTS");
        assert_eq!(tyler_jmts.scope, ManageScope::SegmentLead);
    }

    #[test]
    fn flexible_resolvers() {
        let (_d, store) = seeded_store();
        let g = DiscoveryGraph::from_store(&store).unwrap();

        assert_eq!(
            g.resolve_person("samantha.jordan")
                .map(|p| p.email.as_str()),
            Some("samantha.jordan@kanbrick.com")
        );
        assert_eq!(
            g.resolve_person("tracy.brittcool@kanbrick.com")
                .map(|p| p.role.as_str()),
            Some("CEO")
        );
        assert_eq!(
            g.resolve_company("jmts").map(|c| c.name.as_str()),
            Some("JM Test Systems")
        );
        assert_eq!(
            g.resolve_segment("TLS").map(|s| s.name.as_str()),
            Some("Testing & Lab Services")
        );
        assert!(g.resolve_person("nobody.here").is_none());
    }
}
