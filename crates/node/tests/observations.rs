use archon_kernel::{Cluster, Command, Node, NodeId, Quantity, ResourceClass};
use archon_node::service::{NodeService, ServiceState};

#[test]
fn restore_migrates_legacy_health_attrs_out_of_resource_facts() {
    let mut cluster = Cluster::new();
    cluster
        .apply(Command::ApplyGraph {
            nodes: vec![Node {
                id: NodeId::from_u64(1),
                kind: ResourceClass::Machine,
                attrs: Default::default(),
                capacity: Quantity::new(),
            }],
            edges: Vec::new(),
        })
        .unwrap();
    let revision = cluster.graph.revision;

    let mut value = serde_json::to_value(&cluster).unwrap();
    let graph = value
        .get_mut("graph")
        .and_then(serde_json::Value::as_object_mut)
        .expect("serialized Graph object");
    graph.remove("observations");
    let nodes = graph
        .get_mut("nodes")
        .and_then(serde_json::Value::as_object_mut)
        .expect("serialized Node map");
    let legacy = nodes.values_mut().next().expect("one serialized Node");
    legacy
        .get_mut("attrs")
        .and_then(serde_json::Value::as_object_mut)
        .expect("serialized Node attrs")
        .insert(
            "health".into(),
            serde_json::Value::String("degraded".into()),
        );

    let restored: Cluster = serde_json::from_value(value).unwrap();
    assert_eq!(
        restored
            .graph
            .node(NodeId::from_u64(1))
            .unwrap()
            .attrs
            .get("health")
            .map(String::as_str),
        Some("degraded"),
        "fixture must represent the old snapshot layout"
    );

    let mut service = NodeService::new();
    service.restore(restored, ServiceState::default());
    assert_eq!(service.cluster.graph.revision, revision);
    assert_eq!(
        service
            .cluster
            .graph
            .observation(NodeId::from_u64(1), "health"),
        Some("degraded")
    );
    assert!(
        !service
            .cluster
            .graph
            .node(NodeId::from_u64(1))
            .unwrap()
            .attrs
            .contains_key("health")
    );
}
