//! Sender signatures stay on a Merkle batch. Receivers re-check them.
//! Public keys already known to the seat are left off the wire.

use super::*;
use crate::crypto::{CryptoScheme, KeyRing};

fn signed_batch(n: usize, height: u64) -> (KeyRing, Ledger) {
    let mut keys = KeyRing::new(CryptoScheme::HmacDev);
    keys.generate_id("packer");
    let mut txs = Vec::with_capacity(n);
    for i in 0..n {
        let acct = format!("a{i}");
        keys.generate_id(&acct);
        // Distinct accounts so each leaf is a different payload. Sequence 0,
        // amount i, one destination. Network 2300 so the sender-sig height applies.
        txs.push(
            Tx::sign_body(
                &keys,
                &acct,
                0,
                "bob",
                i as u64,
                1,
                NETWORK_ID_PUBLIC_TESTNET_2300,
                TxBody::Pay,
            )
            .unwrap(),
        );
    }
    let mut ledger = Ledger::seal(
        height,
        "00".into(),
        NETWORK_ID_PUBLIC_TESTNET_2300,
        "packer",
        &keys,
        txs,
        "root".into(),
        1,
    )
    .unwrap();
    ledger.attach_merkle_batch(&keys).unwrap();
    (keys, ledger)
}

#[test]
fn merkle_batch_round_trip_odd_even_and_full() {
    for n in [1usize, 2, 3, 400] {
        let (keys, ledger) =
            signed_batch(n, params_v2::TESTNET_2300_MERKLE_SENDER_SIG_FROM_HEIGHT);
        assert_eq!(ledger.txs.len(), n);
        assert_eq!(ledger.batch_paths.len(), n);
        assert!(!ledger.batch_sig.is_empty());
        for tx in &ledger.txs {
            assert!(!tx.signature.is_empty(), "sender sig kept n={n}");
            assert!(!tx.public_key.is_empty(), "sender pubkey kept n={n}");
        }
        ledger.verify_merkle_batch(&keys).expect("verify");
    }
}

#[test]
fn merkle_batch_rejects_one_byte_payload_change() {
    let (keys, mut ledger) = signed_batch(3, 520_000);
    ledger.txs[1].amount ^= 1;
    let err = ledger.verify_merkle_batch(&keys).unwrap_err();
    assert!(err.contains("tx_id") || err.contains("path") || err.contains("signature"), "{err}");
}

#[test]
fn merkle_batch_rejects_one_byte_path_change() {
    let (keys, mut ledger) = signed_batch(2, 520_000);
    let hex_sib = &mut ledger.batch_paths[0][0];
    let mut bytes = hex::decode(&*hex_sib).unwrap();
    bytes[0] ^= 0x01;
    *hex_sib = hex::encode(bytes);
    let err = ledger.verify_merkle_batch(&keys).unwrap_err();
    assert!(err.contains("path"), "{err}");
}

#[test]
fn merkle_batch_requires_sender_sig_from_the_gate() {
    let (keys, mut ledger) = signed_batch(2, 520_000);
    for tx in &mut ledger.txs {
        tx.signature.clear();
    }
    let err = ledger.verify_merkle_batch(&keys).unwrap_err();
    assert!(err.contains("missing sender"), "{err}");
}

#[test]
fn merkle_batch_omits_public_key_when_the_account_already_has_it() {
    let (keys, mut ledger) = signed_batch(3, 520_000);
    let known = |id: &str| keys.public_hex(id);
    ledger.attach_merkle_batch_known(&keys, &known).unwrap();
    assert!(ledger.txs.iter().all(|tx| tx.public_key.is_empty()));
    assert!(ledger.txs.iter().all(|tx| !tx.signature.is_empty()));
    ledger.verify_merkle_batch_known(&keys, &known).unwrap();
    let err = ledger.verify_merkle_batch(&keys).unwrap_err();
    assert!(err.contains("unknown"), "{err}");
}

#[test]
fn merkle_batch_verifies_a_sender_the_seat_has_not_enrolled() {
    let (keys, ledger) = signed_batch(2, 520_000);
    let mut seat = KeyRing::new(CryptoScheme::HmacDev);
    seat.insert_public("packer", &keys.public_hex("packer").unwrap())
        .unwrap();
    ledger.verify_merkle_batch(&seat).expect("carried key");
}

#[test]
fn merkle_batch_legacy_stripped_body_still_verifies_below_the_gate() {
    let (keys, mut ledger) = signed_batch(3, 519_999);
    for tx in &mut ledger.txs {
        tx.signature.clear();
        tx.public_key.clear();
    }
    ledger.verify_merkle_batch(&keys).expect("legacy stripped body");
}
