from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{label}: expected one match, found {count}")
    return text.replace(old, new, 1)


def write(path: str, transform):
    file = Path(path)
    original = file.read_text()
    updated = transform(original)
    if updated == original:
        raise SystemExit(f"{path}: no changes")
    file.write_text(updated)


def patch_cluster(text: str) -> str:
    text = replace_once(
        text,
        "    pub node_states: BTreeMap<NodeId, NodeState>,\n}\n\n/// (De)hydrate",
        "    pub node_states: BTreeMap<NodeId, NodeState>,\n    pub node_health: BTreeMap<NodeId, String>,\n}\n\n/// (De)hydrate",
        "digest node_health",
    )
    text = replace_once(
        text,
        "    pub node_states: BTreeMap<NodeId, NodeState>,\n}\n\nimpl Default for Cluster",
        "    pub node_states: BTreeMap<NodeId, NodeState>,\n    /// Non-revisioned observations used for placement scoring. Health is\n    /// deliberately outside `Graph::Node.attrs`: hard filters must only see\n    /// revisioned resource facts.\n    #[cfg_attr(\n        feature = \"serde\",\n        serde(default, skip_serializing_if = \"BTreeMap::is_empty\")\n    )]\n    pub node_health: BTreeMap<NodeId, String>,\n}\n\nimpl Default for Cluster",
        "cluster node_health",
    )
    text = replace_once(
        text,
        "            node_states: BTreeMap::new(),\n        }",
        "            node_states: BTreeMap::new(),\n            node_health: BTreeMap::new(),\n        }",
        "default node_health",
    )
    text = replace_once(
        text,
        "    pub fn node_state(&self, node: NodeId) -> Option<NodeState> {\n        self.graph.node(node).map(|_| {\n            self.node_states\n                .get(&node)\n                .copied()\n                .unwrap_or(NodeState::Schedulable)\n        })\n    }\n\n    pub(crate) fn placement_blocked",
        "    pub fn node_state(&self, node: NodeId) -> Option<NodeState> {\n        self.graph.node(node).map(|_| {\n            self.node_states\n                .get(&node)\n                .copied()\n                .unwrap_or(NodeState::Schedulable)\n        })\n    }\n\n    /// Latest non-revisioned health observation for a known node. Absence is\n    /// neutral/healthy for scoring; it is not a revisioned placement fact.\n    pub fn node_health(&self, node: NodeId) -> Option<&str> {\n        self.graph\n            .node(node)\n            .and_then(|_| self.node_health.get(&node).map(String::as_str))\n    }\n\n    /// Upgrade old snapshots that stored health inside revisioned graph attrs.\n    /// Dedicated observation state wins if both representations are present.\n    pub fn migrate_legacy_observations(&mut self) {\n        for (node, health) in self.graph.take_legacy_health_attrs() {\n            self.node_health.entry(node).or_insert(health);\n        }\n    }\n\n    pub(crate) fn placement_blocked",
        "node health methods",
    )
    text = replace_once(
        text,
        "            last_fence: self.last_fence.clone(),\n            node_states: self.node_states.clone(),\n        }",
        "            last_fence: self.last_fence.clone(),\n            node_states: self.node_states.clone(),\n            node_health: self.node_health.clone(),\n        }",
        "digest value node_health",
    )
    text = replace_once(
        text,
        "    fn apply_graph(\n        &mut self,\n        nodes: Vec<crate::types::Node>,\n        edges: Vec<crate::types::Edge>,\n    ) -> Result<Vec<Effect>, Error> {\n        if !self.agreed {\n            return Err(Error::NotAgreed);\n        }\n        let mut staged = self.graph.clone();\n        staged.apply(nodes, edges)?;",
        "    fn apply_graph(\n        &mut self,\n        mut nodes: Vec<crate::types::Node>,\n        edges: Vec<crate::types::Edge>,\n    ) -> Result<Vec<Effect>, Error> {\n        if !self.agreed {\n            return Err(Error::NotAgreed);\n        }\n        let health = take_health_attrs(&mut nodes);\n        let mut staged = self.graph.clone();\n        staged.apply(nodes, edges)?;",
        "apply_graph extract health",
    )
    text = replace_once(
        text,
        "        self.graph = staged;\n        Ok(Vec::new())\n    }\n\n    fn apply_resource_facts(\n        &mut self,\n        nodes: Vec<crate::types::Node>,",
        "        self.graph = staged;\n        self.node_health.extend(health);\n        Ok(Vec::new())\n    }\n\n    fn apply_resource_facts(\n        &mut self,\n        mut nodes: Vec<crate::types::Node>,",
        "apply_graph commit health",
    )
    text = replace_once(
        text,
        "        if !self.agreed {\n            return Err(Error::NotAgreed);\n        }\n        let mut staged = self.graph.clone();\n        staged.apply_resource_facts(nodes, edges, claim_bindings)?;",
        "        if !self.agreed {\n            return Err(Error::NotAgreed);\n        }\n        let health = take_health_attrs(&mut nodes);\n        let mut staged = self.graph.clone();\n        staged.apply_resource_facts(nodes, edges, claim_bindings)?;",
        "apply_resource_facts extract health",
    )
    text = replace_once(
        text,
        "        self.graph = staged;\n        Ok(Vec::new())\n    }\n\n    fn reserve_lease",
        "        self.graph = staged;\n        self.node_health.extend(health);\n        Ok(Vec::new())\n    }\n\n    fn reserve_lease",
        "apply_resource_facts commit health",
    )
    old_health = '''    /// Health is scoring input, not authoritative ownership state: it changes
    /// node attrs without advancing Graph.revision, so in-flight Allocations
    /// are never stranded by a health update.
    fn set_node_health(&mut self, node: NodeId, health: String) -> Result<Vec<Effect>, Error> {
        self.graph
            .set_attr(node, "health", health)
            .then_some(Vec::new())
            .ok_or(Error::UnknownNode(node))
    }'''
    new_health = '''    /// Health is non-revisioned scoring input, not a resource fact or authority
    /// signal, so observation changes never strand in-flight Allocations.
    fn set_node_health(&mut self, node: NodeId, health: String) -> Result<Vec<Effect>, Error> {
        if self.graph.node(node).is_none() {
            return Err(Error::UnknownNode(node));
        }
        self.node_health.insert(node, health);
        Ok(Vec::new())
    }'''
    text = replace_once(text, old_health, new_health, "set_node_health")
    marker = "#[derive(Clone, Debug, PartialEq, Eq)]\npub struct LeaseDigest"
    helper = '''fn take_health_attrs(nodes: &mut [crate::types::Node]) -> BTreeMap<NodeId, String> {
    nodes
        .iter_mut()
        .filter_map(|node| node.attrs.remove("health").map(|health| (node.id, health)))
        .collect()
}

'''
    text = replace_once(text, marker, helper + marker, "health extraction helper")
    return text


def patch_graph(text: str) -> str:
    old_set = '''    /// Set one node attribute without advancing `Graph.revision`. Returns
    /// false when the node is unknown. Used for non-authoritative state such
    /// as health that must never strand in-flight Allocations.
    pub fn set_attr(&mut self, id: NodeId, key: &str, value: String) -> bool {
        match self.nodes.get_mut(&id) {
            Some(node) => {
                node.attrs.insert(key.to_string(), value);
                true
            }
            None => false,
        }
    }

'''
    new_set = '''    /// Remove proof-era health attrs from a deserialized legacy graph without
    /// advancing its revision. Callers move the values into observation state.
    pub(crate) fn take_legacy_health_attrs(&mut self) -> BTreeMap<NodeId, String> {
        self.nodes
            .iter_mut()
            .filter_map(|(id, node)| node.attrs.remove("health").map(|health| (*id, health)))
            .collect()
    }

'''
    text = replace_once(text, old_set, new_set, "graph legacy health migration")
    old_degraded = '''    /// The nearest ancestry node (including `id` itself) marked
    /// `health=degraded`, if any. Health is scoring input, never authority.
    pub fn degraded_ancestor(&self, id: NodeId) -> Option<NodeId> {
        let degraded = |node: NodeId| {
            self.node(node).and_then(|item| item.attrs.get("health"))
                == Some(&"degraded".to_string())
        };
        if degraded(id) {
            return Some(id);
        }
        self.ancestors(id)
            .into_iter()
            .find(|ancestor| degraded(*ancestor))
    }

'''
    text = replace_once(text, old_degraded, "", "remove graph degraded health")
    return text


def patch_select(text: str) -> str:
    old_header = '''pub fn select(
    graph: &Graph,
    occupancy: &Occupancy,
    request: &Request,
    quarantine: &BTreeSet<NodeId>,
) -> Result<Allocation, Error> {
    // Malformed constraints can never be satisfied; refuse them up front'''
    new_header = '''pub fn select(
    graph: &Graph,
    occupancy: &Occupancy,
    request: &Request,
    quarantine: &BTreeSet<NodeId>,
) -> Result<Allocation, Error> {
    select_with_health(graph, occupancy, request, quarantine, &BTreeMap::new())
}

pub(crate) fn select_with_health(
    graph: &Graph,
    occupancy: &Occupancy,
    request: &Request,
    quarantine: &BTreeSet<NodeId>,
    node_health: &BTreeMap<NodeId, String>,
) -> Result<Allocation, Error> {
    // Malformed constraints can never be satisfied; refuse them up front'''
    text = replace_once(text, old_header, new_header, "select health wrapper")
    text = replace_once(
        text,
        "        return select_one_machine(graph, occupancy, request, mode, quarantine);",
        "        return select_one_machine(graph, occupancy, request, mode, quarantine, node_health);",
        "select_one_machine call",
    )
    text = replace_once(
        text,
        "        mode,\n        quarantine,\n    };",
        "        mode,\n        quarantine,\n        node_health,\n    };",
        "top select ctx",
    )
    text = replace_once(
        text,
        "    mode: PackMode,\n    quarantine: &BTreeSet<NodeId>,\n) -> Result<Allocation, Error> {",
        "    mode: PackMode,\n    quarantine: &BTreeSet<NodeId>,\n    node_health: &BTreeMap<NodeId, String>,\n) -> Result<Allocation, Error> {",
        "select_one_machine signature",
    )
    text = replace_once(
        text,
        "            mode,\n            quarantine,\n        };",
        "            mode,\n            quarantine,\n            node_health,\n        };",
        "machine select ctx",
    )
    text = replace_once(
        text,
        "    mode: PackMode,\n    quarantine: &'a BTreeSet<NodeId>,\n}",
        "    mode: PackMode,\n    quarantine: &'a BTreeSet<NodeId>,\n    node_health: &'a BTreeMap<NodeId, String>,\n}",
        "select ctx field",
    )
    text = replace_once(
        text,
        "        mode,\n        quarantine,\n    } = *ctx;",
        "        mode,\n        quarantine,\n        node_health,\n    } = *ctx;",
        "select ctx destructure",
    )
    text = replace_once(
        text,
        "    memory_want: u64,\n}",
        "    memory_want: u64,\n    node_health: &'a BTreeMap<NodeId, String>,\n}",
        "score ctx field",
    )
    text = replace_once(
        text,
        "            mode,\n            memory_want,\n        };",
        "            mode,\n            memory_want,\n            node_health,\n        };",
        "consumable score ctx",
    )
    text = replace_once(
        text,
        "        mode,\n        memory_want: 0,\n    };",
        "        mode,\n        memory_want: 0,\n        node_health,\n    };",
        "count score ctx",
    )
    text = text.replace(
        "graph.degraded_ancestor(node).is_some()",
        "degraded_ancestor(graph, node_health, node).is_some()",
    )
    text = text.replace(
        "let degraded = graph.degraded_ancestor(node).is_some();",
        "let degraded = degraded_ancestor(graph, node_health, node).is_some();",
    )
    text = replace_once(
        text,
        "            .any(|filter| node.attrs.get(&filter.key) != Some(&filter.value))",
        "            .any(|filter| {\n                filter.key == \"health\" || node.attrs.get(&filter.key) != Some(&filter.value)\n            })",
        "hard filter health separation",
    )
    text = replace_once(
        text,
        "        if let Preference::PreferAttr { key, value } = preference\n            && ctx",
        "        if let Preference::PreferAttr { key, value } = preference\n            && key.as_str() != \"health\"\n            && ctx",
        "preference health separation",
    )
    text = replace_once(
        text,
        "    if ctx.graph.degraded_ancestor(node).is_some() {",
        "    if degraded_ancestor(ctx.graph, ctx.node_health, node).is_some() {",
        "score health lookup",
    )
    marker = "fn extras(already: &[Vec<Claim>], chosen: &[Claim]) -> Vec<Claim> {"
    helper = '''fn degraded_ancestor(
    graph: &Graph,
    node_health: &BTreeMap<NodeId, String>,
    id: NodeId,
) -> Option<NodeId> {
    let degraded = |node: NodeId| node_health.get(&node).map(String::as_str) == Some("degraded");
    if degraded(id) {
        return Some(id);
    }
    graph
        .ancestors(id)
        .into_iter()
        .find(|ancestor| degraded(*ancestor))
}

'''
    text = replace_once(text, marker, helper + marker, "select degraded helper")
    return text


def patch_admit(text: str) -> str:
    text = replace_once(
        text,
        "use crate::select::select;",
        "use crate::select::{select, select_with_health};",
        "admit select import",
    )
    old_ceiling = '''pub fn admit_with_ceiling(
    graph: &Graph,
    occupancy: &Occupancy,
    quarantine: &std::collections::BTreeSet<crate::ids::NodeId>,
    owner_ceiling: &ClassUsage,
    queue: &[Queued],
    leases: &BTreeMap<LeaseId, crate::types::Lease>,
) -> Option<Admission> {
    let usage = owner_usage(graph, leases);
    for index in order_queue(queue) {
        let queued = &queue[index];
        if !within_budget(
            usage.get(&queued.owner).cloned().unwrap_or_default(),
            &queued.request,
            owner_ceiling,
        ) {
            continue;
        }
        if let Ok(allocation) = select(graph, occupancy, &queued.request, quarantine) {
            return Some(Admission {
                request: queued.request.clone(),
                owner: queued.owner,
                allocation,
            });
        }
    }
    None
}'''
    new_ceiling = '''pub fn admit_with_ceiling(
    graph: &Graph,
    occupancy: &Occupancy,
    quarantine: &std::collections::BTreeSet<crate::ids::NodeId>,
    owner_ceiling: &ClassUsage,
    queue: &[Queued],
    leases: &BTreeMap<LeaseId, crate::types::Lease>,
) -> Option<Admission> {
    admit_with_ceiling_health(
        graph,
        occupancy,
        quarantine,
        &BTreeMap::new(),
        owner_ceiling,
        queue,
        leases,
    )
}

pub(crate) fn admit_with_health(
    graph: &Graph,
    occupancy: &Occupancy,
    quarantine: &BTreeSet<NodeId>,
    node_health: &BTreeMap<NodeId, String>,
    queue: &[Queued],
) -> Option<Admission> {
    admit_with_ceiling_health(
        graph,
        occupancy,
        quarantine,
        node_health,
        &ClassUsage::new(),
        queue,
        &BTreeMap::new(),
    )
}

pub(crate) fn admit_with_ceiling_health(
    graph: &Graph,
    occupancy: &Occupancy,
    quarantine: &BTreeSet<NodeId>,
    node_health: &BTreeMap<NodeId, String>,
    owner_ceiling: &ClassUsage,
    queue: &[Queued],
    leases: &BTreeMap<LeaseId, crate::types::Lease>,
) -> Option<Admission> {
    let usage = owner_usage(graph, leases);
    for index in order_queue(queue) {
        let queued = &queue[index];
        if !within_budget(
            usage.get(&queued.owner).cloned().unwrap_or_default(),
            &queued.request,
            owner_ceiling,
        ) {
            continue;
        }
        if let Ok(allocation) =
            select_with_health(graph, occupancy, &queued.request, quarantine, node_health)
        {
            return Some(Admission {
                request: queued.request.clone(),
                owner: queued.owner,
                allocation,
            });
        }
    }
    None
}'''
    text = replace_once(text, old_ceiling, new_ceiling, "health-aware admission")

    old_backfill = '''pub fn admit_backfill_with_exclusions(
    graph: &Graph,
    occupancy: &Occupancy,
    quarantine: &std::collections::BTreeSet<NodeId>,
    exclusions: &RequestExclusions,
    owner_ceiling: &ClassUsage,
    queue: &[Queued],
    ctx: &BackfillCtx<'_>,
) -> Option<Admission> {
    let BackfillCtx {
        now,
        leases,
        open_bindings,
    } = ctx;
    let usage = owner_usage(graph, leases);
    let mut blocked: Vec<BlockedHead> = Vec::new();
    for index in order_queue(queue) {
        let queued = &queue[index];
        if !within_budget(
            usage.get(&queued.owner).cloned().unwrap_or_default(),
            &queued.request,
            owner_ceiling,
        ) {
            continue;
        }
        let mut effective = quarantine.clone();
        let hard = exclusions
            .get(&queued.request.id)
            .cloned()
            .unwrap_or_default();
        effective.extend(hard.iter().copied());
        match select(graph, occupancy, &queued.request, &effective) {
            Ok(allocation) => {
                let claims: BTreeSet<NodeId> =
                    allocation.claims.iter().map(|claim| claim.node).collect();
                let finishes = now.saturating_add(queued.request.lifetime);
                let safe = blocked.iter().all(|head| match head.shadow {
                    Some(shadow) => finishes <= shadow || claims.is_disjoint(&head.shadow_claims),
                    // No finite release is proven: only a job that never
                    // touches the head's nodes can be sure not to delay it.
                    None => claims.is_disjoint(&head.shadow_claims),
                });
                if safe {
                    return Some(Admission {
                        request: queued.request.clone(),
                        owner: queued.owner,
                        allocation,
                    });
                }
            }
            Err(_) => {
                // If removing only the temporary quarantine makes this request
                // fit, it is temporarily blocked and must retain the existing
                // unbounded shadow semantics. Hard execution exclusions remain
                // in force in that projection.
                if let Ok(allocation) = select(graph, occupancy, &queued.request, &hard) {
                    blocked.push(BlockedHead {
                        shadow: None,
                        shadow_claims: claims_by_node(&allocation.claims).into_keys().collect(),
                    });
                } else if let Some(head) = shadow_head(
                    graph,
                    &effective,
                    leases,
                    open_bindings,
                    &queued.request,
                    *now,
                ) {
                    blocked.push(head);
                }
            }
        }
    }
    None
}'''
    new_backfill = '''pub fn admit_backfill_with_exclusions(
    graph: &Graph,
    occupancy: &Occupancy,
    quarantine: &std::collections::BTreeSet<NodeId>,
    exclusions: &RequestExclusions,
    owner_ceiling: &ClassUsage,
    queue: &[Queued],
    ctx: &BackfillCtx<'_>,
) -> Option<Admission> {
    admit_backfill_with_exclusions_health(
        graph,
        occupancy,
        quarantine,
        &BTreeMap::new(),
        exclusions,
        owner_ceiling,
        queue,
        ctx,
    )
}

pub(crate) fn admit_backfill_with_exclusions_health(
    graph: &Graph,
    occupancy: &Occupancy,
    quarantine: &BTreeSet<NodeId>,
    node_health: &BTreeMap<NodeId, String>,
    exclusions: &RequestExclusions,
    owner_ceiling: &ClassUsage,
    queue: &[Queued],
    ctx: &BackfillCtx<'_>,
) -> Option<Admission> {
    let BackfillCtx {
        now,
        leases,
        open_bindings,
    } = ctx;
    let usage = owner_usage(graph, leases);
    let mut blocked: Vec<BlockedHead> = Vec::new();
    for index in order_queue(queue) {
        let queued = &queue[index];
        if !within_budget(
            usage.get(&queued.owner).cloned().unwrap_or_default(),
            &queued.request,
            owner_ceiling,
        ) {
            continue;
        }
        let mut effective = quarantine.clone();
        let hard = exclusions
            .get(&queued.request.id)
            .cloned()
            .unwrap_or_default();
        effective.extend(hard.iter().copied());
        match select_with_health(graph, occupancy, &queued.request, &effective, node_health) {
            Ok(allocation) => {
                let claims: BTreeSet<NodeId> =
                    allocation.claims.iter().map(|claim| claim.node).collect();
                let finishes = now.saturating_add(queued.request.lifetime);
                let safe = blocked.iter().all(|head| match head.shadow {
                    Some(shadow) => finishes <= shadow || claims.is_disjoint(&head.shadow_claims),
                    // No finite release is proven: only a job that never
                    // touches the head's nodes can be sure not to delay it.
                    None => claims.is_disjoint(&head.shadow_claims),
                });
                if safe {
                    return Some(Admission {
                        request: queued.request.clone(),
                        owner: queued.owner,
                        allocation,
                    });
                }
            }
            Err(_) => {
                // If removing only the temporary quarantine makes this request
                // fit, it is temporarily blocked and must retain the existing
                // unbounded shadow semantics. Hard execution exclusions remain
                // in force in that projection.
                if let Ok(allocation) =
                    select_with_health(graph, occupancy, &queued.request, &hard, node_health)
                {
                    blocked.push(BlockedHead {
                        shadow: None,
                        shadow_claims: claims_by_node(&allocation.claims).into_keys().collect(),
                    });
                } else if let Some(head) = shadow_head(
                    graph,
                    &effective,
                    node_health,
                    leases,
                    open_bindings,
                    &queued.request,
                    *now,
                ) {
                    blocked.push(head);
                }
            }
        }
    }
    None
}'''
    text = replace_once(text, old_backfill, new_backfill, "health-aware backfill")
    text = replace_once(
        text,
        "fn shadow_head(\n    graph: &Graph,\n    quarantine: &std::collections::BTreeSet<NodeId>,\n    leases:",
        "fn shadow_head(\n    graph: &Graph,\n    quarantine: &std::collections::BTreeSet<NodeId>,\n    node_health: &BTreeMap<NodeId, String>,\n    leases:",
        "shadow health param",
    )
    text = replace_once(
        text,
        "        if let Ok(allocation) = select(graph, &projected, request, quarantine) {",
        "        if let Ok(allocation) =\n            select_with_health(graph, &projected, request, quarantine, node_health)\n        {",
        "shadow timed health select",
    )
    text = replace_once(
        text,
        "    select(graph, &projected, request, quarantine)\n        .ok()",
        "    select_with_health(graph, &projected, request, quarantine, node_health)\n        .ok()",
        "shadow final health select",
    )
    return text


def patch_lib(text: str) -> str:
    text = replace_once(
        text,
        "        select(&self.graph, &self.occupancy(), request, &blocked)",
        "        crate::select::select_with_health(\n            &self.graph,\n            &self.occupancy(),\n            request,\n            &blocked,\n            &self.node_health,\n        )",
        "cluster allocate health",
    )
    text = replace_once(
        text,
        "        admit(&self.graph, &self.occupancy(), queue, &blocked)",
        "        crate::admit::admit_with_health(\n            &self.graph,\n            &self.occupancy(),\n            &blocked,\n            &self.node_health,\n            queue,\n        )",
        "cluster admit health",
    )
    old = '''        admit_with_ceiling(
            &self.graph,
            &self.occupancy(),
            &blocked,
            owner_ceiling,
            queue,
            &self.leases,
        )'''
    new = '''        crate::admit::admit_with_ceiling_health(
            &self.graph,
            &self.occupancy(),
            &blocked,
            &self.node_health,
            owner_ceiling,
            queue,
            &self.leases,
        )'''
    text = replace_once(text, old, new, "cluster ceiling health")
    old = '''        admit_backfill_with_exclusions(
            &self.graph,
            &self.occupancy(),
            &blocked,
            exclusions,
            owner_ceiling,
            queue,
            &crate::admit::BackfillCtx {
                now: self.now,
                leases: &self.leases,
                open_bindings: &self.open_binding_leases(),
            },
        )'''
    new = '''        crate::admit::admit_backfill_with_exclusions_health(
            &self.graph,
            &self.occupancy(),
            &blocked,
            &self.node_health,
            exclusions,
            owner_ceiling,
            queue,
            &crate::admit::BackfillCtx {
                now: self.now,
                leases: &self.leases,
                open_bindings: &self.open_binding_leases(),
            },
        )'''
    text = replace_once(text, old, new, "cluster backfill health")
    return text


def patch_service(text: str) -> str:
    return replace_once(
        text,
        "    pub fn restore(&mut self, cluster: archon_kernel::Cluster, state: ServiceState) {\n        self.cluster = cluster;",
        "    pub fn restore(&mut self, mut cluster: archon_kernel::Cluster, state: ServiceState) {\n        cluster.migrate_legacy_observations();\n        self.cluster = cluster;",
        "snapshot migration restore",
    )


def patch_server(text: str) -> str:
    return replace_once(text, "            version: 1,", "            version: 2,", "snapshot version")


def patch_select_tests(text: str) -> str:
    text = replace_once(
        text,
        "    Command, Edge, EdgeKind, LeaseId, Need, Node, NodeId, OwnerId, ProviderId, Quantity, Request,",
        "    Command, Edge, EdgeKind, Filter, LeaseId, Need, Node, NodeId, OwnerId, ProviderId, Quantity, Request,",
        "select test Filter import",
    )
    tests = r'''

#[test]
fn health_observation_changes_scoring_without_revising_graph() {
    let mut cluster = graph();
    let revision = cluster.graph.revision;
    cluster
        .apply(Command::SetNodeHealth {
            node: NodeId::from_u64(3),
            health: "degraded".into(),
        })
        .unwrap();

    assert_eq!(cluster.graph.revision, revision);
    assert_eq!(cluster.node_health(NodeId::from_u64(3)), Some("degraded"));
    let allocation = cluster
        .allocate(&request(
            RequestClass::Batch,
            vec![Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![],
            }],
        ))
        .expect("healthy peer should remain placeable");
    assert_eq!(allocation.claims[0].node, NodeId::from_u64(4));

    let replayed = Cluster::replay(&cluster.log).expect("health command must replay");
    assert_eq!(replayed.graph.revision, revision);
    assert_eq!(replayed.node_health(NodeId::from_u64(3)), Some("degraded"));
}

#[test]
fn health_observation_cannot_satisfy_a_hard_filter() {
    let mut cluster = graph();
    cluster
        .apply(Command::SetNodeHealth {
            node: NodeId::from_u64(3),
            health: "degraded".into(),
        })
        .unwrap();
    let error = cluster
        .allocate(&request(
            RequestClass::Batch,
            vec![Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: vec![Filter {
                    key: "health".into(),
                    value: "degraded".into(),
                }],
            }],
        ))
        .unwrap_err();
    assert!(matches!(error, archon_kernel::Error::Refused { .. }));
}

#[test]
fn resource_fact_health_attrs_are_extracted_as_observations() {
    let mut cluster = graph();
    let mut cpu = cluster.graph.node(NodeId::from_u64(3)).unwrap().clone();
    cpu.attrs.insert("health".into(), "degraded".into());
    let revision = cluster.graph.revision;
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![cpu],
            edges: vec![],
        })
        .unwrap();

    assert_eq!(cluster.graph.revision, revision + 1);
    assert_eq!(cluster.node_health(NodeId::from_u64(3)), Some("degraded"));
    assert!(
        cluster.graph.node(NodeId::from_u64(3)).unwrap().attrs.get("health").is_none(),
        "observation state must not remain in revisioned node attrs"
    );
}

#[test]
fn legacy_graph_health_migrates_without_changing_revision() {
    let mut cluster = graph();
    let mut cpu = cluster.graph.node(NodeId::from_u64(3)).unwrap().clone();
    cpu.attrs.insert("health".into(), "degraded".into());
    cluster.graph.apply(vec![cpu], vec![]).unwrap();
    let legacy_revision = cluster.graph.revision;
    assert_eq!(
        cluster
            .graph
            .node(NodeId::from_u64(3))
            .unwrap()
            .attrs
            .get("health")
            .map(String::as_str),
        Some("degraded")
    );

    cluster.migrate_legacy_observations();

    assert_eq!(cluster.graph.revision, legacy_revision);
    assert_eq!(cluster.node_health(NodeId::from_u64(3)), Some("degraded"));
    assert!(
        cluster.graph.node(NodeId::from_u64(3)).unwrap().attrs.get("health").is_none()
    );
}
'''
    return text + tests


write("crates/kernel/src/cluster.rs", patch_cluster)
write("crates/kernel/src/graph.rs", patch_graph)
write("crates/kernel/src/select.rs", patch_select)
write("crates/kernel/src/admit.rs", patch_admit)
write("crates/kernel/src/lib.rs", patch_lib)
write("crates/node/src/service.rs", patch_service)
write("crates/control/src/server.rs", patch_server)
write("crates/kernel/tests/select.rs", patch_select_tests)
