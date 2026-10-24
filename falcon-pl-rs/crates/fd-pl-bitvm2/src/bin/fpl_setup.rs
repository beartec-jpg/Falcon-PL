//! Public Groth16 setup tooling (arkworks <-> snarkjs). Holds NO secret.
//!
//!   falcon-pl-bitvm2-setup r1cs  --circuit v1|v3 --out C.r1cs        circuit shape for `snarkjs zkey new`
//!   falcon-pl-bitvm2-setup demo-wtns --circuit v1|v3 --out W.wtns    sample satisfying witness (dry runs)
//!   falcon-pl-bitvm2-setup attest-wtns --input attest.json --out W.wtns [--public-out P.json]
//!        attest.json = {note_id, dest_h160_hex, amount, height, header_hex, pegout_txid_internal_hex, mimc_leaves:[hex]}
//!        (v1 = ledger M1 attest circuit; operator then runs `snarkjs groth16 prove final.zkey W.wtns proof.json public.json`)
//!   falcon-pl-bitvm2-setup v3-wtns --input attest.json --out W.wtns [--public-out P.json] [--binding-out B.hex]
//!        same statement, BitVM v3 circuit (FplWithdrawValidV3, 1 SHA256-bound public input) for a
//!        real withdrawal; B.hex = binding commit the graph carries (FPL_PROOF_DIR/binding_commit.hex)
//!   falcon-pl-bitvm2-setup v3-input --amount SATS --dest-h160 HEX --pegout-txid INTERNALHEX --height H
//!        --header-hex HEX [--note ID] --out attest.json     single-leaf statement (same as falcon-pl-bitvm2-v3-proof)
//!   falcon-pl-bitvm2-setup vk-import --vk verification_key.json [--out-bin vk.bin]
//!        prints the compressed VK hex (= BITVM2_WITHDRAW_VK_HEX for v1; vk.bin for BitVM v3), its sha256, nPublic
//!   falcon-pl-bitvm2-setup proof-import --vk verification_key.json --proof proof.json --public public.json [--out-bin proof.bin] [--public-bin public.bin]
//!        converts + verifies with arkworks; prints proof hex (ledger `proof_hex`) and public BE hex
//! Circuits: v1 = FplWithdrawValid (ledger Bitvm2AttestV2, BITVM2_WITHDRAW_VK_HEX);
//!           v3 = FplWithdrawValidV3 (BitVM connector C split verifier, vk.bin).
use ark_serialize::CanonicalSerialize;
use fd_pl_bitvm2::snarkjs_io as io;
use fd_pl_bitvm2::*;
use sha2::Digest;

fn arg(name: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter().position(|x| x == name).and_then(|i| a.get(i + 1).cloned())
}
fn req(name: &str) -> String {
    arg(name).unwrap_or_else(|| die(&format!("missing {name}")))
}
fn die(m: &str) -> ! {
    eprintln!("{m}");
    std::process::exit(1)
}
fn json(p: &str) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(p).unwrap_or_else(|e| die(&format!("{p}: {e}")))).unwrap_or_else(|e| die(&format!("{p}: {e}")))
}
fn hexn<const N: usize>(s: &str) -> [u8; N] {
    hex::decode(s.trim()).ok().and_then(|v| v.try_into().ok()).unwrap_or_else(|| die(&format!("need {N}-byte hex")))
}

fn demo_inner() -> FplWithdrawValid {
    let dest = [0x11u8; 20];
    let leaf = withdraw_leaf_fr("fpl-setup-demo", &dest, 10_000);
    let (root, path, bits) = build_tree(&[leaf], 0).unwrap();
    FplWithdrawValid::from_withdraw(2300, 10_000, &dest, root, 64, &[0x22; 32], "fpl-setup-demo", &[0x33; 32], &path, &bits, test_vk_hash()).unwrap()
}

fn synth(circuit: &str, inner: FplWithdrawValid) -> io::Synth {
    let r = match circuit {
        "v1" => io::synthesize(inner),
        "v3" => {
            let c = FplWithdrawValidV3::new(inner, [0x33; 32], [0x22; 32]).unwrap_or_else(|e| die(&e));
            io::synthesize(c)
        }
        c => die(&format!("unknown circuit {c} (v1|v3)")),
    };
    r.unwrap_or_else(|e| die(&e))
}

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    match cmd.as_str() {
        "r1cs" | "demo-wtns" => {
            let s = synth(&req("--circuit"), demo_inner());
            if !s.satisfied {
                die("demo witness does not satisfy the circuit");
            }
            let bytes = if cmd == "r1cs" { io::r1cs_bytes(&s) } else { io::wtns_bytes(&s) };
            std::fs::write(req("--out"), &bytes).unwrap();
            let pw = (s.num_constraints + s.num_instance).next_power_of_two().trailing_zeros();
            println!("{{\"constraints\":{},\"public\":{},\"wires\":{},\"min_ptau_power\":{},\"sha256\":\"{}\"}}", s.num_constraints, s.num_instance - 1, s.num_instance + s.num_witness, pw, hex::encode(sha2::Sha256::digest(&bytes)));
        }
        "v3-input" => {
            let dest: [u8; 20] = hexn(&req("--dest-h160"));
            let amount: u64 = req("--amount").parse().unwrap_or_else(|_| die("--amount"));
            let note = arg("--note").unwrap_or_else(|| "fpl-withdraw".into());
            let leaf = withdraw_ops::mimc_leaf_hex(&note, &dest, amount);
            let v = serde_json::json!({"note_id": note, "dest_h160_hex": hex::encode(dest), "amount": amount,
                "height": req("--height").parse::<u64>().unwrap_or_else(|_| die("--height")), "header_hex": req("--header-hex"),
                "pegout_txid_internal_hex": req("--pegout-txid"), "mimc_leaves": [leaf]});
            std::fs::write(req("--out"), v.to_string()).unwrap();
            println!("{v}");
        }
        "attest-wtns" | "v3-wtns" => {
            let v = json(&req("--input"));
            let s = |k: &str| v[k].as_str().unwrap_or_else(|| die(&format!("input.{k}"))).to_string();
            let dest: [u8; 20] = hexn(&s("dest_h160_hex"));
            let amount = v["amount"].as_u64().unwrap_or_else(|| die("input.amount"));
            let height = v["height"].as_u64().unwrap_or_else(|| die("input.height"));
            let leaves: Vec<String> = v["mimc_leaves"].as_array().unwrap_or_else(|| die("input.mimc_leaves")).iter().filter_map(|x| x.as_str().map(String::from)).collect();
            let (header, pegout): ([u8; 32], [u8; 32]) = (hexn(&s("header_hex")), hexn(&s("pegout_txid_internal_hex")));
            let c = withdraw_ops::attest_circuit(&s("note_id"), &dest, amount, height, &header, &pegout, &leaves).unwrap_or_else(|e| die(&e));
            let (public, st) = if cmd == "v3-wtns" {
                let c3 = FplWithdrawValidV3::new(c, pegout, header).unwrap_or_else(|e| die(&e));
                if let Some(b) = arg("--binding-out") {
                    std::fs::write(b, hex::encode(c3.binding_commit())).unwrap();
                }
                (c3.public_values(), io::synthesize(c3).unwrap_or_else(|e| die(&e)))
            } else {
                (c.public_values(), io::synthesize(c).unwrap_or_else(|e| die(&e)))
            };
            if !st.satisfied {
                die("attest witness does not satisfy the circuit (wrong leaves / note?)");
            }
            std::fs::write(req("--out"), io::wtns_bytes(&st)).unwrap();
            if let Some(p) = arg("--public-out") {
                std::fs::write(p, io::public_to_snarkjs(&public).to_string()).unwrap();
            }
            println!("{{\"public_be\":{:?}}}", public.iter().map(|f| hex::encode(fr_to_be32(*f))).collect::<Vec<_>>());
        }
        "vk-import" => {
            let vk = io::vk_from_snarkjs(&json(&req("--vk"))).unwrap_or_else(|e| die(&e));
            let b = groth16::vk_to_bytes(&vk).unwrap();
            if let Some(o) = arg("--out-bin") {
                std::fs::write(o, &b).unwrap();
            }
            println!("{{\"vk_hex\":\"{}\",\"sha256\":\"{}\",\"n_public\":{}}}", hex::encode(&b), hex::encode(sha2::Sha256::digest(&b)), vk.gamma_abc_g1.len() - 1);
        }
        "proof-import" => {
            let vk = io::vk_from_snarkjs(&json(&req("--vk"))).unwrap_or_else(|e| die(&e));
            let proof = io::proof_from_snarkjs(&json(&req("--proof"))).unwrap_or_else(|e| die(&e));
            let public = io::public_from_snarkjs(&json(&req("--public"))).unwrap_or_else(|e| die(&e));
            let ok = groth16::verify(&vk, &public, &proof).unwrap_or(false);
            if let Some(o) = arg("--out-bin") {
                let mut p = vec![];
                proof.a.serialize_compressed(&mut p).unwrap();
                proof.b.serialize_compressed(&mut p).unwrap();
                proof.c.serialize_compressed(&mut p).unwrap();
                std::fs::write(o, p).unwrap();
            }
            if let Some(o) = arg("--public-bin") {
                if public.len() != 1 {
                    die("--public-bin is for the 1-public v3 circuit");
                }
                let mut pb = vec![];
                public[0].serialize_compressed(&mut pb).unwrap();
                std::fs::write(o, pb).unwrap();
            }
            println!("{{\"verifies\":{ok},\"proof_hex\":\"{}\",\"public_be\":{:?}}}", hex::encode(proof_to_bytes(&proof).unwrap()), public.iter().map(|f| hex::encode(fr_to_be32(*f))).collect::<Vec<_>>());
            if !ok {
                std::process::exit(2);
            }
        }
        _ => die("usage: falcon-pl-bitvm2-setup r1cs|demo-wtns|attest-wtns|v3-wtns|v3-input|vk-import|proof-import ... (see source header)"),
    }
}
