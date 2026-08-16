# Fleet License Boundary

**Status:** private planning decision; not the current release license
**Updated:** 2026-08-16

Fleet is currently private inside `omendb/monorepo`. The checked-in
`LICENSE` currently records that this is private pre-release software and does
not grant public use rights. It is not the final public release text.

## Planned public boundary

Use **AGPL-3.0-or-later** for the Fleet resource authority and tightly coupled
core:

- resource graph implementation;
- lease, epoch, fencing, and binding logic;
- allocation plans and cell state machines;
- global/cell/native schedulers;
- node agent and reconciliation;
- built-in execution/provider implementations;
- simulator and correctness tooling coupled to core semantics;
- Fleet CLI when it is coupled to the core.

Use **Apache-2.0** for integration surfaces intended for broad independent
implementation:

- wire schemas and protocol definitions;
- client SDKs;
- provider SDKs and extension interfaces;
- generated clients;
- standalone examples and interoperability tooling where practical.

The boundary should remain technically clean. Prefer out-of-process provider
protocols when a vendor or operator needs to implement an integration without
linking proprietary code into the AGPL core.

## Why AGPL first

Fleet is a networked control plane. AGPL keeps modified network-accessible core
implementations available to the users of those deployments while allowing
commercial operation, support, certification, and hosted services.

AGPL does not prevent proprietary clients from using a documented protocol, nor
does it prevent commercial licenses or services. It does create more adoption
friction than Apache-2.0, especially for OEMs and vendors that want to embed
Fleet code directly.

Starting AGPL preserves the option to offer a separate commercial license later
if the project owns the necessary copyright rights. Code first released under a
permissive license cannot generally be made exclusive afterward.

## Before public release

- replace the scaffold license with the selected AGPL text;
- add SPDX identifiers and copyright ownership policy;
- choose DCO, CLA, or another contribution-rights mechanism;
- decide whether future commercial relicensing requires explicit contributor
  rights;
- inventory dependency, generated-code, fixture, dataset, and documentation
  licenses;
- define documentation and trademark licenses separately;
- publish security disclosure, contribution, and telemetry policies.

This is a release boundary, not a reason to split the private monorepo today.
