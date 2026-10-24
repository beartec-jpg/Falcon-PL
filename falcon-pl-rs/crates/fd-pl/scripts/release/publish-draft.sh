#!/usr/bin/env bash
# Create (or refresh) a DRAFT release in the public releases repo from a green
# "Release binaries" workflow run. Draft releases are invisible to the public until
# someone presses "Publish" (or runs: gh release edit vX --draft=false -R REPO).
#
#   bash publish-draft.sh <workflow-run-id>          # uses gh (logged in as a repo admin)
#   SRC_REPO=beartec-jpg/Falcon-PL DST_REPO=beartec-jpg/falcon-pl-releases bash publish-draft.sh 123456
#
# It never publishes, never signs, and never deletes a published release.
set -euo pipefail
RUN_ID="${1:?usage: publish-draft.sh <workflow-run-id>}"
SRC_REPO="${SRC_REPO:-beartec-jpg/Falcon-PL}"
DST_REPO="${DST_REPO:-beartec-jpg/falcon-pl-releases}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

gh run view "$RUN_ID" -R "$SRC_REPO" --json conclusion,headSha,workflowName -q '.conclusion+" "+.headSha+" "+.workflowName' >"$WORK/run"
read -r CONCL SHA _ <"$WORK/run"
[[ "$CONCL" == "success" ]] || { echo "run $RUN_ID is '$CONCL', not success" >&2; exit 1; }
gh run download "$RUN_ID" -R "$SRC_REPO" -n release-dist -D "$WORK/dist"
cd "$WORK/dist"
sha256sum -c SHA256SUMS
grep -q '^avx512_insns_node=0$' BUILDINFO.txt
grep -q '^avx512_insns_ctl=0$' BUILDINFO.txt
VER="$(sed -n 's/^product_version=//p' BUILDINFO.txt)"
COMMIT="$(sed -n 's/^commit=//p' BUILDINFO.txt)"            # source built (inputs.ref)
TOOLING="$(sed -n 's/^tooling_commit=//p' BUILDINFO.txt)"    # workflow commit (build script, installer, profile)
[[ "$TOOLING" == "$SHA" ]] || { echo "BUILDINFO tooling_commit $TOOLING != run head $SHA" >&2; exit 1; }
[[ "$COMMIT" =~ ^[0-9a-f]{40}$ ]] || { echo "BUILDINFO commit '$COMMIT' is not a full sha" >&2; exit 1; }
PUBKEY="RWTs+gnuKfzPik0Xbcf+m1hD/JrCUnWqaD/OAZFZEY2fcNc90D67mZlM"  # release key 8ACFFC29EE09FAEC (pinned in the installer)
# Beta channel: TAG_SUFFIX=-beta.1 makes tag v<ver>-beta.1 and marks the release a PRERELEASE
# (never "latest", so the default installer keeps installing stable).
TAG_SUFFIX="${TAG_SUFFIX:-}"
[[ -z "$TAG_SUFFIX" || "$TAG_SUFFIX" =~ ^-beta\.[0-9]+$ ]] || { echo "TAG_SUFFIX must look like -beta.N" >&2; exit 2; }
TAG="v$VER$TAG_SUFFIX"
PRE=(); [[ -n "$TAG_SUFFIX" ]] && PRE=(--prerelease --latest=false)

NOTES="$WORK/notes.md"
{
  if [[ -n "$TAG_SUFFIX" ]]; then
    echo "**BETA (opt-in prerelease, for testing).** Not installed by the default installer. Product version $VER."
    echo
    echo "Install: \`curl -fsSL https://github.com/$DST_REPO/releases/download/$TAG/install-falcon-node.sh | bash -s -- --channel beta\`"
    echo "Switch an existing node: \`falcon-node upgrade --channel beta\` (back: \`falcon-node upgrade --channel stable\`)"
    echo
  fi
  echo "Falcon PL $VER (public testnet 2300), linux x86_64 portable build."
  echo
  echo "- Source commit: \`$COMMIT\`; release tooling (installer, network profile): \`$TOOLING\`"
  echo "- Built twice in \`ubuntu:22.04\` + Rust 1.85.1 with RUSTFLAGS unset; both builds byte-identical; AVX-512 instructions: 0 (no target-cpu=native)."
  echo "- glibc >= $(sed -n 's/^glibc_min=//p' BUILDINFO.txt) (Ubuntu 22.04+, Debian 12+)."
  echo
  echo "Install a node: \`curl -fsSL https://falcon-fpl.com/install.sh | bash\`"
  echo
  echo "Verify by hand:"
  echo '```'
  echo "minisign -Vm SHA256SUMS -P $PUBKEY"
  echo "sha256sum -c --ignore-missing SHA256SUMS"
  echo '```'
  echo
  echo '```'
  cat SHA256SUMS
  echo '```'
} >"$NOTES"

FILES=(falcon-pl-node falcon-pl-ctl "falcon-pl-$VER-linux-x86_64.tar.gz" install-falcon-node.sh network-2300.env BUILDINFO.txt SHA256SUMS)
[[ -f SHA256SUMS.minisig ]] && FILES+=(SHA256SUMS.minisig)

if gh release view "$TAG" -R "$DST_REPO" --json isDraft -q .isDraft >"$WORK/draft" 2>/dev/null; then
  if [[ "$(cat "$WORK/draft")" != "true" ]]; then
    echo "$TAG is already PUBLISHED in $DST_REPO; refusing to modify it" >&2; exit 1
  fi
  gh release upload "$TAG" -R "$DST_REPO" --clobber "${FILES[@]}"
  gh release edit "$TAG" -R "$DST_REPO" --notes-file "$NOTES" --draft "${PRE[@]}"
  echo "refreshed DRAFT $TAG in $DST_REPO"
else
  gh release create "$TAG" -R "$DST_REPO" --draft --title "Falcon PL $VER${TAG_SUFFIX:+ (beta${TAG_SUFFIX#-beta})}" --notes-file "$NOTES" "${PRE[@]}" "${FILES[@]}"
  echo "created DRAFT $TAG in $DST_REPO"
fi
gh release view "$TAG" -R "$DST_REPO" --json url,isDraft,assets -q '.url+" draft="+(.isDraft|tostring)+" assets="+([.assets[].name]|join(","))'
