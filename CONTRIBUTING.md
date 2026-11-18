# Contributing to Archon

Archon is early R&D software, licensed under the Apache License, Version
2.0 (see [`LICENSE`](LICENSE)). Contributions are welcome.

## Rights

Developer Certificate of Origin (DCO) is the contribution policy: sign
off every commit (`git commit -s`), adding a `Signed-off-by` line that
affirms you may submit the work under the project license. There is no
contributor license agreement.

## Workflow

- External contributors: fork the repository and open a pull request
  against `main`. Keep each PR to one coherent change with tests.
- All PRs must pass CI: `verify` (fmt, clippy, tests, simulator),
  `privileged-enforcement` (Linux cgroup/device proof), and
  `supply-chain` (cargo-deny).
- The maintainer may additionally work directly on `main` during R&D.

## Local checks

Run these before opening a PR:

```text
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p archon-sim
```

If you change dependencies, also run
`cargo deny check advisories bans licenses sources`.

## Change guidance

- Match the architecture guardrails in [`AGENTS.md`](AGENTS.md):
  narrow ownership boundary, reuse Linux/OCI/driver substrate, explicit
  failure instead of silently weakened contracts, no speculative
  components.
- Add or update tests when existing coverage does not protect a
  meaningful behavior or regression. Use the deterministic simulator
  (`archon-sim`) for distributed ownership and failure invariants, and
  live Linux integration tests for enforcement and timing-sensitive
  proof.
- Update the [README](README.md) when CLI usage, host prerequisites, or
  implemented scope changes. Update the [lab operations guide](docs/operations.md)
  when persistence, recovery, or capacity-reuse behavior changes.

## Security

Do not open a public issue for a suspected vulnerability. See
[`SECURITY.md`](SECURITY.md).
