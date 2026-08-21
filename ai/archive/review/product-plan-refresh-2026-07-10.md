---
date: 2026-07-10
status: superseded
summary: Historical review of Fleet's former private-GPU product framing.
---

# Product Plan Refresh

> Historical review. On 2026-08-16 Fleet's canonical direction became the
> distributed resource operating system in [`../design/DISTRIBUTED_RESOURCE_OS.md`](../design/DISTRIBUTED_RESOURCE_OS.md).
> The GPU/direct-node framing below is preserved as history, not current scope.

## Verdict

The former plan was technically coherent but needed an evidence gate before
broad platform scope. Fleet remains a general-purpose private GPU fleet
operations product. The direct-node, no-Kubernetes, and offline-capable path is
the first proof wedge because it isolates Fleet's shared operational core; it is
not a permanent market, product, or architecture boundary.

## Why This Is Plausible

- NVIDIA's self-hosted Run:ai requires Kubernetes for its control plane and
  managed clusters.
- llm-d, Kueue, and KAI Scheduler focus on Kubernetes-native inference,
  scheduling, routing, and resource management.
- A direct node agent plus containerd/CDI can test the different workflow
  without rebuilding an inference engine.

This is an unproven market gap, not a conclusion that a category is empty.

## Required Proof

Before Fleet builds a broad control plane, a design partner must demonstrate a
real owned-GPU deployment, identify a concrete failure in its existing
direct-node, Kubernetes, or managed workflow, validate the lifecycle proof, and
pay or commit to paid deployment/support. The proof may use signed/offline
deployment plus upgrade/rollback/recovery where that is relevant. A pure UX
preference is insufficient.

## License and Revenue

Keep ELv2 core and Apache SDK/API boundaries provisionally. Do not choose AGPL
as a reflex: it requires source offers for modified network programs and does
not prevent unmodified hosted competition. The commercial unit should begin
with paid deployment/support, then annual per-site subscription plus accelerator
capacity bands. Offline artifacts, provenance, upgrades, recovery, and support
are early product evidence for the first wedge; they cannot be deferred when a
target deployment requires them.

## Sources

- NVIDIA Run:ai self-hosted installation: https://run-ai-docs.nvidia.com/self-hosted/getting-started/installation
- llm-d: https://github.com/llm-d/llm-d
- Kueue overview: https://kueue.sigs.k8s.io/docs/overview/
- KAI Scheduler: https://github.com/kai-scheduler/KAI-Scheduler
- GNU AGPL-3.0: https://www.gnu.org/licenses/agpl-3.0.en.html
- Elastic License 2.0 FAQ: https://www.elastic.co/licensing/elastic-license/faq/
