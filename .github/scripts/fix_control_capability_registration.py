from pathlib import Path


def replace(path: str, old: str, new: str, count: int = 1) -> None:
    p = Path(path)
    text = p.read_text()
    if old not in text:
        raise SystemExit(f"expected snippet not found in {path}: {old[:180]!r}")
    p.write_text(text.replace(old, new, count))

# Split capability proof from graph registration. register_agent remains the
# synchronous convenience API; the control plane can prove a dial-in agent on
# its connection thread, then take the shared lock only for state mutation.
old = '''    pub fn register_agent(
        &mut self,
        description: crate::discover::MachineDescription,
        mut executor: Box<dyn LeaseExecutor>,
    ) -> Result<NodeId, Error> {
        let capabilities = match executor
            .execute(AgentRequest::Capabilities)
            .map_err(|reason| Error::Refused {
                explanation: format!("agent capability query failed: {reason}"),
            })? {
            AgentResponse::Capabilities { capabilities } => capabilities,
            other => {
                return Err(Error::Refused {
                    explanation: format!("agent did not report execution capabilities: {other:?}"),
                });
            }
        };
        crate::discover::validate_machine_description(&description)
'''
new = '''    pub fn query_execution_capabilities(
        executor: &mut dyn LeaseExecutor,
    ) -> Result<ExecutionCapabilities, Error> {
        match executor
            .execute(AgentRequest::Capabilities)
            .map_err(|reason| Error::Refused {
                explanation: format!("agent capability query failed: {reason}"),
            })? {
            AgentResponse::Capabilities { capabilities } => Ok(capabilities),
            other => Err(Error::Refused {
                explanation: format!("agent did not report execution capabilities: {other:?}"),
            }),
        }
    }

    pub fn register_agent(
        &mut self,
        description: crate::discover::MachineDescription,
        mut executor: Box<dyn LeaseExecutor>,
    ) -> Result<NodeId, Error> {
        let capabilities = Self::query_execution_capabilities(executor.as_mut())?;
        self.register_agent_with_capabilities(description, executor, capabilities)
    }

    /// Register an agent whose execution guarantees were already proven on
    /// its connection thread. Capability proof still precedes all Graph
    /// mutation, but a silent network peer never holds a shared controller
    /// lock while the proof waits or times out.
    pub fn register_agent_with_capabilities(
        &mut self,
        description: crate::discover::MachineDescription,
        executor: Box<dyn LeaseExecutor>,
        capabilities: ExecutionCapabilities,
    ) -> Result<NodeId, Error> {
        crate::discover::validate_machine_description(&description)
'''
replace("crates/node/src/service.rs", old, new)

# Dial-in capability proof happens before acquiring the ControlPlane mutex.
old = '''                if let Err(err) = this.lock().unwrap().register_dial_in(stream, description) {
                    eprintln!("archon: agent {peer} registration failed: {err}");
                } else {
                    eprintln!("archon: agent {peer} disconnected");
                }
'''
new = '''                let mut executor = archon_node::service::RemoteExecutor::from_secure(stream);
                let capabilities = match archon_node::service::NodeService::query_execution_capabilities(
                    &mut executor,
                ) {
                    Ok(capabilities) => capabilities,
                    Err(err) => {
                        eprintln!("archon: agent {peer} capability proof failed: {err}");
                        return;
                    }
                };
                if let Err(err) = this
                    .lock()
                    .unwrap()
                    .register_dial_in(executor, description, capabilities)
                {
                    eprintln!("archon: agent {peer} registration failed: {err}");
                } else {
                    eprintln!("archon: agent {peer} disconnected");
                }
'''
replace("crates/control/src/server.rs", old, new)

old = '''    fn register_dial_in(
        &mut self,
        stream: archon_node::transport::SecureStream,
        description: archon_node::discover::MachineDescription,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let executor = archon_node::service::RemoteExecutor::from_secure(stream);
        let machine = self
            .service
            .register_agent(description, Box::new(executor))?;
'''
new = '''    fn register_dial_in(
        &mut self,
        executor: archon_node::service::RemoteExecutor,
        description: archon_node::discover::MachineDescription,
        capabilities: archon_node::protocol::ExecutionCapabilities,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let machine = self.service.register_agent_with_capabilities(
            description,
            Box::new(executor),
            capabilities,
        )?;
'''
replace("crates/control/src/server.rs", old, new)
