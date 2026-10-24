//! Prove `FplWithdrawValid` for operator sidecar (reads leaves from FPL status).

use clap::Parser;
use fd_pl::bitvm::dest_h160_from_address;
use fd_pl_bitvm2::withdraw_ops::{header_hash_placeholder, leaf_index, mimc_leaf_hex, prove_withdraw_attest};
use serde::Serialize;

#[derive(Parser)]
#[command(name = "falcon-pl-bitvm2-prove")]
struct Args {
    #[arg(long)]
    note_id: String,
    #[arg(long)]
    dest: String,
    #[arg(long)]
    amount: u64,
    #[arg(long)]
    pegout_txid: String,
    #[arg(long)]
    height: u64,
    #[arg(long, default_value = "")]
    tip_hash: String,
    #[arg(long)]
    leaves_json: String,
    #[arg(long, default_value = "operator")]
    operator: String,
}

#[derive(Serialize)]
struct Out {
    proof_hex: String,
    public_hex: Vec<String>,
    statement_digest_hex: String,
    claim_id: String,
}

fn main() -> Result<(), String> {
    let args = Args::parse();
    let leaves: Vec<String> = serde_json::from_str(&args.leaves_json).map_err(|e| e.to_string())?;
    let dest_h160 = dest_h160_from_address(&args.dest)?;
    let pegout = fd_pl_bitvm2::withdraw_ops::pegout_txid_bytes(&args.pegout_txid)?;
    let header = header_hash_placeholder(args.height, &args.tip_hash);
    let _ = leaf_index(&args.note_id, &dest_h160, args.amount, &leaves)?;
    let _ = mimc_leaf_hex(&args.note_id, &dest_h160, args.amount);
    let (proof_hex, public_hex, statement_digest_hex, claim_id) = prove_withdraw_attest(
        &args.note_id,
        &dest_h160,
        args.amount,
        args.height,
        &header,
        &pegout,
        &leaves,
        &args.operator,
    )?;
    println!(
        "{}",
        serde_json::to_string(&Out {
            proof_hex,
            public_hex,
            statement_digest_hex,
            claim_id,
        })
        .map_err(|e| e.to_string())?
    );
    Ok(())
}
