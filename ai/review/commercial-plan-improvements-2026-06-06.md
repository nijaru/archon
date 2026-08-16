---
date: 2026-06-06
summary: Commercial and source-boundary improvements to Fleet's plan and spec.
status: active
---

# Commercial Plan Improvements

## Verdict

The plan should not treat monetization as a distant enterprise appendix. Fleet's
public/source boundary affects the architecture from the first public release.

The right default remains:

> Public source-available core under ELv2, Apache 2.0 SDKs/APIs/examples, and
> paid production/enterprise/managed-control-plane offerings later.

The correction is that the free core must be genuinely useful. Do not cripple
the single-fleet product to force upgrades. Charge for production seriousness,
governance, scale, support, and managed operations.

## Improvements To The Plan

### 1. Add a commercial boundary gate before public launch

Before the public repo is pushed as a serious project, Fleet should have:

- exact license headers and component boundaries;
- CLA or DCO decision;
- trademark/project-name policy;
- security disclosure policy;
- telemetry stance: no required outbound telemetry;
- contribution policy;
- free vs paid feature boundary.

This is not legal polish. Infrastructure buyers care about these early, and
retroactive licensing or contributor cleanup is expensive.

### 2. Keep the single-fleet core free and useful

The ELv2 core should include enough to solve a real problem:

- one organization;
- one control plane;
- one or more GPU nodes;
- direct node agent;
- vLLM runtime adapter;
- endpoint lifecycle;
- basic gateway/request path;
- basic scheduler;
- basic cache metadata;
- basic health/log/usage/cost visibility;
- CLI and minimal UI.

Do not paywall the first useful workflow. If the core feels like a demo, the
project will not earn trust.

### 3. Charge for operational risk, not basic access

Paid features should map to production risk:

- HA control plane;
- SSO/SAML/SCIM;
- advanced RBAC;
- audit export;
- support bundles;
- air-gapped install;
- multi-fleet and multi-region;
- advanced quota/budget and chargeback;
- fleet-wide upgrades;
- managed control plane;
- certified runtime/provider integrations;
- support/SLA and deployment help.

### 4. Treat services as the first revenue path

The first dollars are likely to come from paid implementation/support, not a
self-serve SaaS checkout:

- deploy Fleet on a customer's GPU node;
- benchmark against their current vLLM/KServe/custom setup;
- tune runtime and placement;
- document savings, failure recovery, and operator time saved;
- convert to annual support/enterprise if the workload is real.

### 5. Price by managed accelerator count, with a floor

The clean first enterprise pricing unit is managed GPU/accelerator count plus a
minimum annual contract. It matches buyer value better than endpoint count or
request count because Fleet manages expensive hardware capacity.

Keep exact numbers out of public docs until discovery validates them. Internally
use simple anchors:

- small production support/design partner: low five figures annually;
- real platform team deployment: mid five to low six figures annually;
- managed control plane or multi-fleet enterprise: custom.

### 6. Validate ELv2 before betting the company on it

ELv2 is still the right default because it protects against managed-service
resale while allowing internal use. But it is not open source, and some buyers
or contributors will reject it.

Ask prospects explicitly:

- Would ELv2 block internal evaluation?
- Would Apache SDKs/API schemas be enough to integrate?
- Would source review under NDA satisfy procurement?
- Would AGPL/MPL/BSL materially improve adoption?

## Evidence That Would Change The Commercial Plan

- Early users refuse ELv2 before trying the product.
- The project needs broad community contribution more than hosted-service
  protection.
- The first buyers only want paid support and never ask for enterprise features.
- Cloud/GPU providers want partnership/OEM terms before the product has direct
  enterprise pull.
- Managed control plane demand appears before self-hosted enterprise demand.
