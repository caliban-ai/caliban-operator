#!/usr/bin/env bash
# Copy the top-level reference docs into the mdBook guide as chapters.
# Run from the repo root (the docs workflow runs it before `mdbook build`).
#
# docs/*.md stays the source of truth: README.md links to those paths, and they
# are readable on GitHub without building anything. The copies under
# docs/guide/src/ are gitignored, so there is exactly one editable copy of each
# page and no chance of the two drifting. Same arrangement as sync-adrs.sh.
#
# One rewrite happens on the way in. These pages link to directories that live
# outside docs/ -- `../deploy/crd/`, `../deploy/samples/` -- which resolve
# correctly when the file is read on GitHub but point outside the book when it
# is rendered, where they would be dead links. Those are rewritten to absolute
# repository URLs. Links between the pages themselves (`deploying.md#...`) are
# left alone: they are correct in both places.
set -euo pipefail

GUIDE_SRC="docs/guide/src"
REPO_TREE="https://github.com/caliban-ai/caliban-operator/tree/main"

PAGES=(crds deploying container)

for page in "${PAGES[@]}"; do
  src="docs/${page}.md"
  if [[ ! -f "$src" ]]; then
    echo "error: $src not found (run from the repo root)" >&2
    exit 1
  fi
  sed "s#](\.\./#](${REPO_TREE}/#g" "$src" > "$GUIDE_SRC/${page}.md"
done
