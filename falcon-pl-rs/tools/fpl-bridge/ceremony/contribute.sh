#!/usr/bin/env bash
# Ceremony CONTRIBUTOR: run on YOUR OWN machine. Your secret never leaves this process.
#   contribute.sh <circuit> <in.zkey> <out.zkey> "<your name>"
# Entropy = 64 random bytes from the OS + whatever you type; it is passed to snarkjs and never written to disk.
set -euo pipefail
c=$1; in=$2; out=$3; name=$4
X=${SNARKJS:-npx snarkjs}; export NODE_OPTIONS=${NODE_OPTIONS:---max-old-space-size=8192}
echo "sha256 of the file you were given (compare with the coordinator's transcript):"; sha256sum "$in"
if [ -n "${CER_TYPED+x}" ]; then typed=$CER_TYPED; else read -r -s -p "type some random text (not stored), then Enter: " typed; echo; fi
ent="$(head -c 64 /dev/urandom | od -An -tx1 | tr -d ' \n')$typed"
$X zkey contribute "$in" "$out" --name="$name" -e="$ent" | tee "$out.contribution.txt"
unset ent typed
echo; echo "Upload $out to the coordinator and publish the 'Contribution Hash' above (signed post/commit)."
echo "Then delete nothing you need, but reboot (or close the shell) so no copy of the entropy stays in memory."
