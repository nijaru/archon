---
date: 2026-07-10
status: superseded
summary: Historical review of Fleet's former runtime-neutral inference framing.
---

# Runtime-Neutral Evidence Gate

> Historical review. On 2026-08-16 Fleet's canonical direction became the
> distributed resource operating system in [`../design/DISTRIBUTED_RESOURCE_OS.md`](../design/DISTRIBUTED_RESOURCE_OS.md).
> The inference-focused framing below is preserved as history, not current scope.

## Position

Fleet remains a general-purpose private GPU fleet product. Its useful technical
boundary is above, not inside, inference runtimes: normalize deployment,
hardware inventory, lifecycle, benchmark evidence, telemetry, capacity, cost,
and recovery across runtimes such as vLLM, MAX, SGLang, TensorRT-LLM, and later
Dynamo.

This is not a claim that Fleet owns all routing or scheduling. NVIDIA Dynamo
already provides engine-agnostic distributed-inference components across vLLM,
SGLang, and TensorRT-LLM, with a Kubernetes-native production control plane.
Fleet must complement such systems where a customer needs one operational view
and a simpler deployment/recovery path across heterogeneous runtime choices.

## Shared Contract To Prove

Define a versioned runtime adapter contract with four surfaces:

1. **Capability:** supported model families, hardware, parallelism, cache,
   metrics, health, and upgrade constraints.
2. **Lifecycle:** resolve artifact digests, prepare host/cache, start, health
   check, drain, stop, upgrade, rollback, and support-bundle collection.
3. **Observation:** normalized request, latency, queue, utilization, VRAM,
   model-load, cache, error, and version evidence while preserving runtime
   specific metrics.
4. **Benchmark:** one declarative model/hardware/traffic manifest with recorded
   runtime version, configuration, output, cost assumptions, and recovery run.

OpenAI compatibility makes request tests portable; it is insufficient for
deployment, capacity, metrics, and lifecycle equivalence.

## Sequencing

1. Complete the vLLM direct-agent proof on real hardware.
2. Implement the adapter contract and persisted benchmark/support artifact,
   without building a generic scheduler.
3. Add MAX as the second adapter only after vLLM has a passing lifecycle proof.
   Compare the same model, hardware, traffic trace, and recovery drill.
4. Add SGLang, TensorRT-LLM, or Dynamo only when the comparison shows a
   customer-relevant runtime choice.

MAX is a good second adapter because it already exposes an OpenAI-compatible
server and its benchmark CLI supports MAX, vLLM, SGLang, and TRT-LLM backends.
This makes a common evidence format plausible, but a Modular partnership is not
an architectural dependency.

## Continue Criteria

Continue Fleet only if the proof shows at least one material improvement over
direct runtime operation: faster/surer deployment or recovery, lower operator
time, better utilization/cost at equivalent quality/SLO, or a runtime choice a
team cannot otherwise manage safely. A dashboard or scheduler without that
evidence is insufficient.

## Sources

- MAX serving: https://docs.modular.com/stable/max/cli/serve/
- MAX benchmark: https://docs.modular.com/max/cli/benchmark/
- NVIDIA Dynamo overview: https://docs.nvidia.com/dynamo/getting-started/introduction
- NVIDIA Dynamo Kubernetes deployment: https://docs.nvidia.com/dynamo/dev/getting-started/kubernetes-deployment
- vLLM Prometheus metrics: https://docs.vllm.ai/en/latest/api/vllm/v1/metrics/prometheus/
