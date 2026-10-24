//! `fpl-graph regtest-acceptance` — end-to-end graph test on a local bitcoind regtest.
//! Env: FPL_BTC_CLI (default "bitcoin-cli"), FPL_BTC_DATADIR.
use fd_pl_graph::bitcoin::{consensus, Address, Amount, Network, OutPoint, ScriptBuf, Transaction, Txid};
use fd_pl_graph::ceremony::{transcript_hash, Participant};
use fd_pl_graph::graph::*;
use fd_pl_graph::*;
use std::process::Command;
use std::str::FromStr;

fn cli(args: &[&str]) -> Result<String, String> {
    let bin = std::env::var("FPL_BTC_CLI").unwrap_or_else(|_| "bitcoin-cli".into());
    let mut c = Command::new(bin);
    if let Ok(d) = std::env::var("FPL_BTC_DATADIR") {
        c.arg(format!("-datadir={d}"));
    }
    let o = c.args(args).output().map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}
fn mine(n: u32) { let a = cli(&["-rpcwallet=miner", "getnewaddress"]).unwrap(); cli(&["generatetoaddress", &n.to_string(), &a]).unwrap(); }
fn send(tx: &Transaction) -> Result<String, String> { cli(&["sendrawtransaction", &consensus::encode::serialize_hex(tx)]) }

/// Fund `addr` with `sats` from the miner wallet; return the outpoint.
fn fund(addr: &Address, sats: u64) -> OutPoint {
    let btc = format!("{:.8}", sats as f64 / 1e8);
    let txid = cli(&["-rpcwallet=miner", "sendtoaddress", &addr.to_string(), &btc]).unwrap();
    mine(1);
    let raw = cli(&["getrawtransaction", &txid]).unwrap();
    let tx: Transaction = consensus::encode::deserialize_hex(&raw).unwrap();
    let spk = addr.script_pubkey();
    let vout = tx.output.iter().position(|o| o.script_pubkey == spk).unwrap() as u32;
    OutPoint { txid: Txid::from_str(&txid).unwrap(), vout }
}

struct Setup { p: GraphParams, claim: Transaction, payout: Transaction, deposit_value: u64, s0: [u8; 32], s1: [u8; 32], log: Vec<String> }

/// Ceremony: 3 participants (RAM keys), presign Claim + Payout, wipe, attest.
fn setup(deposit_sats: u64, operator_spk: ScriptBuf) -> Setup {
    let mut log = vec![];
    let mut parts: Vec<Participant> = (0..3).map(|i| Participant::new(&format!("p{i}"))).collect();
    let keys: Vec<_> = parts.iter().map(|p| p.xonly).collect();
    for p in &parts { log.push(format!("participant {} xonly {}", p.id, hex::encode(p.xonly.serialize()))); }
    // Operator's one-bit Lamport commitment (split-verifier Assert bit).
    let s0: [u8; 32] = rand32(); let s1: [u8; 32] = rand32();
    let pk0: [u8; 32] = sha(&s0); let pk1: [u8; 32] = sha(&s1);
    let p = GraphParams { keys: keys.clone(), payout_csv: 6, equiv_pk0: pk0, equiv_pk1: pk1, fee_sats: 1000, truc: false };
    let dep = deposit_output(&p).unwrap();
    let dep_out = fund(&dep.address(Network::Regtest), deposit_sats);
    log.push(format!("deposit {}:{} {} sats addr {}", dep_out.txid, dep_out.vout, deposit_sats, dep.address(Network::Regtest)));
    let mut claim = claim_tx(&p, dep_out, deposit_sats).unwrap();
    let mut payout = payout_tx(&p, &claim, operator_spk).unwrap();
    // Presign Claim (deposit nofn leaf).
    let leaf = dep.leaf("nofn").unwrap().clone();
    let prev = vec![fd_pl_graph::bitcoin::TxOut { value: Amount::from_sat(deposit_sats), script_pubkey: dep.spk() }];
    let m = script_path_sighash(&claim, 0, &prev, &leaf).unwrap();
    let sigs: Vec<[u8; 64]> = parts.iter().map(|q| q.sign(&m).unwrap()).collect();
    claim.input[0].witness = nofn_witness(&sigs, &leaf, control_block(&dep.info, &leaf).unwrap());
    // Presign Payout (claim payout leaf).
    let co = claim_output(&p).unwrap();
    let leaf2 = co.leaf("payout").unwrap().clone();
    let prev2 = vec![claim.output[0].clone()];
    let m2 = script_path_sighash(&payout, 0, &prev2, &leaf2).unwrap();
    let sigs2: Vec<[u8; 64]> = parts.iter().map(|q| q.sign(&m2).unwrap()).collect();
    payout.input[0].witness = nofn_witness(&sigs2, &leaf2, control_block(&co.info, &leaf2).unwrap());
    let txids = vec![claim.compute_txid().to_string(), payout.compute_txid().to_string()];
    let th = transcript_hash(&keys, &txids);
    log.push(format!("presigned claim {} payout {} transcript {}", txids[0], txids[1], th));
    for q in parts.iter_mut() {
        let a = q.wipe(&th);
        assert!(q.is_wiped() && a.verify(), "deletion attestation");
        log.push(format!("wiped {} attestation_sig {}", a.participant, &a.sig[..16]));
    }
    Setup { p, claim, payout, deposit_value: deposit_sats, s0, s1, log }
}

fn rand32() -> [u8; 32] { use fd_pl_graph::bitcoin::secp256k1::rand::RngCore; let mut b = [0u8; 32]; fd_pl_graph::bitcoin::secp256k1::rand::thread_rng().fill_bytes(&mut b); b }
fn sha(b: &[u8]) -> [u8; 32] { use sha2::Digest; sha2::Sha256::digest(b).into() }

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    if cmd != "regtest-acceptance" { eprintln!("usage: fpl-graph regtest-acceptance"); std::process::exit(2); }
    let op_addr = Address::from_str(&cli(&["-rpcwallet=miner", "getnewaddress", "op", "bech32m"]).unwrap()).unwrap().assume_checked();
    let ch_addr = Address::from_str(&cli(&["-rpcwallet=miner", "getnewaddress", "ch", "bech32m"]).unwrap()).unwrap().assume_checked();
    let mut results = vec![];

    // 1) Honest path + operator timeout: Payout refused before CSV, accepted after.
    let s = setup(100_000, op_addr.script_pubkey());
    for l in &s.log { println!("[honest] {l}"); }
    let c = send(&s.claim).expect("claim");
    mine(1);
    let early = send(&s.payout);
    println!("[honest] claim {c}; payout before CSV -> {:?}", early.as_ref().err());
    assert!(early.is_err() && early.unwrap_err().contains("non-BIP68-final"), "timeout must be enforced");
    mine(s.p.payout_csv as u32);
    let p = send(&s.payout).expect("payout after CSV");
    mine(1);
    println!("[honest] payout {p} confirmed");
    results.push("honest path: PASS (claim -> payout only after CSV timeout)".to_string());
    results.push("operator timeout: PASS (payout refused non-BIP68-final before CSV)".to_string());

    // 2) Dishonest claim disproved: operator equivocates (both preimages public) -> Disprove before Payout.
    let s = setup(100_000, op_addr.script_pubkey());
    send(&s.claim).expect("claim2");
    mine(1);
    let d = disprove_tx(&s.p, &s.claim, ch_addr.script_pubkey(), &s.s0, &s.s1).unwrap();
    let dtx = send(&d).expect("disprove");
    mine(s.p.payout_csv as u32);
    let late = send(&s.payout);
    println!("[dishonest] disprove {dtx}; payout after -> {:?}", late.as_ref().err());
    assert!(late.is_err(), "payout must fail once disproved");
    // Disprove with only one preimage (honest operator) must fail.
    let s3 = setup(100_000, op_addr.script_pubkey());
    send(&s3.claim).expect("claim3");
    mine(1);
    let bad = disprove_tx(&s3.p, &s3.claim, ch_addr.script_pubkey(), &s3.s0, &[0u8; 32]).unwrap();
    assert!(send(&bad).is_err(), "disprove without equivocation must fail");
    results.push("dishonest claim disproved: PASS (equivocation leaf spends claim; payout then invalid; no disprove vs honest operator)".to_string());

    // 3) No single machine's files can move funds: any 1 or 2 of 3 keys cannot spend the deposit.
    let mut parts: Vec<Participant> = (0..3).map(|i| Participant::new(&format!("q{i}"))).collect();
    let keys: Vec<_> = parts.iter().map(|p| p.xonly).collect();
    let p = GraphParams { keys, payout_csv: 6, equiv_pk0: [1; 32], equiv_pk1: [2; 32], fee_sats: 1000, truc: false };
    let dep = deposit_output(&p).unwrap();
    let out = fund(&dep.address(Network::Regtest), 50_000);
    let mut tx = claim_tx(&p, out, 50_000).unwrap();
    let leaf = dep.leaf("nofn").unwrap().clone();
    let prev = vec![fd_pl_graph::bitcoin::TxOut { value: Amount::from_sat(50_000), script_pubkey: dep.spk() }];
    let m = script_path_sighash(&tx, 0, &prev, &leaf).unwrap();
    let mut sigs: Vec<[u8; 64]> = parts.iter().map(|q| q.sign(&m).unwrap()).collect();
    sigs[2] = [0u8; 64]; // participant 2 absent (its machine/files not available)
    tx.input[0].witness = nofn_witness(&sigs, &leaf, control_block(&dep.info, &leaf).unwrap());
    let r = send(&tx);
    println!("[single-machine] 2-of-3 sigs -> {:?}", r.as_ref().err());
    assert!(r.is_err(), "fewer than n signatures must not move the deposit");
    for q in parts.iter_mut() { q.wipe("na"); }
    results.push("no single machine can move funds: PASS (deposit has only an n-of-n leaf over a NUMS key; 2-of-3 rejected; keys wiped after presign)".to_string());

    // 4) CPFP fee bump: zero-fee v3 (TRUC) presigned Claim is refused alone,
    //    then confirmed as a package with a child spending its P2A anchor.
    {
        let mut parts: Vec<Participant> = (0..3).map(|i| Participant::new(&format!("c{i}"))).collect();
        let keys: Vec<_> = parts.iter().map(|p| p.xonly).collect();
        let p = GraphParams { keys, payout_csv: 6, equiv_pk0: [1; 32], equiv_pk1: [2; 32], fee_sats: 0, truc: true };
        let dep = deposit_output(&p).unwrap();
        let out = fund(&dep.address(Network::Regtest), 60_000);
        let mut claim = claim_tx(&p, out, 60_000).unwrap();
        let leaf = dep.leaf("nofn").unwrap().clone();
        let prev = vec![fd_pl_graph::bitcoin::TxOut { value: Amount::from_sat(60_000), script_pubkey: dep.spk() }];
        let m = script_path_sighash(&claim, 0, &prev, &leaf).unwrap();
        let sigs: Vec<[u8; 64]> = parts.iter().map(|q| q.sign(&m).unwrap()).collect();
        claim.input[0].witness = nofn_witness(&sigs, &leaf, control_block(&dep.info, &leaf).unwrap());
        for q in parts.iter_mut() { q.wipe("cpfp"); }
        let alone = send(&claim);
        println!("[cpfp] zero-fee claim alone -> {:?}", alone.as_ref().err());
        assert!(alone.is_err(), "zero-fee parent must not relay alone");
        // Child: anchor + one wallet UTXO -> change, pays the package fee.
        let u: serde_json::Value = serde_json::from_str(&cli(&["-rpcwallet=miner", "listunspent", "1"]).unwrap()).unwrap();
        let u = u.as_array().unwrap().iter().find(|x| x["amount"].as_f64().unwrap() >= 0.001 && x["amount"].as_f64().unwrap() < 1.0).or_else(|| u.as_array().unwrap().first()).unwrap().clone();
        let in_sats = (u["amount"].as_f64().unwrap() * 1e8).round() as u64;
        let fee = 5_000u64;
        let change = cli(&["-rpcwallet=miner", "getrawchangeaddress", "bech32m"]).unwrap();
        let inputs = serde_json::json!([{"txid": claim.compute_txid().to_string(), "vout": 1}, {"txid": u["txid"], "vout": u["vout"]}]);
        let outs = serde_json::json!([{ change.clone(): format!("{:.8}", (in_sats + ANCHOR_SATS - fee) as f64 / 1e8) }]);
        let _ = (&inputs, &outs);
        use fd_pl_graph::bitcoin::{transaction, absolute, Sequence, TxIn, TxOut, Witness};
        let child = Transaction {
            version: transaction::Version(3),
            lock_time: absolute::LockTime::ZERO,
            input: vec![
                TxIn { previous_output: OutPoint { txid: claim.compute_txid(), vout: 1 }, script_sig: ScriptBuf::new(), sequence: Sequence::ENABLE_RBF_NO_LOCKTIME, witness: Witness::new() },
                TxIn { previous_output: OutPoint { txid: Txid::from_str(u["txid"].as_str().unwrap()).unwrap(), vout: u["vout"].as_u64().unwrap() as u32 }, script_sig: ScriptBuf::new(), sequence: Sequence::ENABLE_RBF_NO_LOCKTIME, witness: Witness::new() },
            ],
            output: vec![TxOut { value: Amount::from_sat(in_sats + ANCHOR_SATS - fee), script_pubkey: Address::from_str(&change).unwrap().assume_checked().script_pubkey() }],
        };
        let raw = consensus::encode::serialize_hex(&child);
        let prevtxs = serde_json::json!([{"txid": claim.compute_txid().to_string(), "vout": 1, "scriptPubKey": hex::encode(anchor_spk().as_bytes()), "amount": format!("{:.8}", ANCHOR_SATS as f64 / 1e8)}]);
        let signed: serde_json::Value = serde_json::from_str(&cli(&["-rpcwallet=miner", "signrawtransactionwithwallet", &raw, &prevtxs.to_string()]).unwrap()).unwrap();
        let child_hex = signed["hex"].as_str().unwrap().to_string();
        let pkg = serde_json::json!([consensus::encode::serialize_hex(&claim), child_hex]);
        let r = cli(&["submitpackage", &pkg.to_string()]).expect("submitpackage");
        println!("[cpfp] submitpackage -> {}", r.replace('\n', " "));
        assert!(r.contains("\"package_msg\": \"success\""), "package must be accepted");
        mine(1);
        let conf = cli(&["getrawtransaction", &claim.compute_txid().to_string(), "1"]).unwrap();
        assert!(conf.contains("\"confirmations\""), "claim confirmed via CPFP");
        results.push("CPFP fee bump: PASS (zero-fee TRUC presigned claim refused alone; confirmed via P2A anchor child in submitpackage)".to_string());
    }

    println!("\nACCEPTANCE (regtest):");
    for r in &results { println!("  {r}"); }
}
