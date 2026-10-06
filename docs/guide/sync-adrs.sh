#!/usr/bin/env bash
# Copy docs/adr/*.md into the mdBook guide and regenerate the ADR SUMMARY block.
# Run from the repo root (the docs workflow runs it before `mdbook build`).
# Portable across BSD (macOS) and GNU awk.
set -euo pipefail

ADR_SRC="docs/adr"
GUIDE_SRC="docs/guide/src"
DEST="$GUIDE_SRC/adr"
SUMMARY="$GUIDE_SRC/SUMMARY.md"
MARKER="<!-- adrs -->"
REPO_BLOB="https://github.com/caliban-ai/caliban-operator/blob/main"

# Chapters that sync-docs.sh ingests, so a `../<name>` link from an ADR still
# resolves inside the rendered book and must NOT be rewritten.
IN_BOOK=(crds.md deploying.md container.md)

# Files in docs/adr that are not themselves ADRs.
is_adr() {
  case "$(basename "$1")" in
    index.md | README.md | template.md) return 1 ;;
    *) return 0 ;;
  esac
}

# Point links that leave the rendered book at the repository instead.
#
# An ADR is correct as a file on GitHub, where `README.md`, `template.md` and
# `../superpowers/...` all resolve. In the book they do not: only files listed in
# SUMMARY.md become pages (`create-missing = false`), and README.md/template.md
# are deliberately excluded by is_adr, while nothing outside docs/adr and the
# three ingested chapters is copied in at all. mdBook does not check markdown
# links, so these published as silent 404s.
#
# The three in-book chapters are protected first, then everything else relative
# is sent to the repo. Doing it in that order means a future `../plans/...` link
# is handled without touching this script.
# sed is used rather than sd: this runs on the GitHub runner, which installs only
# the mdBook toolchain. Edits go via a temp file because `sed -i` takes an
# argument on BSD and not on GNU.
rewrite_outward_links() { # rewrite_outward_links <file>
  local f="$1" tmp="$1.tmp" i=0 name
  local -a script=()

  # Both markdown link forms carry a target: inline `](url)` and a reference
  # definition `[label]: url`. ADR 0000 uses the second for its bootstrap spec,
  # so handling only the first would leave that one broken.
  #
  # Protect the in-book chapters first.
  for name in "${IN_BOOK[@]}"; do
    script+=(-e "s#](\.\./${name}#](@@KEEP${i}@@#g")
    script+=(-e "s#^\(\[[^]]*\]: \)\.\./${name}#\1@@KEEP${i}@@#")
    i=$((i + 1))
  done

  # docs/adr/<file> -- a sibling of the ADR, not a book page.
  script+=(-e "s#](README\.md#](${REPO_BLOB}/${ADR_SRC}/README.md#g")
  script+=(-e "s#](template\.md#](${REPO_BLOB}/${ADR_SRC}/template.md#g")
  script+=(-e "s#^\(\[[^]]*\]: \)README\.md#\1${REPO_BLOB}/${ADR_SRC}/README.md#")
  script+=(-e "s#^\(\[[^]]*\]: \)template\.md#\1${REPO_BLOB}/${ADR_SRC}/template.md#")
  # Anything else one level up is docs/<path>, outside the book.
  script+=(-e "s#](\.\./#](${REPO_BLOB}/docs/#g")
  script+=(-e "s#^\(\[[^]]*\]: \)\.\./#\1${REPO_BLOB}/docs/#")

  # Restore the protected ones.
  i=0
  for name in "${IN_BOOK[@]}"; do
    script+=(-e "s#](@@KEEP${i}@@#](../${name}#g")
    script+=(-e "s#^\(\[[^]]*\]: \)@@KEEP${i}@@#\1../${name}#")
    i=$((i + 1))
  done

  sed "${script[@]}" "$f" > "$tmp"
  mv "$tmp" "$f"
}

mkdir -p "$DEST"
cp "$ADR_SRC"/*.md "$DEST"/

for f in "$DEST"/*.md; do
  rewrite_outward_links "$f"
done

# Build an ADR index page from the file titles (first markdown H1 of each file).
{
  echo "# Architecture Decision Records"
  echo
  for f in "$DEST"/*.md; do
    is_adr "$f" || continue
    base="$(basename "$f")"
    title="$(grep -m1 '^# ' "$f" | sed 's/^# //')"
    echo "- [${title:-$base}](./${base})"
  done
} > "$DEST/index.md"

# Build the nested SUMMARY entries (newest mdBook needs every page listed).
entries=""
for f in "$DEST"/*.md; do
  is_adr "$f" || continue
  base="$(basename "$f")"
  title="$(grep -m1 '^# ' "$f" | sed 's/^# //')"
  entries+="  - [${title:-$base}](./adr/${base})"$'\n'
done

# Regenerate everything after the marker: keep the file up to and including the
# marker line, then append the fresh entries. Uses sed (not a multi-line awk -v),
# which is portable across BSD/macOS and GNU awk.
grep -qF -- "$MARKER" "$SUMMARY" || {
  echo "error: ADR marker '$MARKER' not found in $SUMMARY" >&2
  exit 1
}
tmp="$SUMMARY.tmp"
sed "/$MARKER/q" "$SUMMARY" > "$tmp"
printf '%s' "$entries" >> "$tmp"
mv "$tmp" "$SUMMARY"
