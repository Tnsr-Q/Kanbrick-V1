//! Structural-influence ranking via graphify-rs (handoff §7).
//!
//! This is the one place we use graphify's own analysis algorithms
//! (`graphify_analyze::pagerank`) rather than our hand-rolled org traversal.
//! Because graphify's [`KnowledgeGraph`](graphify_core::graph::KnowledgeGraph) is
//! **undirected**, the score is undirected structural centrality ("who/what is
//! most connected"), not reporting rank (ADR-0003).

use kanbrick_core::NodeLabel;

use crate::model::InfluenceRank;
use crate::DiscoveryEngine;

/// Parse a namespaced graphify node id back into a firm kind + id.
fn parse_node_id(id: &str) -> Option<(NodeLabel, &str)> {
    if let Some(rest) = id.strip_prefix("person:") {
        Some((NodeLabel::Person, rest))
    } else if let Some(rest) = id.strip_prefix("company:") {
        Some((NodeLabel::Company, rest))
    } else {
        id.strip_prefix("segment:")
            .map(|rest| (NodeLabel::Segment, rest))
    }
}

impl DiscoveryEngine {
    /// The `top_n` most structurally central entities in the firm graph, by
    /// PageRank over the (undirected) graph.
    pub fn influence_ranking(&self, top_n: usize) -> Vec<InfluenceRank> {
        let kg = self.graph.knowledge_graph();
        graphify_analyze::pagerank(kg, top_n, 0.85, 20)
            .into_iter()
            .filter_map(|pr| {
                parse_node_id(&pr.id).map(|(kind, firm_id)| InfluenceRank {
                    id: firm_id.to_string(),
                    label: pr.label,
                    kind,
                    score: pr.score,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::seeded_store;
    use crate::DiscoveryEngine;

    #[test]
    fn influence_ranking_surfaces_the_most_connected() {
        let (_d, store) = seeded_store();
        let e = DiscoveryEngine::from_store(&store).unwrap();

        let top = e.influence_ranking(5);
        assert_eq!(top.len(), 5);
        // Every entry maps back to a real firm entity.
        assert!(top.iter().all(|r| !r.id.is_empty()));
        // Scores are sorted descending.
        for w in top.windows(2) {
            assert!(w[0].score >= w[1].score);
        }
        // The President and CEO are the most connected people (manage the whole
        // portfolio and sit atop the reporting tree) — at least one is in the top.
        let ids: Vec<&str> = top.iter().map(|r| r.id.as_str()).collect();
        assert!(
            ids.contains(&"brian.humphrey@kanbrick.com")
                || ids.contains(&"tracy.brittcool@kanbrick.com"),
            "expected an exec in the top 5, got {ids:?}"
        );
    }
}
