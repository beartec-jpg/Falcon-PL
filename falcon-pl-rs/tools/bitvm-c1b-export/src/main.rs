//! Export BitVM chunk script digests for Falcon C.1b pack cache.
//!
//! Separate from the Falcon workspace: depends on vendored BitVM (ark 0.5).
//! Default mode hashes each partial tapscript without writing ~1GB blobs.

use bitvm::chunk::api::{
    api_generate_partial_script, type_conversion_utils::RawProof, NUM_TAPS,
};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

fn main() {
    let mut vendor = PathBuf::from("../..").join("vendor/BitVM");
    let mut out = PathBuf::from("../..")
        .join("crates/fd-pl-bitvm2/data/bitvm_scripts_cache.json");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--vendor" => vendor = PathBuf::from(args.next().expect("--vendor path")),
            "--out" => out = PathBuf::from(args.next().expect("--out path")),
            "-h" | "--help" => {
                eprintln!(
                    "Usage: falcon-pl-bitvm2-c1b-export [--vendor DIR] [--out FILE]\n\
                     Builds BitVM partial scripts for a mock VK and writes per-tap digests."
                );
                return;
            }
            other => panic!("unknown arg {other}"),
        }
    }
    let _ = vendor; // path used by cargo path dep; runtime check optional
    eprintln!("generating BitVM partial scripts (NUM_TAPS={NUM_TAPS})…");
    let raw = RawProof::default();
    let scripts = api_generate_partial_script(&raw.vk);
    assert_eq!(scripts.len(), NUM_TAPS);

    let mut digests = Vec::with_capacity(scripts.len());
    let mut root = Sha256::new();
    root.update(b"fpl-bitvm2-script-digests-v1|");
    for s in &scripts {
        let d = Sha256::digest(s.as_bytes());
        let hex_d = hex::encode(d);
        root.update(hex_d.as_bytes());
        digests.push(hex_d);
    }
    let scripts_root = hex::encode(root.finalize());

    let commit = std::process::Command::new("git")
        .args(["-C", vendor.to_str().unwrap_or("."), "rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    let manifest = serde_json::json!({
        "pack_id": "fpl-bitvm-bn254-c1b-v1",
        "upstream": "https://github.com/BitVM/BitVM",
        "upstream_commit": commit,
        "api_path": "bitvm/src/chunk/api.rs",
        "num_pubs": 1,
        "num_u256": 14,
        "num_hash": 363,
        "num_taps": NUM_TAPS,
        "entrypoints": {
            "generate_partial_scripts": "api_generate_partial_script",
            "generate_full_tapscripts": "api_generate_full_tapscripts",
            "validate_assertions": "validate_assertions"
        },
        "scripts_root_hex": scripts_root,
        "notes": "Per-tap SHA256 digests of api_generate_partial_script(mock VK)."
    });

    let cache = serde_json::json!({
        "manifest": manifest,
        "tap_script_digests_hex": digests,
    });

    if let Some(parent) = out.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&out, serde_json::to_string_pretty(&cache).unwrap()).unwrap();
    eprintln!("wrote {} digests → {}", digests.len(), out.display());
}
