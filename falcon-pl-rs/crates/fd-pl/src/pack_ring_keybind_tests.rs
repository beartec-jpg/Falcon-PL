//! falcon-node 2026-10-21: a seat whose local ring held `falcon-node-bond`'s
//! key packed that account's unbound tx (62b58294, seq 4). Every seat without
//! the ring entry rejected the block with `KEY_BIND_REFUSED`, so all of
//! falcon-node's round-0 turns were lost. The packer must bind keys from
//! chain state only (meta.public_key, classic address, PayToKey commit).

use super::*;

const NID: u32 = crate::NETWORK_ID_PUBLIC_TESTNET_2300;
const TIP: u64 = crate::params_v2::TESTNET_2300_KEY_BIND_FROM_HEIGHT + 100;

fn seat(id: &str) -> Node {
    let mut n = Node::new(
        id,
        ChainConfig { scheme: CryptoScheme::Falcon512, network_id: NID, pack_budget_ms: 0, min_fee: 0, ..ChainConfig::default() },
        KeyRing::new(CryptoScheme::Falcon512),
        &[("bond-acct".into(), 5_000), ("faucet".into(), 1_000_000)],
    );
    n.tip_height = TIP;
    n
}

fn pay_tx(n: &Node, k: &KeyRing) -> Tx {
    let body = TxBody::Pay;
    let seq = n.state.get("bond-acct").sequence;
    let payload = Tx::payload("bond-acct", seq, "faucet", 1, 2, NID, &body);
    Tx {
        account: "bond-acct".into(),
        sequence: seq,
        destination: "faucet".into(),
        amount: 1,
        fee: 2,
        network_id: NID,
        public_key: k.public_hex("bond-acct").unwrap(),
        signature: k.sign("bond-acct", &payload).unwrap(),
        tx_id: sha256_hex(payload.as_bytes()),
        body,
    }
}

#[test]
fn ring_only_key_is_not_packable() {
    let mut w = KeyRing::new(CryptoScheme::Falcon512);
    w.generate_id("bond-acct");
    let mut packer = seat("falcon-node");
    packer.keys.insert_public("bond-acct", &w.public_hex("bond-acct").unwrap()).unwrap();
    let tx = pay_tx(&packer, &w);
    // Pre-gate ingest still honours the ring on this seat ...
    if !packer.key_bind_onchain_gate_open() {
        assert!(packer.key_bind_check(&tx).is_ok());
    }
    // ... and a seat without the ring entry refuses it.
    let other = seat("v11");
    assert_eq!(other.key_bind_check(&tx).unwrap_err(), KEY_BIND_REFUSED);
    // The packer must agree with the strict seat.
    assert_eq!(
        Node::pack_key_bind_check(packer.key_bind_gate_open(), &packer.state, &tx).unwrap_err(),
        KEY_BIND_REFUSED
    );
}

#[test]
fn committed_key_is_packable() {
    let mut w = KeyRing::new(CryptoScheme::Falcon512);
    w.generate_id("bond-acct");
    let mut packer = seat("falcon-node");
    let pk = w.public_hex("bond-acct").unwrap();
    let mut a = packer.state.get("bond-acct");
    a.meta.key_commit = key_commit_hash(&pk).unwrap();
    packer.state.accounts.insert("bond-acct".into(), a);
    let tx = pay_tx(&packer, &w);
    assert!(Node::pack_key_bind_check(packer.key_bind_gate_open(), &packer.state, &tx).is_ok());
}
