use std::collections::{BTreeMap, BTreeSet};

use dokploy_state::ResourceAddress;
use petgraph::{Direction, graph::DiGraph};

use crate::{ChangeKind, DesiredState, PlannedChange, StoredState};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum DependencyGraphKind {
    Desired,
    StoredRemoval,
}

pub(crate) struct DependencyCycle {
    pub(crate) graph: DependencyGraphKind,
    pub(crate) address: ResourceAddress,
}

pub(crate) struct DependencyOrdering {
    desired_rank: BTreeMap<ResourceAddress, usize>,
    removal_rank: BTreeMap<ResourceAddress, usize>,
}

impl DependencyOrdering {
    pub(crate) fn analyze(
        desired: &DesiredState,
        stored: &StoredState,
    ) -> Result<Self, Vec<DependencyCycle>> {
        let desired_graph = desired_graph(desired);
        let removal_graph = removal_graph(desired, stored);
        let mut cycles = cycle_addresses(&desired_graph)
            .into_iter()
            .map(|address| DependencyCycle {
                graph: DependencyGraphKind::Desired,
                address,
            })
            .chain(
                cycle_addresses(&removal_graph)
                    .into_iter()
                    .map(|address| DependencyCycle {
                        graph: DependencyGraphKind::StoredRemoval,
                        address,
                    }),
            )
            .collect::<Vec<_>>();
        cycles.sort_by(|left, right| {
            left.graph
                .cmp(&right.graph)
                .then_with(|| left.address.cmp(&right.address))
        });
        if !cycles.is_empty() {
            return Err(cycles);
        }

        let desired_order = deterministic_topological_order(&desired_graph);
        let removal_order = deterministic_reverse_topological_order(&removal_graph);
        Ok(Self {
            desired_rank: desired_order
                .into_iter()
                .enumerate()
                .map(|(rank, address)| (address, rank))
                .collect(),
            removal_rank: removal_order
                .into_iter()
                .enumerate()
                .map(|(rank, address)| (address, rank))
                .collect(),
        })
    }

    pub(crate) fn order_changes(&self, changes: &mut [PlannedChange]) {
        changes.sort_by(|left, right| {
            let (left_phase, left_rank) = self.change_key(left);
            let (right_phase, right_rank) = self.change_key(right);
            left_phase
                .cmp(&right_phase)
                .then_with(|| left_rank.cmp(&right_rank))
                .then_with(|| left.address().cmp(right.address()))
        });
    }

    fn change_key(&self, change: &PlannedChange) -> (u8, usize) {
        if matches!(change.kind(), ChangeKind::Delete | ChangeKind::Forget) {
            (
                1,
                self.removal_rank
                    .get(change.address())
                    .copied()
                    .unwrap_or(usize::MAX),
            )
        } else {
            (
                0,
                self.desired_rank
                    .get(change.address())
                    .copied()
                    .unwrap_or(usize::MAX),
            )
        }
    }
}

fn desired_graph(desired: &DesiredState) -> DiGraph<ResourceAddress, ()> {
    let addresses = desired.resources.keys().cloned().collect::<BTreeSet<_>>();
    build_graph(addresses, |address| {
        desired.resources[address].dependencies.clone()
    })
}

fn removal_graph(desired: &DesiredState, stored: &StoredState) -> DiGraph<ResourceAddress, ()> {
    let moved_sources = desired
        .moves
        .iter()
        .map(|directive| directive.from())
        .collect::<BTreeSet<_>>();
    let addresses = stored
        .resources
        .keys()
        .filter(|address| {
            !desired.resources.contains_key(*address) && !moved_sources.contains(address)
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    build_graph(addresses, |address| {
        stored.resources[address].dependencies.clone()
    })
}

fn build_graph(
    addresses: BTreeSet<ResourceAddress>,
    dependencies: impl Fn(&ResourceAddress) -> Vec<ResourceAddress>,
) -> DiGraph<ResourceAddress, ()> {
    let mut graph = DiGraph::new();
    let indices = addresses
        .iter()
        .map(|address| (address.clone(), graph.add_node(address.clone())))
        .collect::<BTreeMap<_, _>>();
    for dependent in &addresses {
        for dependency in dependencies(dependent) {
            let Some(dependency_index) = indices.get(&dependency) else {
                continue;
            };
            graph.add_edge(*dependency_index, indices[dependent], ());
        }
    }
    graph
}

fn deterministic_topological_order(graph: &DiGraph<ResourceAddress, ()>) -> Vec<ResourceAddress> {
    let mut indegrees = graph
        .node_indices()
        .map(|index| {
            (
                index,
                graph.neighbors_directed(index, Direction::Incoming).count(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut ready = indegrees
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(index, _)| (graph[*index].clone(), *index))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(graph.node_count());

    while let Some((address, index)) = ready.pop_first() {
        order.push(address);
        let mut dependents = graph
            .neighbors_directed(index, Direction::Outgoing)
            .collect::<Vec<_>>();
        dependents.sort_by(|left, right| graph[*left].cmp(&graph[*right]));
        for dependent in dependents {
            let degree = indegrees
                .get_mut(&dependent)
                .expect("every graph node has an indegree");
            *degree -= 1;
            if *degree == 0 {
                ready.insert((graph[dependent].clone(), dependent));
            }
        }
    }

    order
}

fn deterministic_reverse_topological_order(
    graph: &DiGraph<ResourceAddress, ()>,
) -> Vec<ResourceAddress> {
    let mut outdegrees = graph
        .node_indices()
        .map(|index| {
            (
                index,
                graph.neighbors_directed(index, Direction::Outgoing).count(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut ready = outdegrees
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(index, _)| (graph[*index].clone(), *index))
        .collect::<BTreeSet<_>>();
    let mut order = Vec::with_capacity(graph.node_count());

    while let Some((address, index)) = ready.pop_first() {
        order.push(address);
        let mut dependencies = graph
            .neighbors_directed(index, Direction::Incoming)
            .collect::<Vec<_>>();
        dependencies.sort_by(|left, right| graph[*left].cmp(&graph[*right]));
        for dependency in dependencies {
            let degree = outdegrees
                .get_mut(&dependency)
                .expect("every graph node has an outdegree");
            *degree -= 1;
            if *degree == 0 {
                ready.insert((graph[dependency].clone(), dependency));
            }
        }
    }

    order
}

fn cycle_addresses(graph: &DiGraph<ResourceAddress, ()>) -> Vec<ResourceAddress> {
    let mut addresses = petgraph::algo::kosaraju_scc(graph)
        .into_iter()
        .filter(|component| {
            component.len() > 1
                || component
                    .first()
                    .is_some_and(|index| graph.find_edge(*index, *index).is_some())
        })
        .flatten()
        .map(|index| graph[index].clone())
        .collect::<Vec<_>>();
    addresses.sort();
    addresses
}
