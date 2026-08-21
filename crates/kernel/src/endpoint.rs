use crate::ids::BindingId;
use crate::types::{Endpoint, EndpointPhase};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointOp {
    Prepare,
    Activate,
    Release,
    Fence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EndpointError {
    StaleSession { expected: u64, got: u64 },
    StaleFence { accepted: u64, got: u64 },
    Conflict,
    WrongPhase { phase: EndpointPhase },
}

impl Endpoint {
    pub fn apply(
        &mut self,
        op: EndpointOp,
        binding: BindingId,
        fence: u64,
        session: u64,
    ) -> Result<(), EndpointError> {
        if session != self.session {
            return Err(EndpointError::StaleSession {
                expected: self.session,
                got: session,
            });
        }
        match op {
            EndpointOp::Prepare => self.prepare(binding, fence),
            EndpointOp::Activate => self.activate(binding, fence),
            EndpointOp::Release | EndpointOp::Fence => self.close(fence),
        }
    }

    pub fn handshake(&mut self, session: u64) {
        // Sessions are process generations; an older hello never reinstates.
        if session > self.session {
            self.session = session;
        }
    }

    fn prepare(&mut self, binding: BindingId, fence: u64) -> Result<(), EndpointError> {
        if fence < self.accepted_fence {
            return Err(EndpointError::StaleFence {
                accepted: self.accepted_fence,
                got: fence,
            });
        }
        if fence == self.accepted_fence {
            if self.open && self.binding == Some(binding) && self.phase == EndpointPhase::Prepared {
                return Ok(());
            }
            return Err(EndpointError::Conflict);
        }
        self.accepted_fence = fence;
        self.open = true;
        self.binding = Some(binding);
        self.phase = EndpointPhase::Prepared;
        Ok(())
    }

    fn activate(&mut self, binding: BindingId, fence: u64) -> Result<(), EndpointError> {
        if !self.open || self.binding != Some(binding) || fence != self.accepted_fence {
            return Err(EndpointError::Conflict);
        }
        if !matches!(self.phase, EndpointPhase::Prepared | EndpointPhase::Active) {
            return Err(EndpointError::WrongPhase { phase: self.phase });
        }
        self.phase = EndpointPhase::Active;
        Ok(())
    }

    fn close(&mut self, fence: u64) -> Result<(), EndpointError> {
        if fence < self.accepted_fence {
            return Ok(());
        }
        self.accepted_fence = fence;
        self.open = false;
        self.binding = None;
        self.phase = EndpointPhase::Idle;
        Ok(())
    }
}
