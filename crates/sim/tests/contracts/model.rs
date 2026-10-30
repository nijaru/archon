//! Abstract reference transitions, NOT the production controller or a backend emulator.
//! Two pools of two exclusive units; four immutable attempts and two projects.
//! Grant combines reservation and authorization of one pre-prepared creation effect.
//! No clocks, applications, topology search, disk, quorum algorithm or real fencing.

pub type Bundle = [u8; 2];
pub const EMPTY: Bundle = [0, 0];
pub const ALL: Bundle = [0b11, 0b11];

fn intersects(a: Bundle, b: Bundle) -> bool {
    a.into_iter().zip(b).any(|(a, b)| a & b != 0)
}

fn union(a: Bundle, b: Bundle) -> Bundle {
    [a[0] | b[0], a[1] | b[1]]
}

pub fn counts(bundle: Bundle) -> [u64; 2] {
    bundle.map(|mask| u64::from(mask.count_ones()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Phase {
    Waiting,
    Granted,
    Cancelled,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Create {
    Unsent,
    InFlight,
    Applied,
    Rejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Attempt {
    pub phase: Phase,
    // Durable intent remains unresolved until a result is recorded, even if
    // the external creation has already happened and its reply was lost.
    pub unresolved: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Backend {
    pub create: Create,
    pub live: bool,
    pub reject: bool,
    pub observed_absent: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Anchor {
    pub target: usize,
    pub victims: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Authority {
    pub attempts: [Attempt; 4],
    pub anchor: Option<Anchor>,
    pub turn: usize,
    pub waited: [bool; 2],
    pub revision: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Local,
    Sealed,
    Imported,
    Replicated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Issuer {
    Local,
    Target,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ticket {
    pub attempt: usize,
    pub revision: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct State {
    pub authority: Authority,
    pub backend: [Backend; 4],
    pub mode: Mode,
    pub imported: Option<Authority>,
    // The only volatile state. Restart can discard a selected proposal,
    // never its accepted intent, cancellation, anchor or rejection frontier.
    pub ticket: Option<Ticket>,
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub bundles: [Bundle; 4],
    pub projects: [usize; 4],
    pub reclaimable: [bool; 4],
    pub caps: [[u64; 2]; 2],
    pub root_cap: [u64; 2],
    pub entitlement: [[u64; 2]; 2],
    pub weights: [u64; 2],
}

impl Spec {
    pub fn small() -> Self {
        Self {
            bundles: [[0b01, 0b01], [0b10, 0b10], ALL, [0b01, 0b01]],
            projects: [0, 0, 1, 0],
            reclaimable: [true; 4],
            caps: [[2; 2]; 2],
            root_cap: [2; 2],
            entitlement: [[0; 2]; 2],
            weights: [1; 2],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Select(usize),
    Admit(Issuer),
    Wait(usize),
    Anchor(usize, u8),
    Cancel(Issuer, usize),
    SendCreate(usize),
    FinishCreate(usize),
    RecordCreate(Issuer, usize),
    Stop(usize),
    ObserveAbsent(usize),
    Close(Issuer, usize),
    Restart,
    Seal,
    Import,
    Activate,
    ReopenLocal,
}

// Deliberately broken reference semantics used only to check the checker's
// sensitivity. These are not production compatibility modes or options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    None,
    EarlyClose,
    IgnoreAnchor,
    ReopenSource,
}

impl State {
    pub fn new() -> Self {
        Self {
            authority: Authority {
                attempts: [Attempt {
                    phase: Phase::Waiting,
                    unresolved: false,
                }; 4],
                anchor: None,
                turn: 0,
                waited: [false; 2],
                revision: 0,
            },
            backend: [Backend {
                create: Create::Unsent,
                live: false,
                reject: false,
                observed_absent: false,
            }; 4],
            mode: Mode::Local,
            imported: None,
            ticket: None,
        }
    }

    pub fn issuer(&self) -> Option<Issuer> {
        match self.mode {
            Mode::Local => Some(Issuer::Local),
            Mode::Replicated => Some(Issuer::Target),
            Mode::Sealed | Mode::Imported => None,
        }
    }

    fn occupies(&self, i: usize) -> bool {
        matches!(
            self.authority.attempts[i].phase,
            Phase::Granted | Phase::Cancelled
        )
    }

    pub fn occupied(&self, spec: &Spec) -> Bundle {
        (0..4)
            .filter(|&i| self.occupies(i))
            .fold(EMPTY, |a, i| union(a, spec.bundles[i]))
    }

    fn usage(&self, spec: &Spec, project: Option<usize>, except: u8) -> [u64; 2] {
        let mut used = [0; 2];
        for i in 0..4 {
            if self.occupies(i)
                && except & (1 << i) == 0
                && project.is_none_or(|p| spec.projects[i] == p)
            {
                for (total, amount) in used.iter_mut().zip(counts(spec.bundles[i])) {
                    *total += amount;
                }
            }
        }
        used
    }

    fn caps_fit(&self, spec: &Spec, targets: &[usize], releases: u8) -> bool {
        for project in [None, Some(0), Some(1)] {
            let mut used = self.usage(spec, project, releases);
            for &i in targets {
                if project.is_none_or(|p| spec.projects[i] == p) {
                    for (total, amount) in used.iter_mut().zip(counts(spec.bundles[i])) {
                        *total += amount;
                    }
                }
            }
            let cap = project.map_or(spec.root_cap, |p| spec.caps[p]);
            if used.into_iter().zip(cap).any(|(x, cap)| x > cap) {
                return false;
            }
        }
        true
    }

    fn contributes(&self, spec: &Spec, target: usize, victim: usize) -> bool {
        if intersects(spec.bundles[target], spec.bundles[victim]) {
            return true;
        }
        let demand = counts(spec.bundles[target]);
        let freed = counts(spec.bundles[victim]);
        // A physically disjoint victim can contribute shared ancestor/project
        // quota headroom. Do not confuse pool placement with quota feasibility.
        [None, Some(spec.projects[target])]
            .into_iter()
            .any(|project| {
                if project.is_some_and(|p| p != spec.projects[victim]) {
                    return false;
                }
                let used = self.usage(spec, project, 0);
                let cap = project.map_or(spec.root_cap, |p| spec.caps[p]);
                (0..2).any(|r| freed[r] > 0 && used[r] + demand[r] > cap[r])
            })
    }

    fn fits_now(&self, spec: &Spec, i: usize, fault: Fault) -> bool {
        let blocked = self.authority.anchor.is_some_and(|anchor| {
            anchor.target != i
                && (intersects(spec.bundles[anchor.target], spec.bundles[i])
                    || !self.caps_fit(spec, &[anchor.target, i], anchor.victims))
        });
        self.authority.attempts[i].phase == Phase::Waiting
            && (fault == Fault::IgnoreAnchor || !blocked)
            && !intersects(self.occupied(spec), spec.bundles[i])
            && self.caps_fit(spec, &[i], 0)
    }

    fn ordinary_choice(&self, spec: &Spec, fault: Fault) -> Option<usize> {
        // Equal reference capacities (two units) cancel from this rational
        // comparison. No floating point and no projected-request share.
        let score = |p: usize| {
            self.usage(spec, Some(p), 0)
                .into_iter()
                .zip(spec.entitlement[p])
                .map(|(x, e)| x.saturating_sub(e))
                .max()
                .expect("two resource dimensions")
        };
        let candidates: [Option<usize>; 2] = std::array::from_fn(|p| {
            (0..4).find(|&i| spec.projects[i] == p && self.fits_now(spec, i, fault))
        });
        match candidates {
            [Some(a), Some(b)] => {
                let left = score(0) * spec.weights[1];
                let right = score(1) * spec.weights[0];
                Some(
                    if left < right || (left == right && self.authority.turn == 0) {
                        a
                    } else {
                        b
                    },
                )
            }
            [a, b] => a.or(b),
        }
    }

    fn changed(&mut self) {
        self.authority.revision = self
            .authority
            .revision
            .checked_add(1)
            .expect("finite model transitions fit u8");
    }

    pub fn step(&self, spec: &Spec, action: Action, fault: Fault) -> Option<Self> {
        let mut next = self.clone();
        match action {
            Action::Select(i) => {
                if self.issuer().is_none() || !self.fits_now(spec, i, fault) {
                    return None;
                }
                let anchored = self.authority.anchor.is_some_and(|a| a.target == i);
                if !anchored && self.ordinary_choice(spec, fault) != Some(i) {
                    return None;
                }
                next.ticket = Some(Ticket {
                    attempt: i,
                    revision: self.authority.revision,
                });
            }
            Action::Admit(issuer) => {
                let ticket = self.ticket?;
                let i = ticket.attempt;
                if self.issuer() != Some(issuer)
                    || ticket.revision != self.authority.revision
                    || !self.fits_now(spec, i, fault)
                {
                    return None;
                }
                next.authority.attempts[i] = Attempt {
                    phase: Phase::Granted,
                    unresolved: true,
                };
                next.authority.turn = 1 - spec.projects[i];
                if self.authority.anchor.is_some_and(|a| a.target == i) {
                    next.authority.anchor = None;
                }
                next.ticket = None;
                next.changed();
            }
            Action::Wait(p) => {
                self.issuer()?;
                if !self.authority.waited[p] {
                    next.authority.waited[p] = true;
                    next.changed();
                }
            }
            Action::Anchor(i, victims) => {
                if self.issuer().is_none()
                    || self.authority.anchor.is_some()
                    || !self.authority.waited[spec.projects[i]]
                    || self.authority.attempts[i].phase != Phase::Waiting
                {
                    return None;
                }
                let mut retained = EMPTY;
                for j in 0..4 {
                    if victims & (1 << j) != 0 {
                        if !self.occupies(j)
                            || !spec.reclaimable[j]
                            || !self.contributes(spec, i, j)
                        {
                            return None;
                        }
                    } else if self.occupies(j) {
                        retained = union(retained, spec.bundles[j]);
                    }
                }
                if victims >= 16
                    || intersects(retained, spec.bundles[i])
                    || !self.caps_fit(spec, &[i], victims)
                {
                    return None;
                }
                next.authority.anchor = Some(Anchor { target: i, victims });
                next.changed();
            }
            Action::Cancel(issuer, i) => {
                if self.issuer() != Some(issuer)
                    || self.authority.attempts[i].phase != Phase::Granted
                {
                    return None;
                }
                next.authority.attempts[i].phase = Phase::Cancelled;
                next.changed();
            }
            Action::SendCreate(i) => {
                // Dispatch can outlive its issuer, and controller cancellation
                // can reach an isolated endpoint after this committed command.
                if !self.authority.attempts[i].unresolved
                    || self.backend[i].create != Create::Unsent
                {
                    return None;
                }
                next.backend[i].create = if self.backend[i].reject {
                    Create::Rejected
                } else {
                    Create::InFlight
                };
            }
            Action::FinishCreate(i) => {
                if self.backend[i].create != Create::InFlight {
                    return None;
                }
                next.backend[i].create = Create::Applied;
                next.backend[i].live = true;
                next.backend[i].observed_absent = false;
            }
            Action::RecordCreate(issuer, i) => {
                if self.issuer() != Some(issuer)
                    || !self.authority.attempts[i].unresolved
                    || !matches!(self.backend[i].create, Create::Applied | Create::Rejected)
                {
                    return None;
                }
                next.authority.attempts[i].unresolved = false;
                next.changed();
            }
            Action::Stop(i) => {
                if self.authority.attempts[i].phase != Phase::Cancelled {
                    return None;
                }
                next.backend[i].reject = true;
                next.backend[i].live = false;
            }
            Action::ObserveAbsent(i) => {
                if self.backend[i].live {
                    return None;
                }
                next.backend[i].observed_absent = true;
            }
            Action::Close(issuer, i) => {
                if self.issuer() != Some(issuer)
                    || self.authority.attempts[i].phase != Phase::Cancelled
                    || !self.backend[i].observed_absent
                    || !self.backend[i].reject
                {
                    return None;
                }
                if fault != Fault::EarlyClose
                    && (self.authority.attempts[i].unresolved || self.backend[i].live)
                {
                    return None;
                }
                next.authority.attempts[i].phase = Phase::Closed;
                next.changed();
            }
            Action::Restart => next.ticket = None,
            Action::Seal => {
                if self.mode != Mode::Local {
                    return None;
                }
                next.mode = Mode::Sealed;
                next.ticket = None;
            }
            Action::Import => {
                if self.mode != Mode::Sealed {
                    return None;
                }
                next.imported = Some(self.authority.clone());
                next.mode = Mode::Imported;
            }
            Action::Activate => {
                if self.mode != Mode::Imported {
                    return None;
                }
                next.authority = self.imported.clone().expect("import precedes activation");
                next.mode = Mode::Replicated;
            }
            Action::ReopenLocal => {
                if fault != Fault::ReopenSource || self.mode != Mode::Replicated {
                    return None;
                }
                next.mode = Mode::Local;
            }
        }
        (next != *self).then_some(next)
    }

    pub fn violation(&self, spec: &Spec) -> Option<&'static str> {
        for i in 0..4 {
            if (self.backend[i].live || self.backend[i].create == Create::InFlight)
                && !self.occupies(i)
            {
                return Some("unaccounted live or in-flight creation");
            }
            for j in (i + 1)..4 {
                if self.occupies(i)
                    && self.occupies(j)
                    && intersects(spec.bundles[i], spec.bundles[j])
                {
                    return Some("overlapping exclusive grants");
                }
            }
        }
        if self.imported.is_some() && self.mode == Mode::Local {
            return Some("sealed source reopened");
        }
        if let Some(anchor) = self.authority.anchor {
            // Even a physically free fragment must stay excluded from new
            // nonvictim grants until this exact complete bundle can be admitted.
            for i in 0..4 {
                if self.occupies(i)
                    && i != anchor.target
                    && anchor.victims & (1 << i) == 0
                    && intersects(spec.bundles[i], spec.bundles[anchor.target])
                {
                    return Some("admission stole accumulated capacity");
                }
            }
            if !self.caps_fit(spec, &[anchor.target], anchor.victims) {
                return Some("admission stole projected quota headroom");
            }
        }
        for p in 0..2 {
            if self
                .usage(spec, Some(p), 0)
                .into_iter()
                .zip(spec.caps[p])
                .any(|(x, cap)| x > cap)
            {
                return Some("project cap exceeded");
            }
        }
        if self
            .usage(spec, None, 0)
            .into_iter()
            .zip(spec.root_cap)
            .any(|(x, cap)| x > cap)
        {
            return Some("ancestor cap exceeded");
        }
        None
    }
}

pub fn actions() -> Vec<Action> {
    let mut actions = vec![
        Action::Restart,
        Action::Seal,
        Action::Import,
        Action::Activate,
        Action::ReopenLocal,
    ];
    for p in 0..2 {
        actions.push(Action::Wait(p));
    }
    for i in 0..4 {
        actions.extend([
            Action::Select(i),
            Action::SendCreate(i),
            Action::FinishCreate(i),
            Action::Stop(i),
            Action::ObserveAbsent(i),
        ]);
        for issuer in [Issuer::Local, Issuer::Target] {
            actions.extend([
                Action::Admit(issuer),
                Action::Cancel(issuer, i),
                Action::RecordCreate(issuer, i),
                Action::Close(issuer, i),
            ]);
        }
        for victims in 0..16 {
            actions.push(Action::Anchor(i, victims));
        }
    }
    actions
}
