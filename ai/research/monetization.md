# Monetization Strategy

**Updated:** 2026-08-16

Fleet is a planned open-source distributed resource operating system. The core
should be useful for a small organization; commercial value comes from running
production infrastructure safely across more resources, cells, providers, and
failure domains.

## Core principle

> Make the resource and lease substrate visible and usable. Monetize production
> risk, governance, scale, certified integrations, and managed operation.

## Provisional boundaries

| Component | License direction |
|---|---|
| Resource graph, lease, scheduler, node agent, core control plane | ELv2 |
| Basic execution/provider integrations | ELv2 |
| SDKs, API schemas, policy/adapter interfaces, examples | Apache 2.0 |
| Enterprise governance and operations | Proprietary |
| Managed control plane | Proprietary service |

The boundary remains a private release plan pending customer, legal, and
contributor-policy review.

## Likely commercial value

### Enterprise resource operations

- multi-cell and multi-region federation;
- SSO/SAML/SCIM, RBAC, audit export, and policy governance;
- quotas, chargeback, budgets, and cost/energy optimization;
- signed offline releases, provenance, SBOMs, and LTS rollback;
- backup, restore, recovery support bundles, and incident tooling;
- advanced scheduler policies and simulation;
- certified hardware, storage, network, and workload providers.

### Managed control plane

Customers operate node agents on their own infrastructure while Fleet hosts the
global control plane. This requires strong identity, tenant isolation, explicit
data boundaries, offline/degraded behavior, and auditable operations.

### Support and integration

- deployment and migration from Kubernetes, Slurm, or cloud schedulers;
- provider and runtime certification;
- GPU/accelerator, RDMA, storage, and bare-metal integration;
- performance, placement, and failure-recovery tuning;
- production support and incident response.

## Commercial hypothesis

The first buyer is an organization operating expensive, heterogeneous,
privately controlled compute: AI infrastructure, HPC, research, platform, or
specialized datacenter teams. The strongest initial evidence is not scheduler
throughput; it is reduced operational complexity, safer resource ownership,
better placement under topology/locality constraints, and faster recovery.

Avoid pricing by endpoint count. Capacity, cell, site, and support obligations
are more representative of the product's operational value. Validate willingness
to pay only after a real workload uses the lease and recovery model.

## Public launch gates

Before a serious public launch:

- define the core/enterprise component boundary;
- publish security disclosure and contribution policy;
- choose CLA or DCO;
- document telemetry and offline behavior;
- publish provider and runtime compatibility expectations;
- validate that the open core can operate a useful small deployment.
