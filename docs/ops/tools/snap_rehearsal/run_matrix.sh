#!/usr/bin/env bash
# Sequential scenario driver (one scenario at a time; same scratch port range).
H="${H:-$(dirname "$0")/../snap_catchup_session_rehearsal.sh}"
C16="FALCON_PL_SNAPSHOT_CHUNK_BYTES=16384"
P16="$C16 FALCON_PL_TEST_INBOUND_CATCH_CAP=16 FALCON_PL_TEST_CATCH_DRAIN_MS=5"
run() { echo "$(date -u +%T) start $1"; env "${@:2}" TAG=$1 bash $H > /tmp/falcon-snap-art-$1.run.log 2>&1; echo "$(date -u +%T) end $1 rc=$?"; sleep 3; }
for s in ${SCEN:-s1 s2 s3 s4 s5 s6 s7}; do case $s in
 s1) run t301s1-basic WALLETS=20000 "ARCH_ENV=$C16" ;;
 s2) run t301s2-pressure WALLETS=20000 "ARCH_ENV=$P16" ;;
 s3) run t301s3-reask WALLETS=20000 "ARCH_ENV=$P16" "JOIN_ENV=FALCON_PL_TEST_REASK_AFTER_OFFER=1" ;;
 s4) run t301s4-two WALLETS=20000 "ARCH_ENV=$P16" "JOINERS=j1 j2" ;;
 s5) run t301s5-stall WALLETS=20000 "ARCH_ENV=$C16 FALCON_PL_TEST_SNAP_STALL_FROM_CHUNK=20 FALCON_PL_TEST_SNAP_STALL_MS=25000" TIMEOUT=240 ;;
 s6) run t301s6-large16k WALLETS=64000 "ARCH_ENV=$C16" TIMEOUT=300 ;;
 s7) run t301s7-large256k-pressure WALLETS=64000 "ARCH_ENV=FALCON_PL_TEST_INBOUND_CATCH_CAP=64 FALCON_PL_TEST_CATCH_DRAIN_MS=5" TIMEOUT=300 ;;
esac; done
echo ALLDONE
