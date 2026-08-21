---
date: 2026-06-06
summary: Historical evaluation of Fleet's former GPU-fleet product plan.
status: superseded
---

# Fleet Plan Evaluation

> Historical review. On 2026-08-16 Fleet's canonical direction became the
> distributed resource operating system in [`../design/DISTRIBUTED_RESOURCE_OS.md`](../design/DISTRIBUTED_RESOURCE_OS.md).
> The GPU-fleet framing below is preserved as history, not current scope.

## Verdict

Fleet is worth spending time on, but the long-term plan should be tightened
around a specific control-plane thesis:

> Fleet should own the model-to-hardware control loop for private GPU fleets:
> desired state, node execution, placement, cache/cost/health reasoning,
> lifecycle explanations, and operator workflow.

Fleet can compete commercially with KServe, Run:ai/KAI, Dynamo, llm-d, Baseten,
and custom vLLM stacks. It should not start by reimplementing their deepest
runtime, gateway, or Kubernetes-native distributed inference internals. The
from-scratch advantage is a coherent product model and direct-agent execution
path, not a claim that all lower-level subsystems will be better on day one.

## What Is Strong

- **Model-first object model.** `Model -> ModelVersion -> Endpoint -> Replica`
  plus `Runtime`, `HardwarePool`, policy, quota, cache, health, and usage is
  the right product surface. It avoids exposing pods/jobs as the user's main
  mental model.
- **Direct node agent.** The agent -> containerd -> runtime container path is
  the strongest architectural distinction. It gives Fleet a credible no-K8s
  path for owned, colo, and cloud VM GPUs.
- **Hardware-location agnostic design.** Treating bare metal, colo, cloud VMs,
  and hybrid pools as metadata on `HardwarePool` is more durable than choosing
  a single deployment substrate.
- **Runtime adapter model.** Fleet should manage vLLM, SGLang, TensorRT-LLM,
  MAX, Dynamo, and future engines instead of becoming an inference engine.
- **Scheduler/router shared state.** The distinctive product value is combining
  model, hardware, cache, latency, quota, cost, and health state into placement,
  scaling, and routing decisions.
- **Explanations as UX.** "Why did this model run here?", "why did cost spike?",
  and "why is this GPU idle?" are not dashboard polish. They are the operator
  product.

## What Needed Correction

### 1. The MVP was too large

The previous MVP included control plane, node agent, GPU telemetry, model
cache, vLLM, gateway, autoscaling, quotas, cost tracking, health, logs, metrics,
UI, and CLI. That is not a useful MVP definition. It mixes proof, core product,
and production hardening.

Fleet now needs three nearer gates:

1. **Proof slice:** one control plane, one direct node agent, one GPU node, one
   vLLM container, one OpenAI-compatible request, health/log evidence, and one
   failure/recovery path.
2. **Single-fleet MVP:** model registry, endpoint lifecycle, simple scheduler,
   telemetry, model cache, usage/cost basics, CLI-first operator workflow, and
   minimal UI.
3. **Production core:** rollouts, warm pools, scale-to-zero, quota/budget
   enforcement, stronger gateway, support bundles, and explainable operations.

### 2. Gateway should not be a primary early moat

OpenAI-compatible gateway behavior is necessary, but generic LLM gateway
features are already a crowded layer. Fleet's early gateway should be a minimal
data path that records usage, streams reliably, and routes to healthy replicas.

Do not build a broad provider gateway, prompt gateway, MCP gateway, or
provider-fallback product before Fleet has proven node execution and placement.
If Envoy AI Gateway or a runtime-native frontend is sufficient for a slice, use
or front it. Fleet's value is the control-plane state around the route.

### 3. Dynamo should be both competitor and backend

Dynamo and llm-d are threats if Fleet tries to own distributed inference
internals. They are also backend opportunities if Fleet owns the private fleet
management layer above them.

Correct long-term posture:

- vLLM first for the proof and MVP.
- Dynamo support later as a managed runtime/backend option, not a feature to
  clone.
- Fleet owns model lifecycle, hardware pools, cost, quota, cache metadata,
  operator workflow, and deployment policy across runtime backends.

### 4. UI is important, but CLI/dry-run/explanations come first

The UI should be operator-focused, but the first UX proof should be CLI-first:

- `endpoint plan` with placement/cost/cache explanation.
- `endpoint create` that starts a real replica.
- `node describe` with GPU/container/cache/health state.
- `endpoint explain` or equivalent evidence for placement, route, scale, and
  failure decisions.

A polished dashboard is valuable after the underlying explanations exist.

### 5. NATS should be long-term infrastructure, not proof-slice dependency

NATS JetStream is reasonable for durable events, async commands, replay, and
multi-node operations. It should not be required to prove the direct-agent
model-serving loop.

For the proof slice, a direct ConnectRPC/HTTP control path or simple persistent
stream is enough. Add NATS when multiple nodes, replay, async lifecycle events,
or log/metric fanout need it.

### 6. Single-org trusted v1 should be explicit

The first version should assume one organization and trusted workloads. It
still needs node registration, mTLS or a development equivalent, runtime image
allowlists, and clear secret handling. It should defer hostile multi-tenancy,
strong sandboxing, and public-cloud isolation.

## Updated Long-Term Shape

### Phase A: Direct-Agent Proof

Goal: prove Fleet can run a real model on a real GPU node with less operational
surface than a Kubernetes/KServe stack.

Must prove:

- control plane starts locally;
- GPU node joins with a token;
- node reports inventory and basic health;
- agent pulls and starts a vLLM container through containerd;
- GPU is exposed correctly;
- OpenAI-compatible request reaches the model;
- logs and health are visible;
- container crash is detected and either repaired or clearly explained.

Non-goals:

- multi-tenant auth;
- UI;
- cost model sophistication;
- autoscaling;
- Dynamo/KServe parity;
- advanced KV routing.

### Phase B: Single-Fleet MVP

Goal: make Fleet useful for one team operating a small private GPU fleet.

Own:

- model and endpoint lifecycle;
- direct node agent;
- NVIDIA inventory and telemetry;
- simple VRAM/cache/health-aware placement;
- local model cache metadata;
- basic OpenAI-compatible gateway or runtime frontend integration;
- usage and cost estimates;
- CLI-first workflows;
- minimal web UI for models, endpoints, nodes, GPUs, health, and cost.

Still defer:

- distributed training;
- fine-tuning execution;
- Kubernetes/Slurm/Ray adapters;
- managed control plane;
- advanced gateway/provider features;
- prefill/decode disaggregation unless delegated to a runtime backend.

### Phase C: Production Inference Core

Goal: become credible for production private inference operations.

Add:

- rollouts and rollback;
- canary and traffic split;
- warm pools and scale-to-zero;
- quota and budget enforcement;
- failure-aware placement;
- support bundles;
- audit log;
- stronger secrets and image policy;
- repeatable benchmark suite against bare vLLM and Kubernetes baselines.

### Phase D: Advanced Runtime Management

Goal: manage the best runtime stack for each workload without becoming every
runtime.

Add:

- SGLang adapter;
- MAX adapter if there is a concrete Modular path;
- TensorRT-LLM adapter;
- Dynamo backend integration;
- cache-aware routing metadata where runtime support exists;
- prefill/decode or KV-cache features through runtime-specific integrations.

### Phase E: Enterprise and Managed Control Plane

Goal: monetize production seriousness.

Add:

- HA control plane;
- SSO/SAML/SCIM;
- advanced RBAC;
- audit export;
- air-gapped packages;
- multi-fleet and multi-region;
- managed control plane with customer-owned node agents;
- cloud/provider integrations;
- support/SLA and production upgrade workflows.

## Design Principles To Keep Front And Center

1. **Do not expose pod/job thinking as the product model.**
2. **Reuse runtime engines. Own runtime choice, deployment, and operations.**
3. **Optimize operator time, cold starts, placement quality, cost, and failure
   clarity before chasing lower-level inference internals.**
4. **Every scheduler/router decision should be explainable from recorded state.**
5. **The first customer segment is teams operating private NVIDIA GPUs without a
   mature Kubernetes ML platform.**
6. **If the K8s-native path is already great for a customer, Fleet should not
   try to win that account first.**

## Evidence That Would Change The Plan

- Teams with private GPUs say Kubernetes/KServe/Dynamo is not painful enough to
  switch or trial Fleet.
- A direct-agent containerd path is less reliable or more fragile than expected
  under NVIDIA driver/runtime conditions.
- Model cache and cold-start improvements are not measurable versus bare vLLM
  or Kubernetes baselines.
- Operators care more about managed inference convenience than private fleet
  control.
- Enterprises reject ELv2 strongly enough that adoption suffers before product
  value is visible.

## Immediate Planning Updates

- Treat the current spec as the full product vision, not the MVP.
- Replace Sprint 1 with a direct-agent proof slice.
- Move UI, autoscaling, quotas, and advanced gateway behavior behind proof of
  endpoint lifecycle and explainable placement.
- Treat NATS as a multi-node/event durability tool, not a first proof
  requirement.
- Treat Dynamo as a future runtime/backend integration and a benchmark target,
  not an early reimplementation target.
