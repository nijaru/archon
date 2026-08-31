use archon_kernel::{
    CapacityDimension, Need, Request, RequestClass, RequestId, ResourceClass, qty,
};
use archon_node::workload::WorkloadSpec;

#[test]
fn legacy_flat_request_json_decodes_as_workload_spec() {
    let legacy = serde_json::json!({
        "id": 7,
        "class": "Service",
        "needs": [{"kind":"cpu","quantity":{"count":1},"filters":[]}],
        "topology": [], "preferences": [], "data": [],
        "command": ["sleep", "30"], "image": "busybox:latest",
        "storage": [{"host_path":"/tmp/input","mount_path":"/input"}],
        "ports": [{"container_port":8080,"host_port":18080}],
        "lifetime": 60, "priority": 4, "keep_alive": true,
        "machine_local": true, "grace_secs": 5
    });
    let workload: WorkloadSpec = serde_json::from_value(legacy).expect("legacy workload shape");
    assert_eq!(workload.resources.id, RequestId::from_u64(7));
    assert_eq!(workload.resources.class, RequestClass::Service);
    assert_eq!(
        workload.resources.needs,
        vec![Need {
            kind: ResourceClass::Cpu,
            quantity: qty(CapacityDimension::Count, 1),
            filters: Vec::new()
        }]
    );
    assert_eq!(workload.execution.command, vec!["sleep", "30"]);
    assert_eq!(workload.execution.image.as_deref(), Some("busybox:latest"));
    assert_eq!(workload.execution.storage.len(), 1);
    assert_eq!(workload.execution.ports.len(), 1);
    assert_eq!(workload.execution.grace_secs, 5);
    assert!(workload.keep_alive);
    let encoded = serde_json::to_value(&workload).expect("new workload shape");
    assert_eq!(encoded["id"], 7);
    assert_eq!(encoded["command"], serde_json::json!(["sleep", "30"]));
    assert!(encoded.get("resources").is_none());
}

#[test]
fn pure_resource_request_promotes_without_execution_intent() {
    let request = Request {
        id: RequestId::from_u64(8),
        class: RequestClass::Batch,
        needs: Vec::new(),
        topology: Vec::new(),
        preferences: Vec::new(),
        data: Vec::new(),
        lifetime: 10,
        priority: 1,
        machine_local: true,
    };
    let workload = WorkloadSpec::resource_only(request.clone());
    assert_eq!(workload.resources, request);
    assert!(!workload.execution.has_program());
    assert!(!workload.keep_alive);
}
