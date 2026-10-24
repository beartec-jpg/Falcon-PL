//! Export an FplWithdrawValidV2 Groth16 proof for the BitVM split verifier.
//! Usage: falcon-pl-bitvm2-v2-proof --out DIR --amount SATS --dest-h160 HEX
//!          --pegout-txid HEX --height H --header-hex HEX [--note ID] [--seed HEX]
//! Writes DIR/{vk.bin,proof.bin,public.bin} (arkworks compressed; components
//! serialized separately so ark 0.5 tooling can read them).
//! height/header = the finalized FPL checkpoint the withdrawal is bound to (M1).
use ark_groth16::Groth16;
use ark_serialize::CanonicalSerialize;
use ark_snark::SNARK;
use fd_pl_bitvm2::*;
use rand::SeedableRng;

fn arg(name: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter().position(|x| x == name).and_then(|i| a.get(i + 1).cloned())
}
fn hex32(s: &str) -> [u8; 32] { hex::decode(s).expect("hex").try_into().expect("32 bytes") }

fn main() {
    let out = std::path::PathBuf::from(arg("--out").expect("--out"));
    let amount: u64 = arg("--amount").expect("--amount").parse().unwrap();
    let dest: [u8; 20] = hex::decode(arg("--dest-h160").expect("--dest-h160")).unwrap().try_into().unwrap();
    let pegout = hex32(&arg("--pegout-txid").expect("--pegout-txid"));
    let height: u64 = arg("--height").expect("--height").parse().unwrap();
    let header = hex32(&arg("--header-hex").expect("--header-hex"));
    let note = arg("--note").unwrap_or_else(|| "fpl-withdraw".into());
    let seed = arg("--seed").map(|s| hex32(&s)).unwrap_or([7u8; 32]);
    let leaf = withdraw_leaf_fr(&note, &dest, amount);
    let (root, path, bits) = build_tree(&[leaf], 0).unwrap();
    let c = FplWithdrawValidV2(FplWithdrawValid::from_withdraw(2300, amount, &dest, root, height, &header, &note, &pegout, &path, &bits, test_vk_hash()).unwrap());
    // Deterministic setup seed = testnet single-party VK (ceremony replaces this).
    let mut rng = rand::rngs::StdRng::from_seed(seed);
    let (pk, vk) = Groth16::<ark_bn254::Bn254>::circuit_specific_setup(c.clone(), &mut rng).unwrap();
    let public = c.public_values();
    let proof = Groth16::<ark_bn254::Bn254>::prove(&pk, c, &mut rng).unwrap();
    assert!(Groth16::<ark_bn254::Bn254>::verify(&vk, &public, &proof).unwrap());
    std::fs::create_dir_all(&out).unwrap();
    let mut v = vec![];
    vk.alpha_g1.serialize_compressed(&mut v).unwrap();
    vk.beta_g2.serialize_compressed(&mut v).unwrap();
    vk.gamma_g2.serialize_compressed(&mut v).unwrap();
    vk.delta_g2.serialize_compressed(&mut v).unwrap();
    vk.gamma_abc_g1.serialize_compressed(&mut v).unwrap();
    std::fs::write(out.join("vk.bin"), v).unwrap();
    let mut p = vec![];
    proof.a.serialize_compressed(&mut p).unwrap();
    proof.b.serialize_compressed(&mut p).unwrap();
    proof.c.serialize_compressed(&mut p).unwrap();
    std::fs::write(out.join("proof.bin"), p).unwrap();
    let mut pb = vec![];
    public[0].serialize_compressed(&mut pb).unwrap();
    std::fs::write(out.join("public.bin"), pb).unwrap();
    println!("{{\"public_digest_le\":\"{}\",\"height\":{},\"pegout_txid\":\"{}\"}}", hex::encode(&std::fs::read(out.join("public.bin")).unwrap()), height, hex::encode(pegout));
}
