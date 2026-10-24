//! falcon-dag-wallet — client-side keygen / sign / verify for user payments.
//!
//! Does **not** need a running node. Use with `falcon-dagd --no-auto-sign`
//! (or `--auto-sign false`) so the node only verifies.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use fd_crypto::{
    verify_tx_embedded, Falcon512Wallet, HmacWallet, WalletBackend,
};
use fd_types::Transaction;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "falcon-dag-wallet", about = "Falcon DAG user wallet CLI")]
struct Args {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Generate Falcon-512 faucet wallets (acct1..N)
    GenKeys {
        #[arg(long)]
        dir: PathBuf,
        #[arg(long, default_value_t = 20)]
        accounts: u32,
    },
    /// Sign a payment (HMAC-dev or Falcon from --wallet-dir)
    Sign {
        #[arg(long, default_value = "hmac-sha256-dev")]
        scheme: String,
        #[arg(long, default_value = "")]
        wallet_dir: String,
        #[arg(long, default_value_t = 9002)]
        network_id: u32,
        #[arg(long)]
        account: String,
        #[arg(long)]
        sequence: u64,
        #[arg(long, default_value = "acct2")]
        destination: String,
        #[arg(long, default_value_t = 1)]
        amount: u64,
        #[arg(long, default_value_t = 10)]
        fee: u64,
        /// Write signed tx JSON to this path (default: stdout)
        #[arg(long, default_value = "")]
        out: String,
    },
    /// Verify a signed tx JSON file
    Verify {
        #[arg(long)]
        file: PathBuf,
        #[arg(long, default_value_t = 9002)]
        network_id: u32,
    },
    /// Print RPC submit body for a signed tx JSON file
    SubmitBody {
        #[arg(long)]
        file: PathBuf,
    },
}

fn main() -> Result<()> {
    let args = Args::parse();
    match args.cmd {
        Cmd::GenKeys { dir, accounts } => {
            let w = Falcon512Wallet::faucet(accounts);
            w.save_dir(&dir)?;
            println!(
                "wrote Falcon-512 wallet keys for {accounts} accounts to {}",
                dir.display()
            );
        }
        Cmd::Sign {
            scheme,
            wallet_dir,
            network_id,
            account,
            sequence,
            destination,
            amount,
            fee,
            out,
        } => {
            let mut tx = Transaction::create(
                account,
                sequence,
                destination,
                amount,
                fee,
                0.0,
            );
            match scheme.as_str() {
                "hmac-sha256-dev" | "hmac" | "dev" => {
                    HmacWallet::new().sign_tx(&mut tx, network_id)?;
                }
                "falcon-512" | "falcon" | "falcon512" => {
                    if wallet_dir.is_empty() {
                        bail!("falcon-512 sign requires --wallet-dir");
                    }
                    let w = Falcon512Wallet::load_dir(&wallet_dir)
                        .context("load wallet")?;
                    w.sign_tx(&mut tx, network_id)?;
                }
                other => bail!("unknown scheme: {other}"),
            }
            let json = serde_json::to_string_pretty(&tx)?;
            if out.is_empty() {
                println!("{json}");
            } else {
                std::fs::write(&out, json + "\n")?;
                println!("wrote signed tx {} to {out}", tx.tx_id);
            }
        }
        Cmd::Verify { file, network_id } => {
            let text = std::fs::read_to_string(&file)?;
            let tx: Transaction = serde_json::from_str(&text)?;
            let ok = verify_tx_embedded(&tx, network_id);
            if ok {
                println!("OK tx_id={} account={} scheme_pk={}", tx.tx_id, tx.account, tx.public_key.as_deref().unwrap_or("?"));
            } else {
                bail!("VERIFY FAILED for {}", tx.tx_id);
            }
        }
        Cmd::SubmitBody { file } => {
            let text = std::fs::read_to_string(&file)?;
            let tx: Transaction = serde_json::from_str(&text)?;
            let body = serde_json::json!({
                "method": "submit",
                "params": [{
                    "account": tx.account,
                    "sequence": tx.sequence,
                    "destination": tx.destination,
                    "amount": tx.amount,
                    "fee": tx.fee,
                    "signature": tx.signature,
                    "public_key": tx.public_key,
                }]
            });
            println!("{}", serde_json::to_string(&body)?);
        }
    }
    Ok(())
}
