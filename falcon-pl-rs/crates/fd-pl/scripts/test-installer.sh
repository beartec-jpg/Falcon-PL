#!/usr/bin/env bash
# Dry-run & unit tests for install-falcon-node.sh (Items #1, #8, #13).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALLER="$SCRIPT_DIR/install-falcon-node.sh"

bash -n "$INSTALLER"
bash -n "$SCRIPT_DIR/falcon-node-autoupgrade"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# Source functions from install-falcon-node.sh without executing main "$@"
FUNCS_FILE="$TMP/funcs.sh"
sed '/^main "\$@"/d' "$INSTALLER" >"$FUNCS_FILE"
# shellcheck disable=SC1090
source "$FUNCS_FILE"

# --- Test 1 (Item #1): install_minisign_binary fallback + SHA-256 verification ---
mkdir -p "$TMP/pkg/minisign-linux/x86_64" "$TMP/dest"
cat <<'MS' >"$TMP/pkg/minisign-linux/x86_64/minisign"
#!/usr/bin/env bash
echo "minisign 0.11"
MS
chmod 0755 "$TMP/pkg/minisign-linux/x86_64/minisign"
tar -czf "$TMP/minisign-0.11-linux.tar.gz" -C "$TMP/pkg" minisign-linux
TAR_SHA="$(sha256sum "$TMP/minisign-0.11-linux.tar.gz" | awk '{print $1}')"

FALCON_MINISIGN_DEST_DIR="$TMP/dest"
MINISIGN_TARBALL_URL="file://$TMP/minisign-0.11-linux.tar.gz"
MINISIGN_TARBALL_SHA256="0000000000000000000000000000000000000000000000000000000000000000"
if install_minisign_binary 2>/dev/null; then
  echo "FAIL: install_minisign_binary should reject mismatched SHA-256" >&2
  exit 1
fi

MINISIGN_TARBALL_SHA256="$TAR_SHA"
install_minisign_binary
[[ -x "$TMP/dest/minisign" ]] || { echo "FAIL: minisign binary not executable" >&2; exit 1; }
[[ "$(stat -c '%a' "$TMP/dest/minisign")" == "755" ]] || { echo "FAIL: minisign mode not 755" >&2; exit 1; }

# Verify default URL and pinned SHA-256 in install-falcon-node.sh match official 0.11 release
grep -q 'https://github.com/jedisct1/minisign/releases/download/0.11/minisign-0.11-linux.tar.gz' "$INSTALLER"
grep -q 'f0a0954413df8531befed169e447a66da6868d79052ed7e892e50a4291af7ae0' "$INSTALLER"

# --- Test 2 (Item #8): node.env permissions and NODE_ID preservation ---
FALCON_HOME="$TMP/home"
resolve_home
mkdir -p "$BIN" "$KEYS" "$DATA" "$LOGS" "$CONF" "$RUN" "$VALDIR"
cat <<'ENV' >"$NODE_ENV"
OBSERVER_ID=obs-test-123456
LISTEN=127.0.0.1:19400
SERVICE_MODE=none
VERSION=2.9.81
ENV
chmod 0600 "$NODE_ENV"

# Even when NODE_ID was missing and mode was 0600, load_installed / ensure_node_env_readable recovers NODE_ID and fixes mode to 644
load_installed
[[ "$(kv_get "$NODE_ENV" NODE_ID)" == "obs-test-123456" ]] || { echo "FAIL: NODE_ID not recovered" >&2; exit 1; }
[[ "$(stat -c '%a' "$NODE_ENV")" == "644" ]] || { echo "FAIL: node.env mode not 644 after load_installed" >&2; exit 1; }

chmod 0600 "$NODE_ENV"
kv_set "$NODE_ENV" VERSION "2.10.0"
[[ "$(stat -c '%a' "$NODE_ENV")" == "644" ]] || { echo "FAIL: node.env mode not 644 after kv_set" >&2; exit 1; }
[[ "$(kv_get "$NODE_ENV" NODE_ID)" == "obs-test-123456" ]] || { echo "FAIL: NODE_ID lost after kv_set" >&2; exit 1; }

# --- Test 3 (Item #13): autoupgrade on|off|status and rollback on failed health check ---
OUT_STATUS="$(AUTOUPGRADE_ACTION=status cmd_autoupgrade)"
[[ "$OUT_STATUS" == *"autoupgrade: off"* ]] || { echo "FAIL: default autoupgrade status should be off (got: $OUT_STATUS)" >&2; exit 1; }

AUTOUPGRADE_ACTION=on AUTOUPGRADE_JITTER_SECS=0 cmd_autoupgrade >/dev/null
[[ "$(kv_get "$NODE_ENV" AUTOUPGRADE)" == "on" ]] || { echo "FAIL: AUTOUPGRADE not set to on" >&2; exit 1; }
[[ -x "$BIN/falcon-node-autoupgrade" ]] || { echo "FAIL: falcon-node-autoupgrade not created" >&2; exit 1; }

AUTOUPGRADE_ACTION=off cmd_autoupgrade >/dev/null
[[ "$(kv_get "$NODE_ENV" AUTOUPGRADE)" == "off" ]] || { echo "FAIL: AUTOUPGRADE not set to off" >&2; exit 1; }

# Verify rollback_autoupgrade restores .prev binary and VERSION
echo "v2.9.81-binary" >"$BIN/falcon-pl-node.prev"
echo "v2.9.81-ctl" >"$BIN/falcon-pl-ctl.prev"
echo "v2.10.0-broken" >"$BIN/falcon-pl-node"
echo "v2.10.0-broken" >"$BIN/falcon-pl-ctl"
kv_set "$NODE_ENV" VERSION "2.10.0"
svc_restart() { :; }
rollback_autoupgrade "2.9.81" >/dev/null 2>&1
[[ "$(cat "$BIN/falcon-pl-node")" == "v2.9.81-binary" ]] || { echo "FAIL: falcon-pl-node not rolled back" >&2; exit 1; }
[[ "$(cat "$BIN/falcon-pl-ctl")" == "v2.9.81-ctl" ]] || { echo "FAIL: falcon-pl-ctl not rolled back" >&2; exit 1; }
[[ "$(kv_get "$NODE_ENV" VERSION)" == "2.9.81" ]] || { echo "FAIL: VERSION not rolled back" >&2; exit 1; }

echo "PASS: all installer tests (Items #1, #8, #13) succeeded"
