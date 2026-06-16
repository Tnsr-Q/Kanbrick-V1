//! Org-chart analytics over the `REPORTS_TO` tree (issues #31, #32, #35).
//!
//! The graphify backing graph is undirected, so these directed algorithms are
//! computed from the reporting adjacency [`DiscoveryGraph`] derives at load time
//! (ADR-0003). All results are firm-typed.

use std::collections::{HashMap, HashSet, VecDeque};

use kanbrick_core::{Error, PersonId, Result};

use crate::graph::DiscoveryGraph;
use crate::model::{OrgNeighborhood, PersonRef, ReportingPath, SpanMetrics};
use crate::DiscoveryEngine;

/// Build a [`PersonRef`] for a known-loaded email.
fn person_ref(graph: &DiscoveryGraph, email: &str) -> Result<PersonRef> {
    graph
        .person(email)
        .map(PersonRef::from_node)
        .ok_or_else(|| Error::NotFound(format!("person {email}")))
}

/// Undirected reporting neighbours of `email`: its manager plus direct reports.
fn reporting_neighbors(graph: &DiscoveryGraph, email: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(mgr) = graph.manager_of(email) {
        out.push(mgr.to_string());
    }
    out.extend(graph.direct_reports(email).iter().cloned());
    out
}

impl DiscoveryEngine {
    /// The shortest `REPORTS_TO` chain between two people, in either direction.
    ///
    /// Returns `None` when the two are not connected through the reporting tree.
    /// Unknown identifiers are an [`Error::NotFound`]. (Issue #31.)
    pub fn reporting_path(&self, from: &str, to: &str) -> Result<Option<ReportingPath>> {
        let graph = &self.graph;
        let from = graph
            .resolve_person(from)
            .ok_or_else(|| Error::NotFound(format!("person {from}")))?
            .email
            .clone();
        let to = graph
            .resolve_person(to)
            .ok_or_else(|| Error::NotFound(format!("person {to}")))?
            .email
            .clone();

        if from == to {
            return Ok(Some(ReportingPath {
                steps: vec![person_ref(graph, &from)?],
            }));
        }

        // Breadth-first search over the (undirected) reporting tree.
        let mut prev: HashMap<String, String> = HashMap::new();
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(from.clone());
        let mut queue = VecDeque::new();
        queue.push_back(from.clone());

        while let Some(current) = queue.pop_front() {
            for next in reporting_neighbors(graph, &current) {
                if visited.insert(next.clone()) {
                    prev.insert(next.clone(), current.clone());
                    if next == to {
                        return Ok(Some(reconstruct_path(graph, &prev, &from, &to)?));
                    }
                    queue.push_back(next);
                }
            }
        }
        Ok(None)
    }

    /// Direct and indirect report counts and subtree depth for a person.
    ///
    /// `indirect_reports` counts every subordinate beneath the person
    /// transitively (issue #32 acceptance: the CEO's value is total employees
    /// minus one). (Issue #32.)
    pub fn span_of_control(&self, person: &str) -> Result<SpanMetrics> {
        let graph = &self.graph;
        let email = graph
            .resolve_person(person)
            .ok_or_else(|| Error::NotFound(format!("person {person}")))?
            .email
            .clone();

        let direct = graph.direct_reports(&email).len();

        let mut indirect = 0usize;
        let mut max_depth = 0usize;
        let mut queue: VecDeque<(String, usize)> = graph
            .direct_reports(&email)
            .iter()
            .map(|r| (r.clone(), 1usize))
            .collect();
        while let Some((node, depth)) = queue.pop_front() {
            indirect += 1;
            max_depth = max_depth.max(depth);
            for report in graph.direct_reports(&node) {
                queue.push_back((report.clone(), depth + 1));
            }
        }

        Ok(SpanMetrics {
            person: PersonId::from(email.as_str()),
            direct_reports: direct,
            indirect_reports: indirect,
            max_depth,
        })
    }

    /// Everyone within `depth` reporting hops of a person, with the reporting
    /// edges among them. Depth 0 returns just the person. (Issue #35.)
    pub fn org_neighborhood(&self, person: &str, depth: usize) -> Result<OrgNeighborhood> {
        let graph = &self.graph;
        let center = graph
            .resolve_person(person)
            .ok_or_else(|| Error::NotFound(format!("person {person}")))?
            .email
            .clone();

        let mut dist: HashMap<String, usize> = HashMap::new();
        dist.insert(center.clone(), 0);
        let mut queue = VecDeque::new();
        queue.push_back((center.clone(), 0usize));
        while let Some((node, d)) = queue.pop_front() {
            if d == depth {
                continue;
            }
            for next in reporting_neighbors(graph, &node) {
                if !dist.contains_key(&next) {
                    dist.insert(next.clone(), d + 1);
                    queue.push_back((next, d + 1));
                }
            }
        }

        let mut emails: Vec<&String> = dist.keys().collect();
        emails.sort();
        let members: Vec<PersonRef> = emails
            .iter()
            .filter_map(|e| graph.person(e).map(PersonRef::from_node))
            .collect();

        // Reporting edges whose *both* endpoints are members.
        let mut reporting_edges = Vec::new();
        for e in &emails {
            if let Some(mgr) = graph.manager_of(e) {
                if dist.contains_key(mgr) {
                    reporting_edges.push((PersonId::from(e.as_str()), PersonId::from(mgr)));
                }
            }
        }
        reporting_edges.sort();

        Ok(OrgNeighborhood {
            center: PersonId::from(center.as_str()),
            depth,
            members,
            reporting_edges,
        })
    }

    /// The lowest common manager of two people: the lowest person who is a
    /// (strict) manager of both, via the `REPORTS_TO` tree. `None` when there is
    /// none (e.g. one of them is the CEO). (Issue #35.)
    pub fn common_manager(&self, a: &str, b: &str) -> Result<Option<PersonRef>> {
        let graph = &self.graph;
        let a = graph
            .resolve_person(a)
            .ok_or_else(|| Error::NotFound(format!("person {a}")))?
            .email
            .clone();
        let b = graph
            .resolve_person(b)
            .ok_or_else(|| Error::NotFound(format!("person {b}")))?
            .email
            .clone();

        let ancestors_b: HashSet<String> = graph.ancestors(&b).into_iter().collect();
        for manager in graph.ancestors(&a) {
            if ancestors_b.contains(&manager) {
                return Ok(Some(person_ref(graph, &manager)?));
            }
        }
        Ok(None)
    }
}

/// Reconstruct the path `from → … → to` from BFS predecessors.
fn reconstruct_path(
    graph: &DiscoveryGraph,
    prev: &HashMap<String, String>,
    from: &str,
    to: &str,
) -> Result<ReportingPath> {
    let mut chain = vec![to.to_string()];
    let mut cursor = to.to_string();
    while cursor != from {
        let p = prev
            .get(&cursor)
            .ok_or_else(|| Error::Internal("broken reporting path reconstruction".into()))?;
        chain.push(p.clone());
        cursor = p.clone();
    }
    chain.reverse();
    let steps = chain
        .iter()
        .map(|e| person_ref(graph, e))
        .collect::<Result<Vec<_>>>()?;
    Ok(ReportingPath { steps })
}

#[cfg(test)]
mod tests {
    use crate::test_support::seeded_store;
    use crate::DiscoveryEngine;

    fn engine() -> (tempfile::TempDir, DiscoveryEngine) {
        let (dir, store) = seeded_store();
        let engine = DiscoveryEngine::from_store(&store).unwrap();
        (dir, engine)
    }

    #[test]
    fn reporting_path_leaf_to_ceo() {
        let (_d, e) = engine();
        // PRD checkpoint: samantha.jordan → tracy.brittcool.
        let path = e
            .reporting_path("samantha.jordan", "tracy.brittcool")
            .unwrap()
            .expect("a path exists");
        let chain: Vec<&str> = path.steps.iter().map(|p| p.full_name.as_str()).collect();
        assert_eq!(
            chain,
            vec![
                "Samantha Jordan",
                "Tyler Begemann",
                "Peter Nash",
                "Brian Humphrey",
                "Tracy Britt Cool"
            ]
        );
        assert_eq!(path.len(), 4);
    }

    #[test]
    fn reporting_path_is_symmetric() {
        let (_d, e) = engine();
        let up = e
            .reporting_path("samantha.jordan", "tracy.brittcool")
            .unwrap()
            .unwrap();
        let down = e
            .reporting_path("tracy.brittcool", "samantha.jordan")
            .unwrap()
            .unwrap();
        assert_eq!(up.len(), down.len());
        // Reversed direction yields the reversed chain.
        let up_names: Vec<_> = up.steps.iter().map(|p| p.id.clone()).collect();
        let mut down_names: Vec<_> = down.steps.iter().map(|p| p.id.clone()).collect();
        down_names.reverse();
        assert_eq!(up_names, down_names);
    }

    #[test]
    fn reporting_path_same_person_is_trivial() {
        let (_d, e) = engine();
        let path = e
            .reporting_path("elena.ruiz", "elena.ruiz")
            .unwrap()
            .unwrap();
        assert_eq!(path.steps.len(), 1);
        assert_eq!(path.len(), 0);
    }

    #[test]
    fn reporting_path_across_segments() {
        let (_d, e) = engine();
        // Samantha (under Tyler) ↔ Elena (under Blake): meet at Peter (CSO).
        let path = e
            .reporting_path("samantha.jordan", "elena.ruiz")
            .unwrap()
            .unwrap();
        let names: Vec<&str> = path.steps.iter().map(|p| p.full_name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Samantha Jordan",
                "Tyler Begemann",
                "Peter Nash",
                "Blake Richardson",
                "Elena Ruiz"
            ]
        );
    }

    #[test]
    fn reporting_path_unknown_person_errs() {
        let (_d, e) = engine();
        assert!(e.reporting_path("ghost", "tracy.brittcool").is_err());
    }

    #[test]
    fn reporting_path_unconnected_is_none() {
        // Add an isolated Person (no REPORTS_TO, no reports) → its own component.
        let (_d, store) = seeded_store();
        store
            .execute(
                "CREATE (g:Person {full_name: 'Ghost Unlinked', first_name: 'Ghost', \
                 last_name: 'Unlinked', email: 'ghost.unlinked@kanbrick.com', title: 'Contractor', \
                 role: 'Contractor', clearance_level: 'L1', clearance_label: 'Support', \
                 department: 'External', status: 'active'})",
            )
            .unwrap();
        let e = DiscoveryEngine::from_store(&store).unwrap();
        assert!(e
            .reporting_path("ghost.unlinked@kanbrick.com", "tracy.brittcool")
            .unwrap()
            .is_none());
    }

    #[test]
    fn span_of_control_ceo_mid_and_leaf() {
        let (_d, e) = engine();

        // CEO: 2 direct (President + Support), 11 indirect (everyone else), depth 4.
        let ceo = e.span_of_control("tracy.brittcool").unwrap();
        assert_eq!(ceo.direct_reports, 2);
        assert_eq!(ceo.indirect_reports, 11);
        assert_eq!(ceo.max_depth, 4);

        // PRD checkpoint: Brian's span. 4 direct, 9 indirect, depth 3.
        let brian = e.span_of_control("brian.humphrey").unwrap();
        assert_eq!(brian.direct_reports, 4);
        assert_eq!(brian.indirect_reports, 9);
        assert_eq!(brian.max_depth, 3);

        // Leaf analyst: zero everything.
        let leaf = e.span_of_control("elena.ruiz").unwrap();
        assert_eq!(leaf.direct_reports, 0);
        assert_eq!(leaf.indirect_reports, 0);
        assert_eq!(leaf.max_depth, 0);
    }

    #[test]
    fn org_neighborhood_depths() {
        let (_d, e) = engine();

        // Depth 0: just the person.
        let n0 = e.org_neighborhood("peter.nash", 0).unwrap();
        assert_eq!(n0.members.len(), 1);
        assert_eq!(n0.members[0].full_name, "Peter Nash");

        // Depth 1 of Peter (CSO): his manager (Brian) + his 3 direct reports.
        let n1 = e.org_neighborhood("peter.nash", 1).unwrap();
        let emails: Vec<&str> = n1.members.iter().map(|p| p.email()).collect();
        assert_eq!(n1.members.len(), 5); // Peter + Brian + Tyler + Blake + Sloan
        assert!(emails.contains(&"brian.humphrey@kanbrick.com"));
        assert!(emails.contains(&"tyler.begemann@kanbrick.com"));
        assert!(emails.contains(&"blake.richardson@kanbrick.com"));
        assert!(emails.contains(&"sloan.allen@kanbrick.com"));
    }

    #[test]
    fn common_manager_scenarios() {
        let (_d, e) = engine();

        // Two analysts in different segments → their shared ancestor, the CSO.
        let m = e
            .common_manager("samantha.jordan", "elena.ruiz")
            .unwrap()
            .unwrap();
        assert_eq!(m.full_name, "Peter Nash");

        // Two strategic leaders → the President.
        let m = e
            .common_manager("matt.berns", "andrea.lewis")
            .unwrap()
            .unwrap();
        assert_eq!(m.full_name, "Brian Humphrey");

        // Someone deep + the support coordinator → the CEO.
        let m = e
            .common_manager("samantha.jordan", "dana.prescott")
            .unwrap()
            .unwrap();
        assert_eq!(m.full_name, "Tracy Britt Cool");

        // The CEO has no manager → no common manager with anyone.
        assert!(e
            .common_manager("tracy.brittcool", "elena.ruiz")
            .unwrap()
            .is_none());
    }
}
