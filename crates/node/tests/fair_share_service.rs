use std::collections::BTreeMap;

use archon_kernel::{
    AdmissionPolicy, CapacityDimension, Need, OwnerId, Request, RequestClass, RequestId,
    ResourceClass, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::discover::MachineDescription;
use archon_node::protocol::{ExecutionCapabilities, RuntimeCapabilities};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::{LocalAgentClient, NodeService};

fn request(id: u64) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: ResourceClass::Cpu,
            quantity: qty(CapacityDimension::Count, 1),
            filters: vec![],
        }],
        topology: vec![],
        preferences: vec![],
        data: vec![],
        lifetime: 100,
        priority: 5,
        machine_local: true,
    }
}

#[test]
fn node_service_uses_fair_share_before_lease_commit() {
    let mut service = NodeService::new();
    let description = MachineDescription {
        instance_id: "fair-share-machine".into(),
        name: "fair-share-machine".into(),
        cpus: 4,
        memory_bytes: 0,
        host_nodes: vec![],
        devices: vec![],
    };
    let executor = LocalAgentClient::new(LeaseAgent::new(ProcessRuntime::new()));
    service
        .register_agent_with_capabilities(
            description,
            Box::new(executor),
            ExecutionCapabilities {
                process: RuntimeCapabilities {
                    available: true,
                    cpu_limit: true,
                    memory_limit: false,
                    device_isolation: false,
                    physical_cpu_placement: false,
                    numa_memory_placement: false,
                },
                container: RuntimeCapabilities::default(),
            },
        )
        .expect("register proof machine");

    let policy = AdmissionPolicy {
        fair_share: true,
        owner_weights: BTreeMap::new(),
        ..AdmissionPolicy::default()
    };

    service.submit(request(1), OwnerId::from_u64(1));
    assert_eq!(
        service.admit_one_with_policy(&policy).unwrap(),
        Some(RequestId::from_u64(1))
    );

    service.submit(request(2), OwnerId::from_u64(1));
    service.submit(request(3), OwnerId::from_u64(2));
    assert_eq!(
        service.admit_one_with_policy(&policy).unwrap(),
        Some(RequestId::from_u64(3)),
        "the owner with no current dominant share should be admitted first"
    );
}
