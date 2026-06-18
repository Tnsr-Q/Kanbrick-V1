//! Code-graph ingest (#38, ADR-0003 §6) — unify the code graph with the firm graph.
//!
//! Runs graphify's **AST extraction** (non-LLM, ADR-0003 §6) over a source tree,
//! assembles a graphify [`KnowledgeGraph`], and ingests it into the **same**
//! SparrowDB store that holds the firm data, under a small code ontology.
//!
//! ## Ontology
//!
//! graphify emits a rich set of node kinds and lowercase relations. We fold them
//! onto the three classes / four relations issue #38 prescribes, preserving the
//! precise graphify kind on every node as the `kind` property:
//!
//! | Ontology label | graphify `NodeType`(s) |
//! | --- | --- |
//! | `Function` | Function, Method, Struct, Enum, Trait, Class, Interface, Constant, Variable |
//! | `Module`   | Module, **File**, Namespace, Package |
//! | `Document` | Concept, Paper, Image |
//!
//! A Rust *file* is the natural module/container (graphify's `defines` edges
//! originate from the `File` node), so `File → Module`; this is what makes the
//! cross-layer "functions → their modules" query return rows. The coarse fold
//! lands a `struct` under `Function` as a *named code definition*; the exact
//! kind is never lost (it is stored in `kind`).
//!
//! | Ontology relation | graphify relation(s) | direction |
//! | --- | --- | --- |
//! | `DEFINED_IN` | `defines` (**reversed**) | entity → container |
//! | `CALLS`      | `calls`                  | caller → callee |
//! | `IMPORTS`    | `imports`                | file → import |
//! | `REFERENCES` | `uses`, `implements`, … | source → target |
//!
//! ## Idempotency (ADR-0001)
//!
//! Re-ingesting the same tree must not duplicate nodes or edges. Nodes are
//! written with parameterized node `MERGE` (the supported parameterized write
//! path); edges with inline relationship `MERGE`
//! (`MATCH … MERGE (a)-[:R]->(b)` — the parameterized path does not accept a
//! relationship `MERGE`, but the values we inline are `make_id` outputs, i.e.
//! `[a-z0-9_]`, so they are injection-safe). Both forms match-or-create.
//!
//! ## Feature gate
//!
//! This module lives behind the non-default `codegraph` feature: the extractor
//! pulls a tree-sitter / `reqwest` dependency tree, and the deployed API never
//! enables it, keeping the running service network-free (ADR-0003 §6). CI
//! exercises it via `--all-features`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use graphify_core::graph::KnowledgeGraph;
use graphify_core::model::{GraphNode, NodeType};
use kanbrick_core::{Error, Result};
use kanbrick_store::{Params, Store};

/// A node label in the code ontology persisted to SparrowDB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeLabel {
    /// A named code definition (function, method, struct, trait, enum, …).
    Function,
    /// A code container (source file, module, namespace, package).
    Module,
    /// A non-code artifact (concept, paper, image).
    Document,
}

impl CodeLabel {
    /// The SparrowDB node label (also a valid Cypher identifier).
    pub fn as_str(self) -> &'static str {
        match self {
            CodeLabel::Function => "Function",
            CodeLabel::Module => "Module",
            CodeLabel::Document => "Document",
        }
    }
}

/// A relationship type in the code ontology.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeRelation {
    /// Caller → callee.
    Calls,
    /// File → imported module/symbol.
    Imports,
    /// Entity → its containing module.
    DefinedIn,
    /// A structural reference (cross-file use, trait impl, …).
    References,
}

impl CodeRelation {
    /// The SparrowDB relationship type.
    pub fn as_str(self) -> &'static str {
        match self {
            CodeRelation::Calls => "CALLS",
            CodeRelation::Imports => "IMPORTS",
            CodeRelation::DefinedIn => "DEFINED_IN",
            CodeRelation::References => "REFERENCES",
        }
    }
}

/// Map a graphify node type onto a code-ontology label.
pub fn label_for(node_type: &NodeType) -> CodeLabel {
    match node_type {
        NodeType::Module | NodeType::File | NodeType::Namespace | NodeType::Package => {
            CodeLabel::Module
        }
        NodeType::Concept | NodeType::Paper | NodeType::Image => CodeLabel::Document,
        // Every other graphify kind is a named code definition.
        _ => CodeLabel::Function,
    }
}

/// Map a graphify edge relation onto an ontology relation and whether the
/// ontology edge points **opposite** the graphify edge.
///
/// `DEFINED_IN` reverses graphify's `defines` (container → entity) so the edge
/// reads "entity is DEFINED_IN container".
pub fn relation_for(relation: &str) -> (CodeRelation, bool) {
    match relation {
        "defines" => (CodeRelation::DefinedIn, true),
        "calls" => (CodeRelation::Calls, false),
        "imports" => (CodeRelation::Imports, false),
        // uses / implements / anything else: a structural reference.
        _ => (CodeRelation::References, false),
    }
}

/// Summary of an ingest run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestStats {
    /// `Function`-labelled nodes written.
    pub functions: usize,
    /// `Module`-labelled nodes written.
    pub modules: usize,
    /// `Document`-labelled nodes written.
    pub documents: usize,
    /// Ontology edges written.
    pub edges: usize,
}

impl IngestStats {
    /// Total nodes written across all labels.
    pub fn nodes(&self) -> usize {
        self.functions + self.modules + self.documents
    }
}

/// Run graphify's AST extraction over the source tree rooted at `root`.
///
/// Uses non-LLM extraction only (ADR-0003 §6): tree-sitter where a grammar is
/// available, otherwise graphify's regex extractor — no network or LLM API is
/// contacted. Dangling edges are dropped by `build_from_extraction`.
pub fn extract_code_graph(root: &Path) -> Result<KnowledgeGraph> {
    let files = graphify_extract::collect_files(root);
    let extraction = graphify_extract::extract(&files);
    graphify_build::build_from_extraction(&extraction)
        .map_err(|e| Error::Internal(format!("code-graph build failed: {e}")))
}

/// Write graphify's Neo4j Cypher export (`graph.cypher`) under `dir`.
///
/// This is the inspectable export artifact (#38 AC1). The authoritative ingest
/// into SparrowDB is [`ingest_code_graph`], which uses idempotent, dialect-aware
/// `MERGE` statements rather than the export's cross-statement `CREATE`s.
pub fn export_cypher(graph: &KnowledgeGraph, dir: &Path) -> Result<PathBuf> {
    graphify_export::export_cypher(graph, dir)
        .map_err(|e| Error::Internal(format!("cypher export failed: {e}")))
}

/// Ingest a graphify code graph into `store` under the code ontology.
///
/// Idempotent: re-running over the same graph neither duplicates nodes nor
/// edges. Coexists with firm data already in the store.
pub fn ingest_code_graph(store: &Store, graph: &KnowledgeGraph) -> Result<IngestStats> {
    let mut stats = IngestStats::default();
    let mut label_of: HashMap<&str, CodeLabel> = HashMap::new();

    for node in graph.nodes() {
        let label = label_for(&node.node_type);
        label_of.insert(node.id.as_str(), label);
        merge_node(store, label, node)?;
        match label {
            CodeLabel::Function => stats.functions += 1,
            CodeLabel::Module => stats.modules += 1,
            CodeLabel::Document => stats.documents += 1,
        }
    }

    for (src, tgt, edge) in graph.edges_with_endpoints() {
        let (Some(&src_label), Some(&tgt_label)) = (label_of.get(src), label_of.get(tgt)) else {
            // `build_from_extraction` already dropped dangling edges, so both
            // endpoints are present; skip defensively if not.
            continue;
        };
        let (rel, reversed) = relation_for(&edge.relation);
        let (a, a_label, b, b_label) = if reversed {
            (tgt, tgt_label, src, src_label)
        } else {
            (src, src_label, tgt, tgt_label)
        };
        merge_edge(store, a, a_label, rel, b, b_label)?;
        stats.edges += 1;
    }

    tracing::info!(
        target: "kanbrick_discovery::codegraph",
        functions = stats.functions,
        modules = stats.modules,
        documents = stats.documents,
        edges = stats.edges,
        "ingested code graph"
    );
    Ok(stats)
}

/// Extract the code graph from `root` and ingest it into `store`.
///
/// When `export_dir` is given, also writes graphify's Cypher export there.
pub fn ingest_from_source(
    store: &Store,
    root: &Path,
    export_dir: Option<&Path>,
) -> Result<IngestStats> {
    let graph = extract_code_graph(root)?;
    if let Some(dir) = export_dir {
        export_cypher(&graph, dir)?;
    }
    ingest_code_graph(store, &graph)
}

/// MERGE one code node by `id` (idempotent; the parameterized write path).
fn merge_node(store: &Store, label: CodeLabel, node: &GraphNode) -> Result<()> {
    let cypher = format!(
        "MERGE (n:{} {{id: $id, name: $name, source_file: $source_file, kind: $kind}})",
        label.as_str()
    );
    let params = Params::new()
        .with("id", node.id.as_str())
        .with("name", node.label.as_str())
        .with("source_file", node.source_file.as_str())
        .with("kind", node.node_type.to_string().to_lowercase());
    store.execute_with(&cypher, params)?;
    Ok(())
}

/// MERGE one code edge `(a)-[:REL]->(b)` (idempotent; inline relationship MERGE).
fn merge_edge(
    store: &Store,
    a_id: &str,
    a_label: CodeLabel,
    rel: CodeRelation,
    b_id: &str,
    b_label: CodeLabel,
) -> Result<()> {
    // `make_id` outputs are `[a-z0-9_]`; `escape_id` is belt-and-braces.
    let cypher = format!(
        "MATCH (a:{a_label} {{id: '{a_id}'}}), (b:{b_label} {{id: '{b_id}'}}) \
         MERGE (a)-[:{rel}]->(b)",
        a_label = a_label.as_str(),
        a_id = escape_id(a_id),
        b_label = b_label.as_str(),
        b_id = escape_id(b_id),
        rel = rel.as_str(),
    );
    store.execute(&cypher)?;
    Ok(())
}

/// Escape a node id for inline use in a Cypher string literal.
fn escape_id(id: &str) -> String {
    id.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::seeded_store;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    struct IdRow {
        id: String,
    }

    fn count_label(store: &Store, label: &str) -> i64 {
        store
            .scalar_i64(&format!("MATCH (n:{label}) RETURN count(n)"), Params::new())
            .unwrap()
            .unwrap_or(0)
    }

    /// Project the cross-layer `Function -[:DEFINED_IN]-> Module` join and count
    /// rows in Rust (count-over-path is unreliable in SparrowDB; ADR-0001).
    fn defined_in_rows(store: &Store) -> usize {
        let rows: Vec<IdRow> = store
            .query(
                "MATCH (f:Function)-[:DEFINED_IN]->(m:Module) RETURN f.id",
                Params::new(),
            )
            .unwrap();
        // Read `id` so each projected function is a non-empty identity.
        rows.iter().filter(|r| !r.id.is_empty()).count()
    }

    /// Write a small Rust source tree to ingest.
    fn write_fixture(root: &Path) {
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            r#"
pub fn alpha() -> i32 { 1 }

pub fn beta() -> i32 { alpha() + 1 }

pub struct Widget { pub size: i32 }

impl Widget {
    pub fn area(&self) -> i32 { self.size * self.size }
}
"#,
        )
        .unwrap();
        std::fs::write(root.join("src/util.rs"), "pub fn helper() -> i32 { 42 }\n").unwrap();
    }

    #[test]
    fn label_and_relation_mapping() {
        assert_eq!(label_for(&NodeType::Function), CodeLabel::Function);
        assert_eq!(label_for(&NodeType::Method), CodeLabel::Function);
        assert_eq!(label_for(&NodeType::Struct), CodeLabel::Function);
        assert_eq!(label_for(&NodeType::File), CodeLabel::Module);
        assert_eq!(label_for(&NodeType::Module), CodeLabel::Module);
        assert_eq!(label_for(&NodeType::Paper), CodeLabel::Document);

        assert_eq!(relation_for("defines"), (CodeRelation::DefinedIn, true));
        assert_eq!(relation_for("calls"), (CodeRelation::Calls, false));
        assert_eq!(relation_for("imports"), (CodeRelation::Imports, false));
        assert_eq!(
            relation_for("implements"),
            (CodeRelation::References, false)
        );
        assert_eq!(relation_for("uses"), (CodeRelation::References, false));
    }

    #[test]
    fn ingest_is_idempotent_and_coexists_with_firm_data() {
        let (_d, store) = seeded_store();
        let src = tempfile::tempdir().unwrap();
        write_fixture(src.path());

        // First ingest.
        let stats = ingest_code_graph(&store, &extract_code_graph(src.path()).unwrap()).unwrap();

        // AC: Function and Module node counts are positive.
        assert!(
            stats.functions > 0,
            "expected Function nodes, got {stats:?}"
        );
        assert!(stats.modules > 0, "expected Module nodes, got {stats:?}");
        let functions_1 = count_label(&store, "Function");
        let modules_1 = count_label(&store, "Module");
        assert!(functions_1 > 0);
        assert!(modules_1 > 0);

        // AC: the cross-layer join returns rows.
        let join_1 = defined_in_rows(&store);
        assert!(join_1 > 0, "expected Function-DEFINED_IN->Module rows");

        // AC: re-running the ingest does not duplicate nodes (or edges).
        ingest_code_graph(&store, &extract_code_graph(src.path()).unwrap()).unwrap();
        assert_eq!(count_label(&store, "Function"), functions_1, "Function dup");
        assert_eq!(count_label(&store, "Module"), modules_1, "Module dup");
        assert_eq!(defined_in_rows(&store), join_1, "DEFINED_IN dup");

        // The code graph is ingested *alongside* the firm data, untouched.
        assert_eq!(count_label(&store, "Person"), 12);
        assert_eq!(count_label(&store, "Company"), 9);
    }

    #[test]
    fn ingest_from_source_writes_export_artifact() {
        let (_d, store) = seeded_store();
        let src = tempfile::tempdir().unwrap();
        write_fixture(src.path());
        let out = tempfile::tempdir().unwrap();

        let stats = ingest_from_source(&store, src.path(), Some(out.path())).unwrap();
        assert!(stats.nodes() > 0);
        // graphify writes `graph.cypher` into the export dir.
        assert!(out.path().join("graph.cypher").exists());
    }
}
