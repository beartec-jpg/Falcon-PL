//! Off-chain LC keygen, Schnorr sign, Groth16 setup/prove/verify.

use anyhow::{bail, Context, Result};
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use clap::{Parser, Subcommand};
use fd_pl_snark::codec::{fr_to_be_hex, hex_to_bytes32};
use fd_pl_snark::schnorr::{
    committee_key_root, generate, pad_committee, statement_digest, LcPublic, LcSecret, LcSig,
};
use fd_pl_snark::{export_proof, export_vk, FplLcCircuit};
use rand::rngs::OsRng;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(name = "falcon-pl-snark")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Generate BabyJubJub LC keys (one secret file per id). Prefer one id per seat.
    GenKeys {
        #[arg(long)]
        out_dir: PathBuf,
        #[arg(long, value_delimiter = ',', default_value = "v1,v2,v3,v4,v5,v6")]
        validators: Vec<String>,
    },
    /// Build committee.json from `*.pub.json` only (no secrets).
    CommitteeAssemble {
        #[arg(long)]
        pubs_dir: PathBuf,
    },
    /// Print MiMC merkle root of a committee (pad to 8 with identity).
    CommitteeRoot {
        #[arg(long)]
        keys_dir: PathBuf,
    },
    /// Sign an LC header statement with one validator secret.
    Sign {
        #[arg(long)]
        secret: PathBuf,
        #[arg(long)]
        height: u64,
        #[arg(long)]
        parent: String,
        #[arg(long)]
        header: String,
        #[arg(long)]
        claim_root: String,
        #[arg(long, default_value_t = 2300)]
        network_id: u32,
        #[arg(long)]
        out: PathBuf,
    },
    /// Sign many LC statements with one secret (catch-up). JSON array of
    /// `{height, parent, header, claim_root, network_id?}`.
    SignBatch {
        #[arg(long)]
        secret: PathBuf,
        #[arg(long)]
        statements: PathBuf,
        #[arg(long)]
        out_dir: PathBuf,
        #[arg(long, default_value_t = 2300)]
        network_id: u32,
    },
    /// Circuit-specific Groth16 setup. Test seed is NOT a ceremony.
    Setup {
        #[arg(long)]
        out_dir: PathBuf,
        /// If set, deterministic TEST setup (toxic waste is this seed). Never mainnet.
        #[arg(long, default_value_t = false)]
        test_seed: bool,
    },
    /// Groth16 from a public Hermez/PSE `.ptau` (τ unknown). Initial δ=1.
    SetupFromPtau {
        #[arg(long)]
        ptau: PathBuf,
        #[arg(long)]
        out_dir: PathBuf,
    },
    /// Phase-2 δ contribution. Run on each operator; pk is public.
    Contribute {
        #[arg(long)]
        pk: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value = "anon")]
        tag: String,
    },
    /// Final beacon after operator contributions (public entropy).
    Beacon {
        #[arg(long)]
        pk: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        entropy: String,
        #[arg(long, default_value = "beacon")]
        tag: String,
    },
    /// Prove k-of-n LC signatures. Permissionless: needs pks + sigs, not secrets.
    Prove {
        #[arg(long)]
        pk: PathBuf,
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long)]
        sigs_dir: PathBuf,
        #[arg(long)]
        height: u64,
        #[arg(long)]
        parent: String,
        #[arg(long)]
        header: String,
        #[arg(long)]
        claim_root: String,
        #[arg(long, default_value_t = 2300)]
        network_id: u32,
        #[arg(long, default_value_t = 4)]
        threshold: u64,
        #[arg(long)]
        out: PathBuf,
    },
    /// ark-groth16 verify (`proof.json` + `proof.ark`, `vk.bin`)
    Verify {
        #[arg(long)]
        vk: PathBuf,
        #[arg(long)]
        proof: PathBuf,
    },
    /// Sign the Falcon-512 QC statement with a validator Falcon secret.
    /// Secret JSON: `{ "id": "v1", "secret_key": "<hex>" }` (node key export).
    FalconSign {
        #[arg(long)]
        secret: PathBuf,
        #[arg(long)]
        height: u64,
        #[arg(long)]
        parent: String,
        #[arg(long)]
        header: String,
        #[arg(long)]
        claim_root: String,
        #[arg(long, default_value_t = 2300)]
        network_id: u32,
        #[arg(long)]
        out: PathBuf,
    },
    /// MiMC merkle of Falcon-512 pks (`*.pub.json` with `{id, pk_hex}`).
    FalconCommitteeRoot {
        #[arg(long)]
        keys_dir: PathBuf,
    },
    /// Circuit-specific Groth16 setup for Falcon-512 QC. Not the live LC pk.
    FalconSetup {
        #[arg(long)]
        out_dir: PathBuf,
        #[arg(long, default_value_t = false)]
        test_seed: bool,
    },
    /// Falcon-512 QC Groth16 from a public Hermez/PSE `.ptau` (OPEN=4, τ unknown).
    /// Live Sepolia uses OPEN=1: `falcon-setup-one-from-ptau`.
    FalconSetupFromPtau {
        #[arg(long)]
        ptau: PathBuf,
        #[arg(long)]
        out_dir: PathBuf,
    },
    /// Groth16 setup for OPEN=1 Falcon-512 (11 public inputs, signer index).
    FalconSetupOne {
        #[arg(long)]
        out_dir: PathBuf,
        #[arg(long, default_value_t = false)]
        test_seed: bool,
    },
    /// OPEN=1 Groth16 from a public Hermez/PSE `.ptau` (τ unknown). Initial δ=1.
    /// Follow with `contribute` then `beacon`. Matches live FalconQcBridge.
    FalconSetupOneFromPtau {
        #[arg(long)]
        ptau: PathBuf,
        #[arg(long)]
        out_dir: PathBuf,
        #[arg(long, default_value = "v1")]
        domain: String,
    },
    /// Prove one Falcon-512 signature (OPEN=1). Four of these + distinct
    /// public indices is the 4-of-n path that fits 15GiB setup.
    FalconProveOne {
        #[arg(long)]
        pk: PathBuf,
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long)]
        sigs_dir: PathBuf,
        #[arg(long)]
        height: u64,
        #[arg(long)]
        parent: String,
        #[arg(long)]
        header: String,
        #[arg(long)]
        claim_root: String,
        #[arg(long, default_value_t = 2300)]
        network_id: u32,
        #[arg(long, default_value = "v1")]
        domain: String,
        #[arg(long, default_value = "")]
        next_keys_dir: PathBuf,
        #[arg(long)]
        out: PathBuf,
    },
    /// Prove 4 Falcon-512 signatures. Permissionless given pks + sigs.
    FalconProve {
        #[arg(long)]
        pk: PathBuf,
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long)]
        sigs_dir: PathBuf,
        #[arg(long)]
        height: u64,
        #[arg(long)]
        parent: String,
        #[arg(long)]
        header: String,
        #[arg(long)]
        claim_root: String,
        #[arg(long, default_value_t = 2300)]
        network_id: u32,
        #[arg(long, default_value_t = 4)]
        threshold: u64,
        #[arg(long)]
        out: PathBuf,
    },
}

fn load_json<T: serde::de::DeserializeOwned>(p: &Path) -> Result<T> {
    let t = fs::read_to_string(p).with_context(|| p.display().to_string())?;
    Ok(serde_json::from_str(&t)?)
}

fn write_json(p: &Path, v: &impl serde::Serialize) -> Result<()> {
    fs::write(p, serde_json::to_string_pretty(v)? + "\n")?;
    Ok(())
}

fn write_groth16_dir(out_dir: &Path, pk: &ark_groth16::ProvingKey<ark_bn254::Bn254>) -> Result<()> {
    fs::create_dir_all(out_dir)?;
    fd_pl_snark::save_pk(pk, out_dir.join("pk.bin")).map_err(|e| anyhow::anyhow!(e))?;
    fd_pl_snark::save_vk_bin(&pk.vk, out_dir.join("vk.bin")).map_err(|e| anyhow::anyhow!(e))?;
    fd_pl_snark::save_vk_json(&pk.vk, out_dir.join("vk.json")).map_err(|e| anyhow::anyhow!(e))?;
    write_json(&out_dir.join("vk.sol.json"), &export_vk(&pk.vk))?;
    Ok(())
}

fn load_one_pk(p: &Path) -> Result<LcPublic> {
    let text = fs::read_to_string(p).with_context(|| p.display().to_string())?;
    if let Ok(pk) = serde_json::from_str::<LcPublic>(&text) {
        if !pk.id.is_empty() && !pk.pk_x.is_empty() {
            return Ok(pk);
        }
    }
    let sec: LcSecret = serde_json::from_str(&text)?;
    Ok(sec.public())
}

fn load_committee(dir: &Path) -> Result<Vec<LcPublic>> {
    let meta_path = dir.join("committee.json");
    if meta_path.exists() {
        let meta: serde_json::Value = load_json(&meta_path)?;
        if let Some(ids) = meta.get("ids").and_then(|v| v.as_array()) {
            let mut out = Vec::new();
            for idv in ids {
                let id = idv.as_str().context("committee.json ids")?;
                let pubp = dir.join(format!("{id}.pub.json"));
                let secp = dir.join(format!("{id}.json"));
                let pk = if pubp.exists() {
                    load_one_pk(&pubp)?
                } else if secp.exists() {
                    load_one_pk(&secp)?
                } else {
                    bail!("missing LC pub for {id} in {}", dir.display());
                };
                out.push(pk);
            }
            if out.is_empty() {
                bail!("empty committee.json ids in {}", dir.display());
            }
            return Ok(pad_committee(out));
        }
    }
    let mut ents: Vec<_> = fs::read_dir(dir)?.collect();
    ents.sort_by_key(|e| e.as_ref().ok().map(|x| x.file_name()).unwrap_or_default());
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for ent in ents {
        let ent = ent?;
        let name = ent.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".pub.json") {
            continue;
        }
        let pk = load_one_pk(&ent.path())?;
        if seen.insert(pk.id.clone()) {
            out.push(pk);
        }
    }
    if out.is_empty() {
        for ent in fs::read_dir(dir)? {
            let ent = ent?;
            let name = ent.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".json") || name.starts_with("pl_") {
                continue;
            }
            if name == "committee.json" || name.starts_with("pad") || name.ends_with(".pub.json") {
                continue;
            }
            if let Ok(pk) = load_one_pk(&ent.path()) {
                if seen.insert(pk.id.clone()) {
                    out.push(pk);
                }
            }
        }
    }
    if out.is_empty() {
        bail!("no LC keys in {}", dir.display());
    }
    Ok(pad_committee(out))
}

fn load_sigs(dir: &Path, height: Option<u64>, header: Option<&str>, parent: Option<&str>) -> Result<Vec<LcSig>> {
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for ent in fs::read_dir(dir)? {
        let ent = ent?;
        let name = ent.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".json") {
            continue;
        }
        let raw: serde_json::Value = load_json(&ent.path())?;
        if let Some(h) = height {
            if let Some(got) = raw.get("height").and_then(|v| v.as_u64()) {
                if got != h {
                    continue;
                }
            }
        }
        if let Some(want) = header {
            if let Some(got) = raw.get("header").and_then(|v| v.as_str()) {
                if norm_hex(got) != norm_hex(want) {
                    continue;
                }
            }
        }
        if let Some(want) = parent {
            if let Some(got) = raw.get("parent").and_then(|v| v.as_str()) {
                if norm_hex(got) != norm_hex(want) {
                    continue;
                }
            }
        }
        let sig = if raw.get("sig").is_some() {
            serde_json::from_value(raw["sig"].clone())?
        } else {
            serde_json::from_value(raw)?
        };
        let sig: LcSig = sig;
        if seen.insert(sig.id.clone()) {
            out.push(sig);
        }
    }
    Ok(out)
}

fn norm_hex(s: &str) -> String {
    s.trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .to_ascii_lowercase()
}

fn main() -> Result<()> {
    let args = Args::parse();
    match args.cmd {
        Cmd::GenKeys { out_dir, validators } => {
            fs::create_dir_all(&out_dir)?;
            let mut rng = OsRng;
            let mut pubs = Vec::new();
            for id in &validators {
                let sec = generate(id, &mut rng);
                let sec_path = out_dir.join(format!("{id}.json"));
                write_json(&sec_path, &sec)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(&sec_path, fs::Permissions::from_mode(0o600));
                }
                let pk = sec.public();
                write_json(&out_dir.join(format!("{id}.pub.json")), &pk)?;
                pubs.push(pk);
            }
            if validators.len() >= 2 {
                let padded = pad_committee(pubs);
                let root = committee_key_root(&padded).map_err(|e| anyhow::anyhow!(e))?;
                let meta = serde_json::json!({
                    "scheme": "babyjubjub-schnorr-mimc7-v1",
                    "n": validators.len(),
                    "padded": 8,
                    "key_root": fr_to_be_hex(root),
                    "note": "LC keys are per-validator. Do not collect secrets on one disk.",
                });
                write_json(&out_dir.join("committee.json"), &meta)?;
                println!("lcKeyRoot 0x{}", fr_to_be_hex(root));
            } else {
                println!("wrote {} (pub + secret). assemble committee from all pubs.", out_dir.display());
            }
        }
        Cmd::CommitteeAssemble { pubs_dir } => {
            let mut pubs = Vec::new();
            let mut ids = Vec::new();
            let mut ents: Vec<_> = fs::read_dir(&pubs_dir)?.collect();
            ents.sort_by_key(|e| e.as_ref().ok().map(|x| x.file_name()).unwrap_or_default());
            for ent in ents {
                let ent = ent?;
                let name = ent.file_name().to_string_lossy().into_owned();
                if !name.ends_with(".pub.json") {
                    continue;
                }
                let pk: LcPublic = load_json(&ent.path())?;
                ids.push(pk.id.clone());
                pubs.push(pk);
            }
            if pubs.is_empty() {
                bail!("no *.pub.json in {}", pubs_dir.display());
            }
            let n = pubs.len();
            let padded = pad_committee(pubs);
            let root = committee_key_root(&padded).map_err(|e| anyhow::anyhow!(e))?;
            let meta = serde_json::json!({
                "scheme": "babyjubjub-schnorr-mimc7-v1",
                "n": n,
                "padded": 8,
                "ids": ids,
                "key_root": fr_to_be_hex(root),
                "note": "assembled from public keys only",
            });
            write_json(&pubs_dir.join("committee.json"), &meta)?;
            println!("lcKeyRoot 0x{}", fr_to_be_hex(root));
        }
        Cmd::CommitteeRoot { keys_dir } => {
            let pks = load_committee(&keys_dir)?;
            let root = committee_key_root(&pks).map_err(|e| anyhow::anyhow!(e))?;
            println!("0x{}", fr_to_be_hex(root));
        }
        Cmd::Sign {
            secret,
            height,
            parent,
            header,
            claim_root,
            network_id,
            out,
        } => {
            let sec: LcSecret = load_json(&secret)?;
            let p = hex_to_bytes32(&parent).map_err(|e| anyhow::anyhow!(e))?;
            let h = hex_to_bytes32(&header).map_err(|e| anyhow::anyhow!(e))?;
            let c = hex_to_bytes32(&claim_root).map_err(|e| anyhow::anyhow!(e))?;
            let stmt = statement_digest(height, &p, &h, &c, network_id);
            let sig = sec
                .sign_stmt(stmt, &mut OsRng)
                .map_err(|e| anyhow::anyhow!(e))?;
            write_json(&out, &sig)?;
            println!("signed {} -> {}", sec.id, out.display());
        }
        Cmd::SignBatch {
            secret,
            statements,
            out_dir,
            network_id,
        } => {
            let sec: LcSecret = load_json(&secret)?;
            let rows: Vec<serde_json::Value> = load_json(&statements)?;
            fs::create_dir_all(&out_dir)?;
            let mut n = 0u64;
            for row in rows {
                let height = row
                    .get("height")
                    .and_then(|v| v.as_u64())
                    .context("statement height")?;
                let parent = row
                    .get("parent")
                    .and_then(|v| v.as_str())
                    .context("statement parent")?;
                let header = row
                    .get("header")
                    .and_then(|v| v.as_str())
                    .context("statement header")?;
                let claim_root = row
                    .get("claim_root")
                    .and_then(|v| v.as_str())
                    .context("statement claim_root")?;
                let nid = row
                    .get("network_id")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
                    .unwrap_or(network_id);
                let p = hex_to_bytes32(parent).map_err(|e| anyhow::anyhow!(e))?;
                let h = hex_to_bytes32(header).map_err(|e| anyhow::anyhow!(e))?;
                let c = hex_to_bytes32(claim_root).map_err(|e| anyhow::anyhow!(e))?;
                let stmt = statement_digest(height, &p, &h, &c, nid);
                let sig = sec
                    .sign_stmt(stmt, &mut OsRng)
                    .map_err(|e| anyhow::anyhow!(e))?;
                let att = serde_json::json!({
                    "height": height,
                    "header": norm_hex(header),
                    "parent": norm_hex(parent),
                    "claim_root": norm_hex(claim_root),
                    "sig": sig,
                });
                let path = out_dir.join(format!("{height:010}-{}.json", sec.id));
                write_json(&path, &att)?;
                n += 1;
            }
            println!("signed {n} -> {}", out_dir.display());
        }
        Cmd::Setup { out_dir, test_seed } => {
            fs::create_dir_all(&out_dir)?;
            let (pk, vk) = if test_seed {
                eprintln!("WARNING: test-seed setup; toxic waste is the seed. Not for mainnet.");
                let mut rng = ChaCha20Rng::from_seed(*b"FalconPL-Groth16-TEST-NOT-PROD!!");
                fd_pl_snark::setup(&mut rng).map_err(|e| anyhow::anyhow!(e))?
            } else {
                eprintln!("circuit-specific setup with OsRng (not an MPC ceremony)");
                fd_pl_snark::setup(&mut OsRng).map_err(|e| anyhow::anyhow!(e))?
            };
            fd_pl_snark::save_pk(&pk, out_dir.join("pk.bin")).map_err(|e| anyhow::anyhow!(e))?;
            fd_pl_snark::save_vk_bin(&vk, out_dir.join("vk.bin")).map_err(|e| anyhow::anyhow!(e))?;
            fd_pl_snark::save_vk_json(&vk, out_dir.join("vk.json")).map_err(|e| anyhow::anyhow!(e))?;
            write_json(
                &out_dir.join("vk.sol.json"),
                &export_vk(&vk),
            )?;
            println!("wrote {}", out_dir.display());
        }
        Cmd::SetupFromPtau { ptau, out_dir } => {
            fs::create_dir_all(&out_dir)?;
            let ptau_hash = fd_pl_snark::ceremony::ptau_sha256(&ptau).map_err(|e| anyhow::anyhow!(e))?;
            eprintln!("ptau sha256 {ptau_hash}");
            let (pk, vk, digest) =
                fd_pl_snark::ceremony::setup_from_ptau(&ptau).map_err(|e| anyhow::anyhow!(e))?;
            fd_pl_snark::save_pk(&pk, out_dir.join("pk.bin")).map_err(|e| anyhow::anyhow!(e))?;
            fd_pl_snark::save_vk_bin(&vk, out_dir.join("vk.bin")).map_err(|e| anyhow::anyhow!(e))?;
            fd_pl_snark::save_vk_json(&vk, out_dir.join("vk.json")).map_err(|e| anyhow::anyhow!(e))?;
            write_json(&out_dir.join("vk.sol.json"), &export_vk(&vk))?;
            write_json(
                &out_dir.join("ptau.json"),
                &serde_json::json!({
                    "ptau_sha256": ptau_hash,
                    "setup_digest": digest,
                    "source": "powersOfTau28_hez (PSE / Hermez)",
                }),
            )?;
            println!("wrote {} setup_digest {digest}", out_dir.display());
        }
        Cmd::Contribute { pk, out, tag } => {
            let proving = fd_pl_snark::load_pk(&pk).map_err(|e| anyhow::anyhow!(e))?;
            let (next, transcript) = fd_pl_snark::ceremony::contribute_delta(&proving, &mut OsRng, &tag)
                .map_err(|e| anyhow::anyhow!(e))?;
            if let Some(dir) = out.parent() {
                fs::create_dir_all(dir)?;
            }
            fd_pl_snark::save_pk(&next, &out).map_err(|e| anyhow::anyhow!(e))?;
            fd_pl_snark::save_vk_bin(&next.vk, out.with_extension("vk.bin")).map_err(|e| anyhow::anyhow!(e))?;
            fd_pl_snark::save_vk_json(&next.vk, out.with_extension("vk.json")).map_err(|e| anyhow::anyhow!(e))?;
            write_json(&out.with_extension("vk.sol.json"), &export_vk(&next.vk))?;
            println!("contribute {tag} transcript {transcript}");
        }
        Cmd::Beacon {
            pk,
            out,
            entropy,
            tag,
        } => {
            let proving = fd_pl_snark::load_pk(&pk).map_err(|e| anyhow::anyhow!(e))?;
            let raw = hex::decode(entropy.trim().trim_start_matches("0x")).context("beacon hex")?;
            let (next, transcript) =
                fd_pl_snark::ceremony::contribute_beacon(&proving, &raw, &tag).map_err(|e| anyhow::anyhow!(e))?;
            fd_pl_snark::save_pk(&next, &out).map_err(|e| anyhow::anyhow!(e))?;
            fd_pl_snark::save_vk_bin(&next.vk, out.with_extension("vk.bin")).map_err(|e| anyhow::anyhow!(e))?;
            fd_pl_snark::save_vk_json(&next.vk, out.with_extension("vk.json")).map_err(|e| anyhow::anyhow!(e))?;
            write_json(&out.with_extension("vk.sol.json"), &export_vk(&next.vk))?;
            println!("beacon {tag} transcript {transcript}");
        }
        Cmd::Prove {
            pk,
            keys_dir,
            sigs_dir,
            height,
            parent,
            header,
            claim_root,
            network_id,
            threshold,
            out,
        } => {
            let proving = fd_pl_snark::load_pk(&pk).map_err(|e| anyhow::anyhow!(e))?;
            let committee = load_committee(&keys_dir)?;
            let sigs = load_sigs(&sigs_dir, Some(height), Some(&header), Some(&parent))?;
            let p = hex_to_bytes32(&parent).map_err(|e| anyhow::anyhow!(e))?;
            let h = hex_to_bytes32(&header).map_err(|e| anyhow::anyhow!(e))?;
            let c = hex_to_bytes32(&claim_root).map_err(|e| anyhow::anyhow!(e))?;
            let circuit = FplLcCircuit::from_committee(
                height, &p, &h, &c, network_id, threshold, &committee, &sigs,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            let public = circuit.public_values();
            let proof =
                fd_pl_snark::prove(&proving, circuit, &mut OsRng).map_err(|e| anyhow::anyhow!(e))?;
            write_json(&out, &export_proof(&proof, &public))?;
            let mut ark_buf = Vec::new();
            proof
                .serialize_uncompressed(&mut ark_buf)
                .map_err(|e| anyhow::anyhow!(e))?;
            let ark_path = out.with_extension("ark");
            fs::write(&ark_path, ark_buf)?;
            println!("proof {} {}", out.display(), ark_path.display());
        }
        Cmd::Verify { vk, proof } => {
            let verifying = fd_pl_snark::load_vk_bin(&vk).map_err(|e| anyhow::anyhow!(e))?;
            let ark_path = {
                let p = proof.clone();
                if p.extension().and_then(|e| e.to_str()) == Some("json") {
                    p.with_extension("ark")
                } else {
                    p
                }
            };
            if !ark_path.exists() {
                bail!(
                    "need ark-serialized proof at {} (prove writes proof.json + proof.ark)",
                    ark_path.display()
                );
            }
            let buf = fs::read(&ark_path)?;
            let ark_proof = ark_groth16::Proof::<ark_bn254::Bn254>::deserialize_uncompressed(&buf[..])
                .map_err(|e| anyhow::anyhow!(e))?;
            let sol: fd_pl_snark::SolidityProof = if proof.extension().and_then(|e| e.to_str()) == Some("json")
            {
                load_json(&proof)?
            } else {
                bail!("pass the JSON proof to read public inputs")
            };
            let public: Vec<_> = sol
                .input
                .iter()
                .map(|s| fd_pl_snark::fr_from_be_hex(s).map_err(|e| anyhow::anyhow!(e)))
                .collect::<Result<Vec<_>>>()?;
            let ok = fd_pl_snark::verify(&verifying, &public, &ark_proof)
                .map_err(|e| anyhow::anyhow!(e))?;
            println!("{ok}");
            if !ok {
                bail!("invalid proof");
            }
        }
        Cmd::FalconSign {
            secret,
            height,
            parent,
            header,
            claim_root,
            network_id,
            out,
        } => {
            use fd_pl_snark::falcon::native::{statement_bytes, FalconSig};
            use pqcrypto_falcon::falcon512;
            use pqcrypto_traits::sign::{DetachedSignature as _, SecretKey as _};
            let raw: serde_json::Value = load_json(&secret)?;
            let id = raw
                .get("id")
                .and_then(|v| v.as_str())
                .context("secret id")?
                .to_string();
            let sk_hex = raw
                .get("secret_key")
                .or_else(|| raw.get("sk"))
                .and_then(|v| v.as_str())
                .context("secret_key hex")?;
            let sk_bytes = hex::decode(sk_hex.trim().trim_start_matches("0x"))?;
            let sk = falcon512::SecretKey::from_bytes(&sk_bytes)
                .map_err(|e| anyhow::anyhow!("falcon sk: {e}"))?;
            let p = hex_to_bytes32(&parent).map_err(|e| anyhow::anyhow!(e))?;
            let h = hex_to_bytes32(&header).map_err(|e| anyhow::anyhow!(e))?;
            let c = hex_to_bytes32(&claim_root).map_err(|e| anyhow::anyhow!(e))?;
            let msg = statement_bytes(height, &p, &h, &c, network_id);
            let sig = falcon512::detached_sign(&msg, &sk);
            let out_sig = FalconSig {
                id: id.clone(),
                sig_hex: hex::encode(sig.as_bytes()),
            };
            write_json(&out, &out_sig)?;
            println!("falcon-signed {id} -> {}", out.display());
        }
        Cmd::FalconCommitteeRoot { keys_dir } => {
            use fd_pl_snark::falcon::native::{committee_key_root, decode_pk, FalconPublic};
            let mut pubs: Vec<FalconPublic> = Vec::new();
            let mut ents: Vec<_> = fs::read_dir(&keys_dir)?.collect();
            ents.sort_by_key(|e| e.as_ref().ok().map(|x| x.file_name()).unwrap_or_default());
            for ent in ents {
                let ent = ent?;
                let name = ent.file_name().to_string_lossy().into_owned();
                if !name.ends_with(".pub.json") {
                    continue;
                }
                pubs.push(load_json(&ent.path())?);
            }
            if pubs.is_empty() {
                bail!("no Falcon *.pub.json in {}", keys_dir.display());
            }
            let mut hs = Vec::new();
            for p in &pubs {
                let raw = hex::decode(p.pk_hex.trim().trim_start_matches("0x"))?;
                hs.push(decode_pk(&raw).map_err(|e| anyhow::anyhow!(e))?);
            }
            let root = committee_key_root(&hs).map_err(|e| anyhow::anyhow!(e))?;
            println!("0x{}", fr_to_be_hex(root));
        }
        Cmd::FalconSetup { out_dir, test_seed } => {
            fs::create_dir_all(&out_dir)?;
            let (pk, vk) = if test_seed {
                eprintln!("WARNING: Falcon QC test-seed setup; toxic waste is the seed.");
                let mut rng = ChaCha20Rng::from_seed(*b"FalconPL-FalconQC-TEST-NOT-PROD!");
                fd_pl_snark::setup_falcon(&mut rng).map_err(|e| anyhow::anyhow!(e))?
            } else {
                eprintln!("Falcon-512 QC circuit-specific setup with OsRng (not an MPC ceremony)");
                fd_pl_snark::setup_falcon(&mut OsRng).map_err(|e| anyhow::anyhow!(e))?
            };
            fd_pl_snark::save_pk(&pk, out_dir.join("pk.bin")).map_err(|e| anyhow::anyhow!(e))?;
            fd_pl_snark::save_vk_bin(&vk, out_dir.join("vk.bin")).map_err(|e| anyhow::anyhow!(e))?;
            fd_pl_snark::save_vk_json(&vk, out_dir.join("vk.json")).map_err(|e| anyhow::anyhow!(e))?;
            write_json(&out_dir.join("vk.sol.json"), &export_vk(&vk))?;
            println!("wrote falcon-qc {}", out_dir.display());
        }
        Cmd::FalconSetupOne { out_dir, test_seed } => {
            fs::create_dir_all(&out_dir)?;
            let (pk, vk) = if test_seed {
                eprintln!("WARNING: Falcon OPEN=1 test-seed setup; toxic waste is the seed.");
                let mut rng = ChaCha20Rng::from_seed(*b"FalconPL-Falcon1-TEST-NOT-PROD!!");
                fd_pl_snark::setup_falcon_open::<1, _>(&mut rng).map_err(|e| anyhow::anyhow!(e))?
            } else {
                eprintln!("Falcon-512 OPEN=1 Groth16 setup (OsRng, not an MPC ceremony)");
                fd_pl_snark::setup_falcon_open::<1, _>(&mut OsRng).map_err(|e| anyhow::anyhow!(e))?
            };
            let _ = vk;
            write_groth16_dir(&out_dir, &pk)?;
            println!("wrote falcon-qc OPEN=1 {}", out_dir.display());
        }
        Cmd::FalconSetupOneFromPtau { ptau, out_dir, domain } => {
            fs::create_dir_all(&out_dir)?;
            let ptau_hash = fd_pl_snark::ceremony::ptau_sha256(&ptau).map_err(|e| anyhow::anyhow!(e))?;
            eprintln!("falcon-qc OPEN=1 domain={domain} ptau sha256 {ptau_hash}");
            let (pk, _vk, digest) = if domain == "v2" {
                fd_pl_snark::ceremony::setup_falcon_v2_from_ptau(&ptau)
                    .map_err(|e| anyhow::anyhow!(e))?
            } else {
                fd_pl_snark::ceremony::setup_falcon_open_from_ptau::<1>(&ptau)
                    .map_err(|e| anyhow::anyhow!(e))?
            };
            write_groth16_dir(&out_dir, &pk)?;
            write_json(
                &out_dir.join("ptau.json"),
                &serde_json::json!({
                    "circuit": if domain == "v2" { "FplFalconQcCircuitV2" } else { "FplFalconQcCircuit<1>" },
                    "domain": domain,
                    "ptau_sha256": ptau_hash,
                    "setup_digest": digest,
                    "source": "powersOfTau28_hez (PSE / Hermez)",
                    "delta": "1 (run contribute then beacon)",
                    "note": "OPEN=1; v2 has 16 public inputs and no free claimRoot",
                }),
            )?;
            println!(
                "wrote falcon-qc OPEN=1 domain={domain} {} setup_digest {digest} (δ=1; run contribute then beacon)",
                out_dir.display()
            );
        }
        Cmd::FalconSetupFromPtau { ptau, out_dir } => {
            fs::create_dir_all(&out_dir)?;
            let ptau_hash = fd_pl_snark::ceremony::ptau_sha256(&ptau).map_err(|e| anyhow::anyhow!(e))?;
            eprintln!("falcon-qc ptau sha256 {ptau_hash}");
            let (pk, vk, digest) = fd_pl_snark::ceremony::setup_falcon_from_ptau(&ptau)
                .map_err(|e| anyhow::anyhow!(e))?;
            fd_pl_snark::save_pk(&pk, out_dir.join("pk.bin")).map_err(|e| anyhow::anyhow!(e))?;
            fd_pl_snark::save_vk_bin(&vk, out_dir.join("vk.bin")).map_err(|e| anyhow::anyhow!(e))?;
            fd_pl_snark::save_vk_json(&vk, out_dir.join("vk.json")).map_err(|e| anyhow::anyhow!(e))?;
            write_json(&out_dir.join("vk.sol.json"), &export_vk(&vk))?;
            write_json(
                &out_dir.join("ptau.json"),
                &serde_json::json!({
                    "circuit": "FplFalconQcCircuit<4>",
                    "ptau_sha256": ptau_hash,
                    "setup_digest": digest,
                    "source": "powersOfTau28_hez (PSE / Hermez)",
                    "note": "not the live BabyJub LC pk; lcKeyRoot on Sepolia is a different circuit",
                }),
            )?;
            println!("wrote falcon-qc {} setup_digest {digest}", out_dir.display());
        }
        Cmd::FalconProveOne {
            pk,
            keys_dir,
            sigs_dir,
            height,
            parent,
            header,
            claim_root,
            network_id,
            domain,
            next_keys_dir,
            out,
        } => {
            use fd_pl_snark::falcon::native::{FalconPublic, FalconSig};
            use fd_pl_snark::{FplFalconQcCircuit, FplFalconQcCircuitV2};
            fn load_pubs(dir: &PathBuf) -> anyhow::Result<Vec<FalconPublic>> {
                let mut committee: Vec<FalconPublic> = Vec::new();
                let mut ents: Vec<_> = fs::read_dir(dir)?.collect();
                ents.sort_by_key(|e| e.as_ref().ok().map(|x| x.file_name()).unwrap_or_default());
                for ent in ents {
                    let ent = ent?;
                    let name = ent.file_name().to_string_lossy().into_owned();
                    if name.ends_with(".pub.json") {
                        committee.push(load_json(&ent.path())?);
                    }
                }
                Ok(committee)
            }
            fn load_sigs(dir: &PathBuf) -> anyhow::Result<Vec<FalconSig>> {
                let mut sigs: Vec<FalconSig> = Vec::new();
                for ent in fs::read_dir(dir)? {
                    let ent = ent?;
                    let name = ent.file_name().to_string_lossy().into_owned();
                    if !name.ends_with(".json") {
                        continue;
                    }
                    let raw: serde_json::Value = load_json(&ent.path())?;
                    let sig = if raw.get("sig_hex").is_some() {
                        serde_json::from_value(raw)?
                    } else if raw.get("sig").is_some() {
                        serde_json::from_value(raw["sig"].clone())?
                    } else {
                        serde_json::from_value(raw)?
                    };
                    sigs.push(sig);
                }
                Ok(sigs)
            }
            let proving = fd_pl_snark::load_pk(&pk).map_err(|e| anyhow::anyhow!(e))?;
            let committee = load_pubs(&keys_dir)?;
            if committee.is_empty() {
                bail!("no Falcon *.pub.json in {}", keys_dir.display());
            }
            let mut sigs = load_sigs(&sigs_dir)?;
            if sigs.is_empty() {
                bail!("no Falcon sigs");
            }
            sigs.truncate(1);
            let p = hex_to_bytes32(&parent).map_err(|e| anyhow::anyhow!(e))?;
            let h = hex_to_bytes32(&header).map_err(|e| anyhow::anyhow!(e))?;
            if domain == "v2" {
                let next_dir = if next_keys_dir.as_os_str().is_empty() {
                    keys_dir.clone()
                } else {
                    next_keys_dir
                };
                let mut next = load_pubs(&next_dir)?;
                if next.len() < 6 {
                    bail!("v2 next committee needs 6 pubs, got {}", next.len());
                }
                next.truncate(6);
                let circuit = FplFalconQcCircuitV2::from_committee(
                    height, &p, &h, network_id, &committee, &next, &sigs[0],
                )
                .map_err(|e| anyhow::anyhow!(e))?;
                let public = circuit.public_values();
                let proof = fd_pl_snark::prove_falcon_v2(&proving, circuit, &mut OsRng)
                    .map_err(|e| anyhow::anyhow!(e))?;
                write_json(&out, &export_proof(&proof, &public))?;
                println!("falcon OPEN=1 v2 proof {} publics={}", out.display(), public.len());
            } else {
                let c = hex_to_bytes32(&claim_root).map_err(|e| anyhow::anyhow!(e))?;
                let circuit = FplFalconQcCircuit::<1>::from_committee(
                    height, &p, &h, &c, network_id, 1, &committee, &sigs,
                )
                .map_err(|e| anyhow::anyhow!(e))?;
                let public = circuit.public_values();
                let proof = fd_pl_snark::prove_falcon_open::<1, _>(&proving, circuit, &mut OsRng)
                    .map_err(|e| anyhow::anyhow!(e))?;
                write_json(&out, &export_proof(&proof, &public))?;
                println!("falcon OPEN=1 proof {} index {}", out.display(), public.last().map(|x| format!("{x:?}")).unwrap_or_default());
            }
        }
        Cmd::FalconProve {
            pk,
            keys_dir,
            sigs_dir,
            height,
            parent,
            header,
            claim_root,
            network_id,
            threshold,
            out,
        } => {
            use fd_pl_snark::falcon::native::{FalconPublic, FalconSig};
            use fd_pl_snark::FplFalconQcCircuit;
            let proving = fd_pl_snark::load_pk(&pk).map_err(|e| anyhow::anyhow!(e))?;
            let mut committee: Vec<FalconPublic> = Vec::new();
            let mut ents: Vec<_> = fs::read_dir(&keys_dir)?.collect();
            ents.sort_by_key(|e| e.as_ref().ok().map(|x| x.file_name()).unwrap_or_default());
            for ent in ents {
                let ent = ent?;
                let name = ent.file_name().to_string_lossy().into_owned();
                if name.ends_with(".pub.json") {
                    committee.push(load_json(&ent.path())?);
                }
            }
            if committee.is_empty() {
                bail!("no Falcon *.pub.json in {}", keys_dir.display());
            }
            let mut sigs: Vec<FalconSig> = Vec::new();
            for ent in fs::read_dir(&sigs_dir)? {
                let ent = ent?;
                let name = ent.file_name().to_string_lossy().into_owned();
                if !name.ends_with(".json") {
                    continue;
                }
                let raw: serde_json::Value = load_json(&ent.path())?;
                let sig = if raw.get("sig_hex").is_some() {
                    serde_json::from_value(raw)?
                } else if raw.get("sig").is_some() {
                    serde_json::from_value(raw["sig"].clone())?
                } else {
                    serde_json::from_value(raw)?
                };
                sigs.push(sig);
            }
            let p = hex_to_bytes32(&parent).map_err(|e| anyhow::anyhow!(e))?;
            let h = hex_to_bytes32(&header).map_err(|e| anyhow::anyhow!(e))?;
            let c = hex_to_bytes32(&claim_root).map_err(|e| anyhow::anyhow!(e))?;
            let circuit = FplFalconQcCircuit::<4>::from_committee(
                height, &p, &h, &c, network_id, threshold, &committee, &sigs,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            let public = circuit.public_values();
            let proof =
                fd_pl_snark::prove_falcon(&proving, circuit, &mut OsRng).map_err(|e| anyhow::anyhow!(e))?;
            write_json(&out, &export_proof(&proof, &public))?;
            let mut ark_buf = Vec::new();
            proof
                .serialize_uncompressed(&mut ark_buf)
                .map_err(|e| anyhow::anyhow!(e))?;
            fs::write(out.with_extension("ark"), ark_buf)?;
            println!("falcon proof {}", out.display());
        }
    }
    Ok(())
}
