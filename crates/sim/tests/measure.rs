//! Scale-measurement invariants: the report path places every request,
//! drains the queue, replays to the same digest, reproduces identical
//! structural figures across runs, and keeps log growth per allocation
//! bounded. No wall-clock figure is asserted anywhere.

use archon_sim::{ScaleParams, measure_scale};

const PARAMS: ScaleParams = ScaleParams {
    machines: 8,
    requests: 24,
};

#[test]
fn scale_report_places_everything_and_replays() {
    let report = measure_scale(&PARAMS);
    assert_eq!(report.placed, PARAMS.requests, "every request must place");
    assert!(report.queue_drained, "queue must drain");
    assert!(
        report.replay_matches,
        "trace must replay to the same digest"
    );
}

#[test]
fn scale_report_structural_figures_are_deterministic() {
    let first = measure_scale(&PARAMS);
    let second = measure_scale(&PARAMS);
    assert_eq!(first.placed, second.placed);
    assert_eq!(first.log_commands, second.log_commands);
    assert_eq!(first.epoch, second.epoch);
    assert_eq!(first.graph_revision, second.graph_revision);
    assert_eq!(first.leases, second.leases);
    assert_eq!(first.bindings, second.bindings);
    assert_eq!(first.log_per_alloc, second.log_per_alloc);
}

#[test]
fn log_growth_per_alloc_stays_bounded() {
    // Regression tripwire against accidental unbounded growth in the
    // command log, not a performance claim: current measured value is
    // ~7.7 commands per allocation, stable across cluster sizes. The 4x
    // headroom absorbs legitimate lifecycle additions without hiding a
    // structural blowup.
    let report = measure_scale(&PARAMS);
    assert!(
        report.log_per_alloc < 32.0,
        "log_per_alloc={} exceeds tripwire; investigate log growth",
        report.log_per_alloc
    );
}
