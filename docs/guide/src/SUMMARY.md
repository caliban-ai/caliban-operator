# Summary

[caliban-operator](./introduction.md)

# Reference

- [CRD reference](./crds.md)
- [Deploying and configuring](./deploying.md)
- [Container image](./container.md)

# Changelog

- [Changelog](./changelog.md)

# Architecture Decisions

- [ADR Index](./adr/index.md)
<!-- adrs -->
  - [ADR 0000 · Record architecture decisions](./adr/0000-architecture-decision-records.md)
  - [ADR 0001 · kube-rs stack + `CalibanTask` CRD API](./adr/0001-kube-rs-stack-and-calibantask-crd.md)
  - [ADR 0002 · Reconcile `CalibanTask` → agent-sandbox `Sandbox` (+ per-task SA & NetworkPolicy)](./adr/0002-reconcile-calibantask-to-sandbox.md)
  - [ADR 0003 · The caliband launch contract (daemon args + workspace init)](./adr/0003-caliband-launch-contract.md)
  - [ADR 0004 · `Workspace` CRD + `CalibanTask` resolve-and-pin](./adr/0004-workspace-crd-and-resolve-and-pin.md)
  - [ADR 0005 · Operator owns infrastructure, prospero owns agent lifecycle, caliban owns its contract](./adr/0005-operator-infrastructure-prospero-agent-lifecycle.md)
  - [ADR 0006 · Unattended permission posture is authorized by Workspace policy](./adr/0006-unattended-permission-posture-authorized-by-workspace-policy.md)
