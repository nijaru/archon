use archon_kernel::{
    CapacityDimension, Need, OwnerId, Request, RequestClass, RequestId, ResourceClass, qty,
};
use archon_node::agent::LeaseAgent;
use archon_node::discover::MachineDescription;
use archon_node::protocol::{AgentRequest, AgentResponse};
use archon_node::runtime::ProcessRuntime;
use archon_node::service::{LeaseExecutor, LocalExecutor, NodeService};

fn machine(instance: &str) -> MachineDescription {
    MachineDescription {
        instance_id: instance.into(),
        name: instance.into(),
        cpus: 1,
        memory_bytes: 1 << 30,
        host_nodes: Vec::new(),
        devices: Vec::new(),
    }
}

fn cpu_request(id: u64) -> Request {
    Request {
        id: RequestId::from_u64(id),
        class: RequestClass::Batch,
        needs: vec![Need {
            kind: ResourceClass::Cpu,
            quantity: qty(CapacityDimension::Count, 1),
            filters: Vec::new(),
        }],
        topology: Vec::new(),
        preferences: Vec::new(),
        data: Vec::new(),
        command: vec!["true".into()],
        image: None,
        storage: Vec::new(),
        ports: Vec::new(),
        lifetime: 60,
        priority: 1,
        machine_local: true,
        grace_secs: 0,
        keep_alive: false,
    }
}

#[test]
fn lifecycle_only_process_reports_no_resource_enforcement() {
    let mut agent = LeaseAgent::new(ProcessRuntime::new());
    let AgentResponse::Capabilities { capabilities } = agent.handle(AgentRequest::Capabilities)
    else {
        panic!("capability response");
    };
    assert!(capabilities.process.available);
    assert!(!capabilities.process.cpu_limit);
    assert!(!capabilities.process.memory_limit);
    assert!(!capabilities.process.device_isolation);
    assert!(!capabilities.process.physical_cpu_placement);
    assert!(!capabilities.process.numa_memory_placement);
}

#[cfg(target_os = "linux")]
#[test]
fn configured_but_unusable_cgroup_root_reports_no_resource_enforcement() {
    let runtime = ProcessRuntime::new()
        .with_cgroup_root("/proc/archon-capability-proof-must-not-exist".into());
    let capabilities = runtime.capabilities();
    assert!(capabilities.available);
    assert!(!capabilities.cpu_limit);
    assert!(!capabilities.memory_limit);
    assert!(!capabilities.device_isolation);
}

#[cfg(target_os = "linux")]
#[test]
fn unusable_cgroup_configuration_is_excluded_before_cpu_lease_authority() {
    let mut service = NodeService::new();
    let runtime = ProcessRuntime::new()
        .with_cgroup_root("/proc/archon-capability-authority-proof-must-not-exist".into());
    service
        .register_agent(
            machine("unusable-cgroup"),
            Box::new(LocalExecutor::new(LeaseAgent::new(runtime))),
        )
        .expect("registration");
    service.submit(cpu_request(1), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), None);
    assert!(
        service.cluster.leases.is_empty(),
        "unproven enforcement must exclude the machine before Lease authority"
    );
}

#[test]
fn lifecycle_only_process_is_excluded_before_cpu_lease_authority() {
    let mut service = NodeService::new();
    service
        .register_agent(
            machine("lifecycle-only"),
            Box::new(LocalExecutor::new(LeaseAgent::new(ProcessRuntime::new()))),
        )
        .expect("registration");
    service.submit(cpu_request(1), OwnerId::from_u64(1));
    assert_eq!(service.admit_one().unwrap(), None);
    assert!(service.cluster.leases.is_empty());
}

struct LegacyExecutor;

impl LeaseExecutor for LegacyExecutor {
    fn execute(&mut self, _request: AgentRequest) -> Result<AgentResponse, String> {
        Ok(AgentResponse::Welcome {
            instance_id: "legacy".into(),
            name: "legacy".into(),
            cpus: 1,
            memory_bytes: 1 << 30,
            host_nodes: Vec::new(),
            devices: Vec::new(),
        })
    }
}

#[test]
fn registration_refuses_an_agent_that_cannot_prove_capabilities() {
    let mut service = NodeService::new();
    let error = service
        .register_agent(machine("legacy"), Box::new(LegacyExecutor))
        .expect_err("legacy capability ambiguity must fail closed");
    assert!(
        error
            .to_string()
            .contains("did not report execution capabilities")
    );
    assert!(
        service
            .cluster
            .graph
            .nodes_of_class(ResourceClass::Machine)
            .is_empty(),
        "capability proof happens before Graph mutation"
    );
}
