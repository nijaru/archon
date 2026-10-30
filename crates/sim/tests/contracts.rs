//! Bounded executable specification, not implementation conformance evidence.
//!
//! Run: cargo test -p archon-sim --test contracts -- --nocapture
//! Explores all enabled event orders up to each explicit depth in small fixtures.
//! The reference transitions are independent of Cluster::apply on purpose: the
//! new policy/cutover protocol is not implemented there. Existing simulator
//! tests continue to exercise the real kernel. Passing here proves neither
//! persistence, Linux enforcement, consensus, nor unconditional liveness.

#[path = "contracts/model.rs"]
mod model;

use std::collections::{HashMap, VecDeque};

use model::{Action, Create, Fault, Issuer, Phase, Spec, State, actions};

#[derive(Debug)]
struct Counterexample {
    reason: &'static str,
    trace: Vec<Action>,
}

struct Node {
    state: State,
    parent: Option<(usize, Action)>,
    depth: usize,
}

struct Exploration {
    states: usize,
    transitions: usize,
    frontier: usize,
    // Only computed when no depth frontier remains. Existence of a finishing
    // path from every state is weaker than inevitability under a fair scheduler.
    stranded: Option<[usize; 4]>,
}

fn trace(nodes: &[Node], mut index: usize, last: Action) -> Vec<Action> {
    let mut result = vec![last];
    while let Some((parent, action)) = nodes[index].parent {
        result.push(action);
        index = parent;
    }
    result.reverse();
    result
}

fn check(
    spec: &Spec,
    initial: State,
    actions: &[Action],
    depth: usize,
    fault: Fault,
) -> Result<Exploration, Counterexample> {
    assert_eq!(initial.violation(spec), None, "invalid fixture");
    let mut seen = HashMap::from([(initial.clone(), 0)]);
    let mut nodes = vec![Node {
        state: initial,
        parent: None,
        depth: 0,
    }];
    let mut queue = VecDeque::from([0]);
    let mut reverse = vec![Vec::new()];
    let mut transitions = 0;
    let mut frontier = 0;
    while let Some(index) = queue.pop_front() {
        if nodes[index].depth == depth {
            frontier += 1;
            continue;
        }
        for &action in actions {
            let Some(next) = nodes[index].state.step(spec, action, fault) else {
                continue;
            };
            transitions += 1;
            if let Some(reason) = next.violation(spec) {
                return Err(Counterexample {
                    reason,
                    trace: trace(&nodes, index, action),
                });
            }
            let successor = if let Some(&id) = seen.get(&next) {
                id
            } else {
                let id = nodes.len();
                seen.insert(next.clone(), id);
                nodes.push(Node {
                    state: next,
                    parent: Some((index, action)),
                    depth: nodes[index].depth + 1,
                });
                reverse.push(Vec::new());
                queue.push_back(id);
                assert!(
                    nodes.len() <= 200_000,
                    "explicit model state budget exceeded; narrow the fixture"
                );
                id
            };
            reverse[successor].push(index);
        }
    }
    let stranded = (frontier == 0).then(|| {
        std::array::from_fn(|attempt| {
            let mut reachable = vec![false; nodes.len()];
            let mut queue = VecDeque::new();
            for (i, node) in nodes.iter().enumerate() {
                if node.state.authority.attempts[attempt].phase != Phase::Waiting {
                    reachable[i] = true;
                    queue.push_back(i);
                }
            }
            while let Some(i) = queue.pop_front() {
                for &parent in &reverse[i] {
                    if !reachable[parent] {
                        reachable[parent] = true;
                        queue.push_back(parent);
                    }
                }
            }
            reachable.into_iter().filter(|yes| !yes).count()
        })
    });
    Ok(Exploration {
        states: nodes.len(),
        transitions,
        frontier,
        stranded,
    })
}

fn apply(state: &mut State, spec: &Spec, action: Action) {
    *state = state
        .step(spec, action, Fault::None)
        .unwrap_or_else(|| panic!("disabled {action:?} in {state:?}"));
    assert_eq!(state.violation(spec), None, "after {action:?}");
}

fn grant(state: &mut State, spec: &Spec, i: usize) {
    apply(state, spec, Action::Select(i));
    apply(
        state,
        spec,
        Action::Admit(state.issuer().expect("active authority")),
    );
}

fn close(state: &mut State, spec: &Spec, i: usize) {
    let issuer = state.issuer().expect("active authority");
    apply(state, spec, Action::Cancel(issuer, i));
    apply(state, spec, Action::Stop(i));
    if state.backend[i].create == Create::Unsent {
        apply(state, spec, Action::SendCreate(i));
    }
    if state.backend[i].create == Create::InFlight {
        apply(state, spec, Action::FinishCreate(i));
        apply(state, spec, Action::Stop(i));
    }
    if state.authority.attempts[i].unresolved {
        apply(state, spec, Action::RecordCreate(issuer, i));
    }
    apply(state, spec, Action::ObserveAbsent(i));
    apply(state, spec, Action::Close(issuer, i));
}

fn effect_actions(i: usize) -> Vec<Action> {
    let mut result = vec![
        Action::Restart,
        Action::Seal,
        Action::Import,
        Action::Activate,
    ];
    result.extend([
        Action::SendCreate(i),
        Action::FinishCreate(i),
        Action::Stop(i),
        Action::ObserveAbsent(i),
    ]);
    for issuer in [Issuer::Local, Issuer::Target] {
        result.extend([
            Action::Cancel(issuer, i),
            Action::RecordCreate(issuer, i),
            Action::Close(issuer, i),
        ]);
    }
    result
}

#[test]
fn bounded_cancel_and_cutover_orders_retain_live_and_uncertain_effects() {
    let spec = Spec::small();
    let mut initial = State::new();
    grant(&mut initial, &spec, 0);
    let report = check(&spec, initial, &effect_actions(0), 14, Fault::None)
        .unwrap_or_else(|failure| panic!("{failure:?}"));
    assert!(report.states > 100, "the exploration must not be vacuous");
    assert!(report.transitions > report.states);
    println!(
        "cancel/cutover: states={} edges={} depth=14 frontier={} (bounded, not a liveness proof)",
        report.states, report.transitions, report.frontier
    );
}

#[test]
fn checker_finds_late_create_after_an_absent_lookup_was_treated_as_closure() {
    let spec = Spec::small();
    let mut initial = State::new();
    grant(&mut initial, &spec, 0);
    let failure = match check(
        &spec,
        initial.clone(),
        &effect_actions(0),
        8,
        Fault::EarlyClose,
    ) {
        Err(failure) => failure,
        Ok(_) => panic!("checker did not detect deliberately broken closure"),
    };
    assert_eq!(failure.reason, "unaccounted live or in-flight creation");
    // Replay the shortest counterexample instead of accepting a diagnostic
    // string as proof that the checker exercised the offending transition.
    let mut state = initial;
    for &action in &failure.trace {
        state = state
            .step(&spec, action, Fault::EarlyClose)
            .expect("replayable witness");
    }
    assert!(state.violation(&spec).is_some());
    println!("early-close counterexample: {:?}", failure.trace);
}

#[test]
fn accumulation_survives_cutover_and_blocks_fragment_refill() {
    let spec = Spec::small();
    let mut state = State::new();
    grant(&mut state, &spec, 0);
    grant(&mut state, &spec, 1);
    // The root cap is full, yet the post-release plan must remain eligible.
    assert!(state.step(&spec, Action::Select(2), Fault::None).is_none());
    apply(&mut state, &spec, Action::Wait(1));
    apply(&mut state, &spec, Action::Anchor(2, 0b0011));
    close(&mut state, &spec, 0);
    let checkpoint = state.authority.clone();
    assert!(state.step(&spec, Action::Select(3), Fault::None).is_none());
    // Transfer the still-credible hold between two distinct releases. This
    // checks snapshot contents, not elapsed-time or storage durability.
    apply(&mut state, &spec, Action::Seal);
    apply(&mut state, &spec, Action::Import);
    apply(&mut state, &spec, Action::Activate);
    assert_eq!(state.authority, checkpoint);
    close(&mut state, &spec, 1);
    grant(&mut state, &spec, 2);
    assert_eq!(state.authority.attempts[2].phase, Phase::Granted);
    assert_eq!(state.occupied(&spec), model::ALL);
    assert!(state.authority.anchor.is_none());
}

#[test]
fn checker_detects_an_admission_stealing_accumulated_capacity() {
    let spec = Spec::small();
    let mut initial = State::new();
    grant(&mut initial, &spec, 0);
    grant(&mut initial, &spec, 1);
    apply(&mut initial, &spec, Action::Wait(1));
    apply(&mut initial, &spec, Action::Anchor(2, 0b0011));
    close(&mut initial, &spec, 0);
    let actions = [
        Action::Select(3),
        Action::Admit(Issuer::Local),
        Action::Restart,
    ];
    assert!(check(&spec, initial.clone(), &actions, 3, Fault::None).is_ok());
    let failure = match check(&spec, initial, &actions, 3, Fault::IgnoreAnchor) {
        Err(failure) => failure,
        Ok(_) => panic!("checker did not detect deliberately ignored accumulation exclusion"),
    };
    assert_eq!(failure.reason, "admission stole accumulated capacity");
    assert_eq!(
        failure.trace,
        [Action::Select(3), Action::Admit(Issuer::Local)]
    );
}

#[test]
fn stale_policy_tickets_and_protected_victims_cannot_authorize_admission() {
    let mut spec = Spec::small();
    let mut state = State::new();
    apply(&mut state, &spec, Action::Select(0));
    apply(&mut state, &spec, Action::Restart);
    assert!(
        state
            .step(&spec, Action::Admit(Issuer::Local), Fault::None)
            .is_none()
    );
    apply(&mut state, &spec, Action::Select(0));
    apply(&mut state, &spec, Action::Wait(1));
    assert!(
        state
            .step(&spec, Action::Admit(Issuer::Local), Fault::None)
            .is_none()
    );
    grant(&mut state, &spec, 0);
    spec.reclaimable[0] = false;
    assert!(
        state
            .step(&spec, Action::Anchor(2, 1), Fault::None)
            .is_none()
    );
    // It cannot be anchored without its protected conflicting occupant either.
    assert!(
        state
            .step(&spec, Action::Anchor(2, 0), Fault::None)
            .is_none()
    );
    spec.reclaimable[0] = true;
    apply(&mut state, &spec, Action::Anchor(2, 1));
}

#[test]
fn one_turn_composes_both_pools_and_charges_the_whole_bundle() {
    let mut spec = Spec::small();
    spec.bundles = [[0b01, 0], [0, 0b01], [0b10, 0b10], [0b10, 0b10]];
    spec.projects = [0, 1, 0, 1];
    let mut state = State::new();
    grant(&mut state, &spec, 0);
    grant(&mut state, &spec, 1);
    // Opposite per-pool rankings: A owns pool P, B owns pool Q. The
    // domain's aggregate-share tie has exactly one round-robin winner.
    assert!(state.step(&spec, Action::Select(3), Fault::None).is_none());
    grant(&mut state, &spec, 2);
    assert_eq!(state.occupied(&spec), model::ALL);
    assert!(state.step(&spec, Action::Select(3), Fault::None).is_none());
    // A cap in either dimension refuses the entire bundle, not half of it.
    let mut capped = State::new();
    spec.caps[0] = [2, 0];
    assert!(capped.step(&spec, Action::Select(2), Fault::None).is_none());
    assert_eq!(capped.authority.attempts[2].phase, Phase::Waiting);
    grant(&mut capped, &spec, 0);
}

#[test]
fn weighted_current_share_not_projected_request_size_selects_the_project() {
    let mut spec = Spec::small();
    // P0 owns one unit, P1 owns none and asks for two in the other pool.
    spec.bundles = [[1, 0], [2, 0], [0, 3], [0, 1]];
    let mut state = State::new();
    grant(&mut state, &spec, 0);
    assert!(state.step(&spec, Action::Select(1), Fault::None).is_none());
    apply(&mut state, &spec, Action::Select(2));
    // A retained exact turn is invalidated by any committed policy progress.
    apply(&mut state, &spec, Action::Wait(0));
    assert!(
        state
            .step(&spec, Action::Admit(Issuer::Local), Fault::None)
            .is_none()
    );
    grant(&mut state, &spec, 2);
}

#[test]
fn cutover_preserves_intent_and_cannot_timeout_back_to_local_writes() {
    let spec = Spec::small();
    let mut state = State::new();
    grant(&mut state, &spec, 0);
    apply(&mut state, &spec, Action::SendCreate(0));
    apply(&mut state, &spec, Action::Seal);
    let sealed = state.authority.clone();
    apply(&mut state, &spec, Action::Import);
    // The backend can complete while neither controller may mutate state.
    apply(&mut state, &spec, Action::FinishCreate(0));
    assert!(
        state
            .step(&spec, Action::RecordCreate(Issuer::Local, 0), Fault::None)
            .is_none()
    );
    apply(&mut state, &spec, Action::Activate);
    assert_eq!(state.authority, sealed);
    assert!(
        state
            .step(&spec, Action::Cancel(Issuer::Local, 0), Fault::None)
            .is_none()
    );
    assert!(
        state
            .step(&spec, Action::ReopenLocal, Fault::None)
            .is_none()
    );
    apply(&mut state, &spec, Action::RecordCreate(Issuer::Target, 0));
    assert!(state.backend[0].live);
    close(&mut state, &spec, 0);
    let failure = match check(&spec, state, &[Action::ReopenLocal], 1, Fault::ReopenSource) {
        Err(failure) => failure,
        Ok(_) => panic!("checker did not detect a deliberately reopened source"),
    };
    assert_eq!(failure.reason, "sealed source reopened");
}

#[test]
fn bounded_admission_reclaim_and_cutover_interleavings() {
    let spec = Spec::small();
    let mut initial = State::new();
    grant(&mut initial, &spec, 0);
    apply(&mut initial, &spec, Action::Wait(1));
    apply(&mut initial, &spec, Action::Anchor(2, 1));
    let mut events = effect_actions(0);
    events.extend([
        Action::Select(2),
        Action::Select(3),
        Action::Admit(Issuer::Local),
        Action::Admit(Issuer::Target),
    ]);
    let report = check(&spec, initial, &events, 15, Fault::None)
        .unwrap_or_else(|failure| panic!("{failure:?}"));
    assert_eq!(
        report.stranded.map(|counts| counts[2]),
        Some(0),
        "every state in the exhausted anchored graph must retain a path to complete admission"
    );
    println!(
        "anchored admission: states={} edges={} depth=15 frontier={}",
        report.states, report.transitions, report.frontier
    );
}

#[test]
fn bounded_competing_admissions_with_the_full_action_alphabet() {
    let spec = Spec::small();
    let report = check(&spec, State::new(), &actions(), 7, Fault::None)
        .unwrap_or_else(|failure| panic!("{failure:?}"));
    assert!(
        report.frontier > 0,
        "this larger exploration is intentionally depth bounded"
    );
    println!(
        "all events: states={} edges={} depth=7 frontier={}",
        report.states, report.transitions, report.frontier
    );
}

#[test]
fn accumulation_protects_shared_quota_even_on_physically_disjoint_units() {
    let mut spec = Spec::small();
    spec.bundles = [[1, 0], [0, 1], [2, 0], [1, 0]];
    spec.root_cap = [1, 2];
    spec.projects = [0, 1, 1, 0];
    let mut state = State::new();
    grant(&mut state, &spec, 0);
    grant(&mut state, &spec, 1);
    close(&mut state, &spec, 1);
    // The next ordinary tie belongs to P0 after P1's completed admission.
    apply(&mut state, &spec, Action::Wait(1));
    // The target's actual unit is free. Its only blocker is shared quota,
    // so a physically disjoint reclaimable victim must still be eligible.
    apply(&mut state, &spec, Action::Anchor(2, 1));
    close(&mut state, &spec, 0);
    // Refilling the released victim unit would steal the target's quota.
    assert!(state.step(&spec, Action::Select(3), Fault::None).is_none());
    let failure = match check(
        &spec,
        state.clone(),
        &[Action::Select(3), Action::Admit(Issuer::Local)],
        2,
        Fault::IgnoreAnchor,
    ) {
        Err(failure) => failure,
        Ok(_) => panic!("checker did not detect stolen projected quota"),
    };
    assert_eq!(failure.reason, "admission stole projected quota headroom");
    grant(&mut state, &spec, 2);
}

#[test]
fn weight_and_entitlement_change_current_project_order_without_changing_capacity() {
    let mut spec = Spec::small();
    spec.bundles = [[1, 0], [0, 1], [2, 0], [0, 2]];
    spec.projects = [0, 1, 0, 1];
    let mut state = State::new();
    grant(&mut state, &spec, 0);
    grant(&mut state, &spec, 1);
    for (weights, entitlement, winner) in [
        ([1, 1], [[0, 0], [0, 0]], 2),
        ([1, 2], [[0, 0], [0, 0]], 3),
        ([1, 1], [[0, 0], [0, 1]], 3),
    ] {
        spec.weights = weights;
        spec.entitlement = entitlement;
        assert!(
            state
                .step(&spec, Action::Select(winner), Fault::None)
                .is_some()
        );
        let loser = if winner == 2 { 3 } else { 2 };
        assert!(
            state
                .step(&spec, Action::Select(loser), Fault::None)
                .is_none()
        );
    }
}
