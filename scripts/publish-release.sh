#!/usr/bin/env bash
set -euo pipefail

: "${RELEASE_TAG:?Set RELEASE_TAG}"
: "${RELEASE_REPO:?Set RELEASE_REPO}"
python3 scripts/release.py version --tag "$RELEASE_TAG"
prerelease=$(python3 scripts/release.py prerelease --tag "$RELEASE_TAG")
latest=true
if [[ "$prerelease" == true ]]; then latest=false; fi

# A failed upload remains a draft. Reruns may repair a draft, never a published release.
if draft=$(gh release view "$RELEASE_TAG" --repo "$RELEASE_REPO" --json isDraft --jq .isDraft 2>/dev/null); then
  if [[ "$draft" != true ]]; then
    printf 'Release %s is already published; create a new version tag.\n' "$RELEASE_TAG" >&2
    exit 1
  fi
  gh release upload "$RELEASE_TAG" dist/* --repo "$RELEASE_REPO" --clobber
else
  gh release create "$RELEASE_TAG" dist/* --repo "$RELEASE_REPO" \
    --verify-tag --draft --generate-notes --title "Jammygit $RELEASE_TAG" \
    --prerelease="$prerelease"
fi
gh release edit "$RELEASE_TAG" --repo "$RELEASE_REPO" --draft=false \
  --prerelease="$prerelease" --latest="$latest"
