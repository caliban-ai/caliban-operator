# Architecture Decision Records

caliban-operator keeps its architecture decisions here, in MADR-lite format (see
[ADR 0000](0000-architecture-decision-records.md)). Each record is an append-only
`NNNN-kebab-title.md` with **Context**, **Decision**, **Consequences**. A decision
is changed by writing a new ADR that supersedes the old one, never by rewriting
history.

| ADR | Title | Status |
|-----|-------|--------|
| [0000](0000-architecture-decision-records.md) | Record architecture decisions (MADR-lite under `docs/adr/`) | accepted |
| [0001](0001-kube-rs-stack-and-calibantask-crd.md) | kube-rs stack + `CalibanTask` CRD API (`caliban.caliban-ai.dev/v1alpha1`, namespaced, status subresource; generated CRD YAML) | accepted |
| [0002](0002-reconcile-calibantask-to-sandbox.md) | Reconcile `CalibanTask` → agent-sandbox `Sandbox` (+ per-task token-less SA & default-deny NetworkPolicy; foreign `Sandbox` type; SSA + owner refs; status from `serviceFQDN`) | accepted |
| [0003](0003-caliband-launch-contract.md) | The caliband launch contract — daemon `args` (`--workspace-root`/`--listen`), git-clone init container for the workspace, plaintext TCP for the first e2e | accepted |
| [0004](0004-workspace-crd-and-resolve-and-pin.md) | `Workspace` CRD (named providers, operator-sole-Secret-reader) + `CalibanTask` `workspaceRef`/`providerRef` resolve-and-pin; inline `spec.workspace` removed (pre-v1 breaking) | accepted |
