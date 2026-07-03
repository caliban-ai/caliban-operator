# ADR 0000 · Record architecture decisions

- **Status:** accepted
- **Date:** 2026-06-14

## Context

caliban has kept Architecture Decision Records since the Layer-0 bootstrap
(ADRs 0001–0047). The original [2026-05-22 Layer-0 bootstrap design][bootstrap]
placed them at the repository root in `adrs/`, reasoning that ADRs are first-class
Layer-0 deliverables and top-level placement makes them impossible to miss.

Since then the sibling repositories adopted the conventional
[`adr-tools`](https://github.com/npryce/adr-tools) / [MADR](https://adr.github.io/madr/)
layout instead: **prospero** and **gonzalo** both keep their records under
`docs/adr/`, seed the log with a meta "record architecture decisions" entry, and
(prospero) ship a `template.md`. caliban was the outlier — root `adrs/` (plural),
no meta record, no template — which created cross-repo confusion and path
mismatches for anyone moving between the three repos.

There was no ADR stating *why* caliban records decisions or where they live; that
rationale lived only in a feature design doc, which is exactly the kind of
external dependency ADRs are supposed to avoid.

## Decision

We will keep Architecture Decision Records under **`docs/adr/`**, in **MADR-lite**
format (a lightweight extension of Michael Nygard's original ADR style), matching
sibling repos prospero and gonzalo. Specifically:

- **Location:** `docs/adr/` (singular `adr`), not the former root `adrs/`. This
  supersedes the root-placement decision in the [Layer-0 bootstrap design][bootstrap];
  existing records were relocated with `git mv` to preserve history.
- **This meta record is numbered `0000`** so the existing `0001`–`0047` numbering
  is preserved — no renumbering churn, and the log still opens with a record of the
  practice itself.
- **Format:** each ADR is one append-only file `NNNN-kebab-title.md` with
  **Context**, **Decision**, and **Consequences**. A decision is changed by writing
  a new ADR that supersedes the old one, never by rewriting history.
- **Template:** new ADRs start from [`template.md`](template.md).
- **Status legend:** `accepted` / `superseded` / `proposed` / `rejected`, indexed
  in [`README.md`](README.md).

## Consequences

- **Positive:** one consistent ADR convention across caliban / gonzalo / prospero;
  the conventional, tooling-friendly `docs/adr/` location; and the rationale for the
  practice now lives in an ADR rather than a feature design doc, so it is
  self-sustaining.
- **Negative:** a one-time churn to relocate the directory and update every inbound
  reference (crate rustdoc, README, the mdBook guide, the parity matrix, and the
  historical design docs). ADRs no longer sit at the repo root, so they are slightly
  less discoverable from a bare `ls` — mitigated by a pointer from the top-level
  `README.md`.
- **Revisit if:** the agreed cross-sibling ADR standard changes, or the `docs/adr/`
  layout proves harder to maintain than the root placement it replaced.

[bootstrap]: ../superpowers/specs/2026-05-22-layer-0-bootstrap-design.md
