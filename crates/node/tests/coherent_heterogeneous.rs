use std::collections::{BTreeMap, BTreeSet};

use archon_kernel::{
    CapacityDimension, Error, Need, Request, RequestClass, RequestId, ResourceClass,
    TopologyConstraint, TopologyRelation, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::discover::{DeviceSpec, HostNodeSpec, MachineDescription};
use archon_node::protocol::{ExecutionCapabilities, RuntimeCapabilities};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::{LocalExecutor, NodeService};

const GIB: u64 = 1 << 30;

fn caps() -> ExecutionCapabilities {
    ExecutionCapabilities {
        process: RuntimeCapabilities {
            available: true,
            cpu_limit: true,
            memory_limit: true,
            device_isolation: true,
            physical_cpu_placement: false,
            numa_memory_placement: false,
        },
        container: RuntimeCapabilities::default(),
    }
}

fn host_node(
    id: &str,
    kind: ResourceClass,
    parent: Option<&str>,
    capacity: archon_kernel::Quantity,
) -> HostNodeSpec {
    HostNodeSpec {
        id: id.into(),
        kind,
        parent: parent.map(str::to_owned),
        attrs: BTreeMap::new(),
        capacity,
    }
}

fn description(local_memory_gib: u64) -> MachineDescription {
    MachineDescription {
        instance_id: format!("coherent-{local_memory_gib}"),
        name: "heterogeneous-proof".into(),
        cpus: 2,
        memory_bytes: 16 * GIB,
        host_nodes: vec![
            host_node("00-numa0", ResourceClass::Numa, None, BTreeMap::new()),
            host_node(
                "01-cpu0",
                ResourceClass::Cpu,
                Some("00-numa0"),
                qty(CapacityDimension::Count, 1),
            ),
            host_node(
                "02-mem0",
                ResourceClass::Memory,
                Some("00-numa0"),
                qty(CapacityDimension::Bytes, (16 - local_memory_gib) * GIB),
            ),
            host_node("10-numa1", ResourceClass::Numa, None, BTreeMap::new()),
            host_node(
                "11-cpu1",
                ResourceClass::Cpu,
                Some("10-numa1"),
                qty(CapacityDimension::Count, 1),
            ),
            host_node(
                "12-mem1",
                ResourceClass::Memory,
                Some("10-numa1"),
                qty(CapacityDimension::Bytes, local_memory_gib * GIB),
            ),
        ],
        devices: vec![DeviceSpec {
            kind: ResourceClass::Gpu,
            id: "gpu0".into(),
            dev: "/dev/null".into(),
            host_parent: Some("10-numa1".into()),
            access: Vec::new(),
            attrs: BTreeMap::from([("provider".into(), "proof".into())]),
        }],
    }
}

fn register(local_memory_gib: u64) -> NodeService {
    let mut service = NodeService::new();
    let executor = LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()));
    service
        .register_agent_with_capabilities(
            description(local_memory_gib),
            Box::new(executor),
            caps(),
        )
        .expect("normalized heterogeneous machine registration");
    service
}

fn request(memory_gib: u64) -> Request {
    Request {
        id: RequestId::from_u64(1),
        class: RequestClass::Batch,
        needs: vec![
            Need {
                kind: ResourceClass::Cpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: Vec::new(),
            },
            Need {
                kind: ResourceClass::Memory,
                quantity: qty(CapacityDimension::Bytes, memory_gib * GIB),
                filters: Vec::new(),
            },
            Need {
                kind: ResourceClass::Gpu,
                quantity: qty(CapacityDimension::Count, 1),
                filters: Vec::new(),
            },
        ],
        topology: vec![
            TopologyConstraint {
                left: 0,
                right: 2,
                relation: TopologyRelation::SameAncestor {
                    class: ResourceClass::Numa,
                },
            },
            TopologyConstraint {
                left: 1,
                right: 2,
                relation: TopologyRelation::SameAncestor {
                    class: ResourceClass::Numa,
                },
            },
        ],
        preferences: Vec::new(),
        data: Vec::new(),
        command: Vec::new(),
        image: None,
        storage: Vec::new(),
        ports: Vec::new(),
        lifetime: 60,
        priority: 1,
        keep_alive: false,
        machine_local: true,
        grace_secs: 0,
    }
}

#[test]
fn cpu_memory_and_gpu_allocate_as_one_numa_local_request() {
    let service = register(8);
    let allocation = service
        .cluster
        .allocate(&request(4))
        .expect("NUMA1 has enough CPU, memory, and GPU capacity");

    assert_eq!(allocation.claims.len(), 3);
    let numa: BTreeSet<_> = allocation
        .claims
        .iter()
        .map(|claim| {
            service
                .cluster
                .graph
                .ancestor_of_class(claim.node, ResourceClass::Numa)
                .expect("every requested resource is NUMA-contained")
        })
        .collect();
    assert_eq!(numa.len(), 1, "all heterogeneous claims must share one NUMA node");

    let kinds: BTreeSet<_> = allocation
        .claims
        .iter()
        .map(|claim| service.cluster.graph.node(claim.node).expect("claim node").kind)
        .collect();
    assert_eq!(
        kinds,
        BTreeSet::from([
            ResourceClass::Cpu,
            ResourceClass::Memory,
            ResourceClass::Gpu,
        ])
    );
    assert!(
        allocation.explanation.contains("same-ancestor(numa) constrained placement"),
        "material topology must be visible in the allocation explanation: {}",
        allocation.explanation
    );
}

#[test]
fn globally_available_resources_are_refused_when_numa_locality_cannot_be_met() {
    // The GPU and one CPU live under NUMA1, but NUMA1 has only 1 GiB of
    // memory. NUMA0 has the other 15 GiB. Every Need is individually
    // satisfiable on the machine, but no allocation can satisfy the two hard
    // SameAncestor(NUMA) relationships for a 4 GiB memory request.
    let service = register(1);

    assert_eq!(
        service
            .cluster
            .graph
            .nodes_of_class(ResourceClass::Gpu)
            .len(),
        1
    );
    assert!(
        service
            .cluster
            .graph
            .nodes_of_class(ResourceClass::Memory)
            .iter()
            .any(|id| service.cluster.graph.node(*id).is_some_and(|node| {
                node.capacity
                    .get(&CapacityDimension::Bytes)
                    .is_some_and(|bytes| *bytes >= 4 * GIB)
            })),
        "sufficient memory exists globally"
    );

    let error = service
        .cluster
        .allocate(&request(4))
        .expect_err("hard NUMA locality must prevent cross-domain resource stitching");
    let Error::Refused { explanation } = error else {
        panic!("expected explained placement refusal, got {error}");
    };
    assert!(
        explanation.contains("topology") || explanation.contains("numa"),
        "refusal should identify the hard locality problem: {explanation}"
    );
}
