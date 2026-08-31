use archon_kernel::{
    Allocation, NodeId, PlacementExplanation, PlacementReason,
};

#[test]
fn legacy_string_explanation_deserializes_without_typed_reasons() {
    let json = r#"{"claims":[],"graph_revision":7,"explanation":"legacy placement summary"}"#;
    let allocation: Allocation = serde_json::from_str(json).expect("deserialize legacy allocation");

    assert_eq!(allocation.graph_revision, 7);
    assert_eq!(allocation.explanation.summary, "legacy placement summary");
    assert!(allocation.explanation.reasons.is_empty());
}

#[test]
fn structured_explanation_round_trips_typed_reasons() {
    let allocation = Allocation {
        claims: Vec::new(),
        graph_revision: 9,
        explanation: PlacementExplanation::new(
            "selected machine",
            vec![PlacementReason::MachineSelected {
                machine: NodeId::from_u64(42),
            }],
        ),
    };

    let json = serde_json::to_string(&allocation).expect("serialize structured allocation");
    let restored: Allocation = serde_json::from_str(&json).expect("deserialize structured allocation");

    assert_eq!(restored, allocation);
    assert!(json.contains("MachineSelected"));
    assert!(json.contains("selected machine"));
}
