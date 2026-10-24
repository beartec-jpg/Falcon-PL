#!/usr/bin/env bash
# Signs SHA256SUMS of a DRAFT release with the Falcon PL release key and attaches SHA256SUMS.minisig.
#
# The release key lives on host1 only (see docs/ops/HANDOVER.md section 10). It is never copied off the
# host: SHA256SUMS is streamed to host1 over `tailscale ssh`, signed there, and only the signature comes back.
# The signature is verified locally against the PINNED public key before upload.
#
#   bash sign-sums.sh v2.9.61            # needs: gh (beartec-jpg), tailscale ssh to falcon@host1, minisign
set -euo pipefail
TAG="${1:?usage: sign-sums.sh <tag>}"
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-beta\.[0-9]+)?$ ]] || { echo "bad tag: $TAG" >&2; exit 2; }
REPO="${DST_REPO:-beartec-jpg/falcon-pl-releases}"
SIGN_HOST="${SIGN_HOST:-falcon@host1}"
# Must match RELEASE_MINISIGN_PUBKEY_PINNED in install-falcon-node.sh (key id 8ACFFC29EE09FAEC).
PUBKEY="RWTs+gnuKfzPik0Xbcf+m1hD/JrCUnWqaD/OAZFZEY2fcNc90D67mZlM"

command -v minisign >/dev/null || { echo "minisign required locally (to verify)" >&2; exit 1; }
[[ "$(gh release view "$TAG" -R "$REPO" --json isDraft -q .isDraft)" == true ]] \
  || { echo "$TAG is not a draft release in $REPO: refusing" >&2; exit 1; }

W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
gh release download "$TAG" -R "$REPO" -p SHA256SUMS -D "$W"
# The checksums must match the files attached to the draft.
gh release download "$TAG" -R "$REPO" -D "$W/files" -p '*' >/dev/null
(cd "$W/files" && sha256sum -c ../SHA256SUMS >/dev/null) || { echo "SHA256SUMS does not match the draft's assets" >&2; exit 1; }
cat "$W/SHA256SUMS"
if [[ "${YES:-0}" != 1 ]]; then
  read -r -p "Sign these checksums for $TAG on $SIGN_HOST? [y/N] " a; [[ "$a" == y || "$a" == Y ]] || exit 1
fi

TC="falcon-pl $TAG linux-x86_64 SHA256SUMS"
# Remote side: the key and its password file stay on the host; nothing secret is echoed.
# Redirects are intentionally local (stdin = checksums, stdout = signature).
# shellcheck disable=SC2024
sudo tailscale ssh "$SIGN_HOST" "set -eu; D=\$HOME/falcon-pl-public-testnet-2300/keys/release-signing; \
  T=\$(mktemp -d); trap 'rm -rf \"\$T\"' EXIT; cat > \"\$T/SHA256SUMS\"; \
  \$HOME/.local/bin/minisign -S -s \"\$D/minisign.key\" -m \"\$T/SHA256SUMS\" -t '$TC' < \"\$D/minisign.pass\" >/dev/null 2>&1; \
  cat \"\$T/SHA256SUMS.minisig\"" < "$W/SHA256SUMS" > "$W/SHA256SUMS.minisig"

minisign -Vm "$W/SHA256SUMS" -P "$PUBKEY" -x "$W/SHA256SUMS.minisig"
gh release upload "$TAG" -R "$REPO" --clobber "$W/SHA256SUMS.minisig"
echo "attached SHA256SUMS.minisig to $TAG (draft)"
