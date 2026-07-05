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
