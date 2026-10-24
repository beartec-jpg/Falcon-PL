//! Control client for falcon-pl-node (Falcon PL / FPL).
//!
//!   falcon-pl-ctl gen-keys / gen-wallet --keys-dir ...
//!   falcon-pl-ctl status / account / faucet / pay / claim / heartbeat
//!   falcon-pl-ctl vault-open / vault-lock / create-pool / …
//!   falcon-pl-ctl bond / unbond / complete-unbond   # signed TxBody + SubmitTx
//!   falcon-pl-ctl --admin-sock PATH tick / force-settle / hard-reset / snapshot-*
//!   falcon-pl-ctl --admin-sock PATH rail-header / rail-deposit / rail-withdraw
//!   falcon-pl-ctl submit-eth-v3-deposit / qc-leaves

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use fd_pl::bridge::{test_deposit_proof, BridgeProof};
use fd_pl::crypto::{CryptoScheme, KeyRing};
use fd_pl::{Tx, TxBody, WireMsg, FAUCET_ACCOUNT, NETWORK_ID_PRIVATE_V22};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpStream, UnixStream};

#[derive(Parser, Debug)]
#[command(version = fd_pl::PRODUCT_VERSION)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:19101")]
    addr: String,
    /// Unix operator socket (`--admin-sock` on the node). Required for tick /
    /// hard-reset / snapshot / rails once the TCP wire is locked.
    #[arg(long, default_value = "")]
    admin_sock: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    GenKeys {
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, value_delimiter = ',', default_value = "v1,v2,v3")]
        validators: Vec<String>,
        #[arg(long, default_value_t = 32)]
        load_wallets: u32,
    },
    /// Node status (slim by default — no accounts dump).
    Status {
        /// Include full accounts map (large; only for debugging / seq dumps).
        #[arg(long, default_value_t = false)]
        accounts: bool,
    },
    /// Read-only health snapshot (`falcon.health.v1`) as JSON.
    Health {
        /// Recent log warnings/errors to include (1-64, default 20).
        #[arg(long, default_value_t = 20)]
        warnings: u32,
    },
    /// htop-style auto-refreshing health view (q quits). Falls back to
    /// `status` on nodes without the health endpoint.
    Dash {
        /// Refresh interval in seconds.
        #[arg(long, default_value_t = 2.0)]
        interval: f64,
        /// Print one frame and exit (no raw mode; for scripts / non-TTY).
        #[arg(long, default_value_t = false)]
        once: bool,
        /// Disable ANSI colour (also honours NO_COLOR).
        #[arg(long, default_value_t = false)]
        no_color: bool,
        /// Light theme: no painted background, darker text (or FALCON_DASH_THEME=light).
        /// Default paints a dark background (256-colour 234) on every cell.
        #[arg(long, default_value_t = false)]
        light: bool,
        /// Keys dir holding this node's SEAT key: enables the `c` (claim seat
        /// rewards) action. Keys are only used to sign, never shown.
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value = "falcon-512")]
        scheme: String,
        #[arg(long, default_value_t = fd_pl::NETWORK_ID_PUBLIC_TESTNET_2300)]
        network_id: u32,
    },
    /// Local browser dashboard: serves `/` (embedded page), `/health/v1`
    /// (health_req body, unchanged) and `/history/v1` (in-memory samples).
    /// Loopback only unless --allow-remote.
    Web {
        #[arg(long, default_value = "127.0.0.1:19480")]
        listen: String,
        /// Allow a non-loopback bind (Tailscale IP, 0.0.0.0). No auth: only on a trusted network.
        #[arg(long, default_value_t = false)]
        allow_remote: bool,
        /// Seconds between history samples (24 h of samples are kept at 10 s).
        #[arg(long, default_value_t = fd_pl::health_web::SAMPLE_SECS)]
        sample_secs: u64,
    },
    /// Operator balances in one line: liquid FPL, bonded, pending rewards,
    /// unbonding (read-only; default account = this node's id; no keys read).
    Balance {
        /// Account / seat id (default: the node's own id).
        #[arg(long)]
        account: Option<String>,
        /// Print the balance block as JSON.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Query one account: sequence, balance, claimable, assets.
    Account {
        #[arg(long)]
        account: String,
    },
    /// Create/load an extra wallet key into keys-dir (staging).
    GenWallet {
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long)]
        account: String,
        #[arg(long, default_value = "hmac")]
        scheme: String,
    },
    /// Drip from genesis `faucet` account (auto sequence when --sequence 0).
    Faucet {
        #[arg(long)]
        to: String,
        #[arg(long, default_value_t = 1_000)]
        amount: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        /// 0 = fetch next sequence from node.
        #[arg(long, default_value_t = 0)]
        sequence: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
        /// v2.9.65 (C-1): sha256 hex of the recipient's raw Falcon public key.
        /// Sends PayToKey so only that key can bind the account (needs the
        /// key-bind height). Empty = plain Pay.
        #[arg(long, default_value = "")]
        key_hash: String,
    },
    Seal {
        /// Immediate pack+commit via admin sock (solo LC recovery).
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    Pay {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        #[arg(long, default_value_t = 10)]
        amount: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        /// 0 = fetch next sequence from node account query.
        #[arg(long, default_value_t = 0)]
        sequence: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// Send bridged BTC, ETH, or USDC to another Falcon account.
    PayAsset {
        #[arg(long)]
        from: String,
        #[arg(long)]
        to: String,
        /// BTC, ETH, or USDC (ledger rail name, not the FBTC label).
        #[arg(long)]
        asset: String,
        #[arg(long)]
        amount: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value_t = 0)]
        sequence: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    VaultOpen {
        #[arg(long)]
        account: String,
        #[arg(long)]
        destination: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    VaultLock {
        #[arg(long)]
        account: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    Claim {
        #[arg(long)]
        account: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    CreatePool {
        #[arg(long)]
        pool_id: String,
        #[arg(long, default_value = "FPL")]
        asset_a: String,
        #[arg(long)]
        asset_b: String,
        #[arg(long, default_value = "alice")]
        account: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    AddLiq {
        #[arg(long)]
        pool_id: String,
        #[arg(long)]
        provider: String,
        #[arg(long)]
        amt_a: u64,
        #[arg(long)]
        amt_b: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    Swap {
        #[arg(long)]
        pool_id: String,
        #[arg(long)]
        who: String,
        #[arg(long)]
        token_in: String,
        #[arg(long)]
        amount_in: u64,
        #[arg(long, default_value_t = 1)]
        min_out: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    CreateMarket {
        #[arg(long)]
        market_id: String,
        #[arg(long)]
        asset: String,
        #[arg(long, default_value = "alice")]
        account: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    LendSupply {
        #[arg(long)]
        market_id: String,
        #[arg(long)]
        who: String,
        #[arg(long)]
        amount: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    LendWithdraw {
        #[arg(long)]
        market_id: String,
        #[arg(long)]
        who: String,
        #[arg(long)]
        shares: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    LendBorrow {
        #[arg(long)]
        market_id: String,
        #[arg(long)]
        who: String,
        #[arg(long)]
        amount: u64,
        #[arg(long)]
        collateral_fpl: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    LendSetPrice {
        #[arg(long)]
        market_id: String,
        #[arg(long)]
        who: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    LendRepay {
        #[arg(long)]
        market_id: String,
        #[arg(long)]
        who: String,
        #[arg(long)]
        amount: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    RemoveLiq {
        #[arg(long)]
        pool_id: String,
        #[arg(long)]
        provider: String,
        #[arg(long)]
        lp_burn: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    SwapRoute {
        #[arg(long)]
        who: String,
        #[arg(long)]
        token_in: String,
        #[arg(long)]
        token_out: String,
        #[arg(long)]
        amount_in: u64,
        #[arg(long, default_value_t = 1)]
        min_out: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// Mint secondary asset for private LP soak (USD etc.) — ledger tx.
    CreditAsset {
        #[arg(long)]
        account: String,
        #[arg(long)]
        asset: String,
        #[arg(long)]
        amount: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    Bond {
        #[arg(long)]
        id: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        amount: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// v2.9.62: bond a NEW seat (from H_act). Signs the proof of possession
    /// with the validator key from `--validator-keys` (never printed).
    BondV2 {
        #[arg(long)]
        id: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        amount: u64,
        /// Keys dir holding `<id>.json` (validator secret) + `pl_keys.json`.
        /// Or pass `--pop <file>` (output of `pop` run on the validator host).
        #[arg(long, default_value = "")]
        validator_keys: String,
        /// JSON from `falcon-pl-ctl pop` (public key + PoP signature).
        #[arg(long, default_value = "")]
        pop: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "falcon-512")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// v2.9.62: print the proof of possession for a new seat as JSON
    /// (`id`, `bond_account`, `network_id`, `public_key`, `pop_sig`). Run it on
    /// the validator host; the secret never leaves `--keys-dir`.
    Pop {
        #[arg(long)]
        id: String,
        /// Account that will sign the BondV2 (bound into the PoP message).
        #[arg(long)]
        bond_account: String,
        /// Keys dir holding `<id>.json`.
        #[arg(long)]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// v2.9.62: admission key approves (id, public key) until a height.
    ApproveValidator {
        #[arg(long)]
        id: String,
        /// Validator public key hex, or `@file` with the hex.
        #[arg(long)]
        pubkey: String,
        /// Absolute expiry height, or `+N` = tip + N.
        #[arg(long)]
        expires: String,
        /// Admission account id in `--keys-dir`.
        #[arg(long, default_value = "admission")]
        from: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "falcon-512")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// v2.9.62: admission key withdraws an unused approval.
    RevokeApproval {
        #[arg(long)]
        id: String,
        #[arg(long, default_value = "admission")]
        from: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "falcon-512")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// v2.9.62: owner clears `inactive` on a seat.
    Reactivate {
        #[arg(long)]
        id: String,
        /// Signer (owner); defaults to the validator id.
        #[arg(long, default_value = "")]
        from: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "falcon-512")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// v2.9.62: owner sets/clears the archive flag (`--on true|false`).
    /// Seat payout: move this seat's validator reward bucket to `--to`'s
    /// liquid balance. Signed with the SEAT key (`--keys-dir` holding the
    /// seat's key file, e.g. the node keys dir), never the wallet key.
    ClaimSeat {
        #[arg(long)]
        validator: String,
        #[arg(long)]
        to: String,
        /// 0 = suggested fee. Paid from the seat bucket.
        #[arg(long, default_value_t = 0)]
        fee: u64,
        #[arg(long, default_value = "falcon-512")]
        scheme: String,
        #[arg(long)]
        keys_dir: String,
        #[arg(long, default_value_t = fd_pl::NETWORK_ID_PUBLIC_TESTNET_2300)]
        network_id: u32,
    },
    /// Seat payout: set (or `--clear`) the account credited with this seat's
    /// validator rewards at every epoch settle. Signed with the SEAT key.
    SetSeatPayout {
        #[arg(long)]
        validator: String,
        #[arg(long, default_value = "")]
        payout_account: String,
        #[arg(long)]
        clear: bool,
        #[arg(long, default_value_t = 0)]
        fee: u64,
        #[arg(long, default_value = "falcon-512")]
        scheme: String,
        #[arg(long)]
        keys_dir: String,
        #[arg(long, default_value_t = fd_pl::NETWORK_ID_PUBLIC_TESTNET_2300)]
        network_id: u32,
    },
    SetArchive {
        #[arg(long)]
        id: String,
        #[arg(long)]
        on: bool,
        #[arg(long, default_value = "")]
        from: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "falcon-512")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// v2.9.62: admission key clears a seat's archive flag.
    RevokeArchive {
        #[arg(long)]
        id: String,
        #[arg(long, default_value = "admission")]
        from: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "falcon-512")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    Unbond {
        #[arg(long)]
        id: String,
        #[arg(long)]
        amount: u64,
        /// Signer (defaults to validator id).
        #[arg(long, default_value = "")]
        from: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    CompleteUnbond {
        #[arg(long)]
        id: String,
        #[arg(long, default_value = "")]
        from: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// Clear jail by posting the escalated rebond (min_bond × 2^jail_count).
    Unjail {
        #[arg(long)]
        id: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        amount: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    WatcherCredit {
        #[arg(long)]
        account: String,
        #[arg(long, default_value_t = 1)]
        work: u64,
        /// Mark current wall-clock hour slot (default on).
        #[arg(long, default_value_t = true)]
        slot: bool,
        /// Bulk-mark presence slots 0..N-1 (max 168) without waiting wall time.
        #[arg(long, default_value_t = 0)]
        presence_slots: u64,
        /// Shorthand: full-epoch presence (168 slots).
        #[arg(long, default_value_t = false)]
        full_presence: bool,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// Browser/VPS presence tick: fee + current slot, zero work.
    Heartbeat {
        #[arg(long)]
        account: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// BRIDGE operator tx (refused by the ledger before the bridge height).
    /// `--body` is the TxBody JSON, e.g. `{"kind":"operator_heartbeat","capacity_sats":120000}`,
    /// `{"kind":"operator_register","btc_xonly":"<64hex>","reward_spk_hex":"5120..","capacity_sats":0}`
    /// with `--amount 5000` (bond), `{"kind":"operator_unbond"}`,
    /// `{"kind":"operator_graph_bind",...}`, `{"kind":"operator_slash",...}`.
    Operator {
        #[arg(long)]
        account: String,
        #[arg(long)]
        body: String,
        /// FPL amount (bond for operator_register; 0 otherwise).
        #[arg(long, default_value_t = 0)]
        amount: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// Signed rail headers from a watcher account (real work, not WatcherCredit).
    /// Each accepted header is +1 work and marks the current presence slot.
    WatcherWork {
        #[arg(long)]
        account: String,
        #[arg(long, default_value = "ETH")]
        asset: String,
        /// How many sequential headers to submit (168 + 1 slot ⇒ weight ≥ 1).
        #[arg(long, default_value_t = 168)]
        count: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    Tick {
        #[arg(long, default_value_t = 1000)]
        dt_ms: u64,
    },
    ForceSettle,
    /// Operator-only: arm hard-resync from genesis (v2.3.2 P2). Does not auto-run on forks.
    HardReset {
        #[arg(long, default_value = "operator")]
        reason: String,
    },
    /// Submit SPV rail header (BTC/ETH/BNB). Admin wire — one node only.
    RailHeader {
        #[arg(long, default_value = "ETH")]
        asset: String,
        #[arg(long)]
        height: u64,
        #[arg(long)]
        hash: String,
        #[arg(long, default_value = "")]
        parent_hash: String,
        #[arg(long)]
        merkle_root: String,
        #[arg(long, default_value = "")]
        watcher: String,
        #[arg(long, default_value = "")]
        raw: String,
    },
    /// Signed Bitcoin header onto the public mesh (not admin wire, not val keys).
    SubmitBtcHeader {
        #[arg(long)]
        account: String,
        /// BTC (legacy testnet3 rail) or BTC4 (testnet4 bridge rail).
        #[arg(long, default_value = "BTC")]
        asset: String,
        /// 80-byte Bitcoin header hex.
        #[arg(long)]
        raw: String,
        /// Bitcoin block height (checkpoint / next).
        #[arg(long)]
        height: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
        /// 0 = fetch next committed sequence. Burst catch-up passes N, N+1, …
        #[arg(long, default_value_t = 0)]
        sequence: u64,
    },
    /// Insert pruned Bitcoin headers below the contiguous tip window.
    SubmitBtcBackfill {
        #[arg(long)]
        account: String,
        /// JSON array of `{height, raw}` (80-byte header hex). Must end at floor-1.
        #[arg(long)]
        file: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
        #[arg(long, default_value_t = 0)]
        sequence: u64,
    },
    /// Signed ETH/USDC rail header (watcher merkle of dest-lock Deposit logs).
    SubmitEvmHeader {
        #[arg(long)]
        account: String,
        #[arg(long, default_value = "ETH")]
        asset: String,
        #[arg(long)]
        height: u64,
        #[arg(long)]
        hash: String,
        #[arg(long, default_value = "")]
        parent_hash: String,
        #[arg(long)]
        merkle_root: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// SPV deposit mint (test fixture builds single-leaf proof if --test-fixture).
    RailDeposit {
        #[arg(long, default_value = "BTC")]
        asset: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        amount: u64,
        #[arg(long, default_value = "")]
        external_txid: String,
        #[arg(long, default_value = "")]
        block_hash: String,
        #[arg(long, default_value_t = 0)]
        height: u64,
        #[arg(long, default_value = "")]
        lock_id: String,
        #[arg(long, default_value = "")]
        merkle_root: String,
        #[arg(long, default_value = "")]
        watcher: String,
        /// Build a valid single-leaf test proof from args (private fleet).
        #[arg(long, default_value_t = false)]
        test_fixture: bool,
        #[arg(long, default_value = "")]
        raw_tx: String,
        #[arg(long, default_value = "")]
        merkle_path: String,
        #[arg(long, default_value_t = 0)]
        merkle_index: u32,
        #[arg(long, default_value = "")]
        token: String,
    },
    RailWithdraw {
        #[arg(long, default_value = "BTC")]
        asset: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        amount: u64,
        #[arg(long)]
        external_to: String,
        #[arg(long, default_value = "")]
        vault_sig: String,
        #[arg(long, default_value = "")]
        signed_btc_tx: String,
    },
    /// Operator admin: BitVM2 pegout + Claim attestation (admin sock; no SubmitTx).
    RailBitvm2Attest {
        #[arg(long)]
        note_id: String,
        #[arg(long, default_value = "v1")]
        operator: String,
        #[arg(long)]
        pegout_txid_hex: String,
        #[arg(long)]
        claim_txid_hex: String,
        #[arg(long)]
        proof_hex: String,
        #[arg(long)]
        statement_digest_hex: String,
        /// Comma-separated 13 public field elements (32-byte hex each).
        #[arg(long)]
        public_hex: String,
    },
    /// Operator admin: record Bitcoin peg-out txid for an open BitVM2 claim.
    RailBitvm2Pegout {
        #[arg(long)]
        note_id: String,
        #[arg(long, default_value = "v1")]
        operator: String,
        #[arg(long)]
        pegout_txid_hex: String,
    },
    /// Build a signed P2PKH/P2WPKH deposit: prevout → vault + FALC dest.
    BtcDepositTx {
        #[arg(long)]
        privhex_file: PathBuf,
        #[arg(long)]
        prev_txid: String,
        #[arg(long, default_value_t = 0)]
        vout: u32,
        #[arg(long)]
        sats: u64,
        #[arg(long)]
        dest_account: String,
        #[arg(long)]
        vault_spk: String,
        #[arg(long, default_value = "")]
        prev_spk: String,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
    },
    /// 1:1 user vault: print P2WSH address (user key only; take_commit from seed).
    BtcUserVaultAddress {
        #[arg(long)]
        privhex_file: PathBuf,
        #[arg(long)]
        account: String,
        #[arg(long)]
        seed_file: PathBuf,
        #[arg(long, default_value = "")]
        nonce_hex: String,
    },
    /// Deposit into the user vault + 76-byte FALC memo.
    BtcUserVaultDeposit {
        #[arg(long)]
        privhex_file: PathBuf,
        #[arg(long)]
        prev_txid: String,
        #[arg(long, default_value_t = 0)]
        vout: u32,
        #[arg(long)]
        sats: u64,
        #[arg(long)]
        account: String,
        #[arg(long)]
        seed_file: PathBuf,
        #[arg(long)]
        prev_spk: String,
        #[arg(long, default_value = "")]
        nonce_hex: String,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
    },
    /// Take BTC after burn (preimage from status published_takes + user sig).
    BtcUserVaultTake {
        #[arg(long)]
        privhex_file: PathBuf,
        #[arg(long)]
        utxo: String,
        #[arg(long)]
        account: String,
        #[arg(long)]
        seed_file: PathBuf,
        #[arg(long)]
        dest: String,
        #[arg(long, default_value = "")]
        nonce_hex: String,
        #[arg(long, default_value = "")]
        preimage_hex: String,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
    },
    /// CSV refund (user key only).
    BtcUserVaultRefund {
        #[arg(long)]
        privhex_file: PathBuf,
        #[arg(long)]
        utxo: String,
        #[arg(long)]
        account: String,
        #[arg(long)]
        seed_file: PathBuf,
        #[arg(long)]
        dest: String,
        #[arg(long, default_value = "")]
        nonce_hex: String,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
    },
    /// Pooled transferable-FBTC rail: generate abort key + print pool address.
    BtcFbtcPoolCeremony {
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long, default_value_t = fd_pl::fbtc_pool::POOL_CSV_BLOCKS)]
        csv: u32,
    },
    /// Print pool address from keys-dir/pool.json or hex params.
    BtcFbtcPoolAddress {
        #[arg(long, default_value = "")]
        keys_dir: PathBuf,
        #[arg(long, default_value = "")]
        abort_commit_hex: String,
        #[arg(long, default_value = "")]
        abort_xonly: String,
        #[arg(long, default_value_t = fd_pl::fbtc_pool::POOL_CSV_BLOCKS)]
        csv: u32,
    },
    /// Dave (anyone) deposits BTC into the pool + FALC memo.
    BtcFbtcPoolDeposit {
        #[arg(long)]
        privhex_file: PathBuf,
        #[arg(long)]
        prev_txid: String,
        #[arg(long, default_value_t = 0)]
        vout: u32,
        #[arg(long)]
        sats: u64,
        #[arg(long)]
        account: String,
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long, default_value = "")]
        prev_spk: String,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
    },
    /// Sally signs peg-out: pool → dest + change-to-pool. No watch / n-of-n / front.
    BtcFbtcPoolPegout {
        #[arg(long)]
        privhex_file: PathBuf,
        #[arg(long)]
        utxo: String,
        #[arg(long)]
        dest: String,
        #[arg(long)]
        amount: u64,
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
    },
    /// Presign abort (return-to-pool only) for the current pool outpoint.
    BtcFbtcPoolPresignAbort {
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long)]
        utxo: String,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Broadcastable abort: presigned return-to-pool + published preimage.
    BtcFbtcPoolAbort {
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long)]
        presign_json: Option<PathBuf>,
        #[arg(long, default_value = "")]
        preimage_hex: String,
    },
    /// Build a signed P2PKH/P2WPKH pay: prevout → dest + change.
    BtcPayTx {
        #[arg(long)]
        privhex_file: PathBuf,
        #[arg(long)]
        prev_txid: String,
        #[arg(long, default_value_t = 0)]
        vout: u32,
        #[arg(long)]
        sats: u64,
        #[arg(long)]
        dest: String,
        #[arg(long)]
        amount: u64,
        #[arg(long, default_value = "")]
        prev_spk: String,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
    },
    /// Build a signed BitVM2 Claim marker: OP_RETURN FPL2||digest + change.
    BtcClaimTx {
        #[arg(long)]
        privhex_file: PathBuf,
        #[arg(long)]
        prev_txid: String,
        #[arg(long, default_value_t = 0)]
        vout: u32,
        #[arg(long)]
        sats: u64,
        #[arg(long)]
        statement_digest_hex: String,
        #[arg(long, default_value = "")]
        prev_spk: String,
        #[arg(long, default_value_t = 500)]
        fee: u64,
    },
    /// Combine t vault shares and emit a BIP143 signed vault spend.
    BtcSignWithdraw {
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long, value_delimiter = ',', default_value = "v1,v2,v3")]
        seats: Vec<String>,
        #[arg(long)]
        dest: String,
        #[arg(long)]
        amount: u64,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
        /// utxo as txid:vout:sats (repeatable)
        #[arg(long)]
        utxo: Vec<String>,
        #[arg(long)]
        vault_spk: String,
    },
    /// Signed RailDeposit (public mesh — not admin wire).
    SubmitRailDeposit {
        #[arg(long)]
        account: String,
        #[arg(long, default_value = "BTC")]
        asset: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        amount: u64,
        #[arg(long)]
        external_txid: String,
        #[arg(long)]
        block_hash: String,
        #[arg(long)]
        height: u64,
        #[arg(long, default_value = "")]
        merkle_root: String,
        #[arg(long, default_value = "")]
        raw_tx: String,
        #[arg(long, default_value = "")]
        raw_tx_file: String,
        #[arg(long, default_value = "")]
        merkle_path: String,
        #[arg(long, default_value = "")]
        merkle_path_file: String,
        #[arg(long, default_value_t = 0)]
        merkle_index: u32,
        #[arg(long, default_value = "")]
        token: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// Signed EthV3Deposit (permissionless — any funded account, same as
    /// submit-rail-deposit). `credit_to` empty asks for a refund after expiry.
    /// Receipt RLP is `--raw` / `--raw-tx` (or the `-file` form). The receipt
    /// MPT nodes are `--path` / `--merkle-path`.
    SubmitEthV3Deposit {
        #[arg(long)]
        account: String,
        #[arg(long, default_value = "ETH")]
        asset: String,
        /// 32-byte Deposit topic1, with or without 0x.
        #[arg(long)]
        deposit_id: String,
        /// FPL name preimage of the log dest20. Omit or pass empty to refund.
        #[arg(long, default_value = "")]
        credit_to: String,
        #[arg(long, default_value = "")]
        external_txid: String,
        #[arg(long)]
        block_hash: String,
        #[arg(long)]
        height: u64,
        #[arg(long, alias = "raw", default_value = "")]
        raw_tx: String,
        #[arg(long, alias = "raw-file", default_value = "")]
        raw_tx_file: String,
        #[arg(long, alias = "path", default_value = "")]
        merkle_path: String,
        #[arg(long, alias = "path-file", default_value = "")]
        merkle_path_file: String,
        #[arg(long, default_value_t = 0)]
        merkle_index: u32,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// MiMC pk leaves for `*.pub.json` in a keys dir (`id` + 32-byte hex).
    /// The V3 prover matches these to `FalconQcBridgeV3.currentLeaves`.
    QcLeaves {
        #[arg(long)]
        keys_dir: PathBuf,
    },
    /// Signed RailWithdraw including optional BIP143 vault tx.
    SubmitRailWithdraw {
        #[arg(long)]
        account: String,
        #[arg(long, default_value = "BTC")]
        asset: String,
        #[arg(long)]
        amount: u64,
        #[arg(long)]
        external_to: String,
        #[arg(long, default_value = "")]
        vault_sig: String,
        #[arg(long, default_value = "")]
        signed_btc_tx: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// Sepolia ETH LC bootstrap JSON (once).
    SubmitEthLcBootstrap {
        #[arg(long)]
        account: String,
        #[arg(long)]
        json_file: PathBuf,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// Sepolia ETH LC update JSON.
    SubmitEthLcUpdate {
        #[arg(long)]
        account: String,
        #[arg(long)]
        json_file: PathBuf,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// Execution headers JSON (deposit block → LC head).
    SubmitEthExecHeaders {
        #[arg(long)]
        account: String,
        #[arg(long)]
        json_file: PathBuf,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
        /// 0 = next committed sequence. A fill wave passes N, N+1, … so one seal
        /// can apply the whole run (max 128 txs or the 1s pack budget).
        #[arg(long, default_value_t = 0)]
        sequence: u64,
    },
    /// Several execution-header batches signed N, N+1, … and delivered together.
    /// JSON file is an array of header arrays. One pack can apply the whole run.
    SubmitEthExecWave {
        #[arg(long)]
        account: String,
        #[arg(long)]
        json_file: PathBuf,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
        #[arg(long, default_value_t = 0)]
        sequence: u64,
    },
    /// Packed last FROST spend: pool UTXO → NUMS P2TR. Does not burn FBTC.
    SubmitBtcPoolMove {
        #[arg(long)]
        account: String,
        #[arg(long)]
        raw_tx: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// Permissionless BitVM challenge (signed SubmitTx). Honest dest/amount is rejected.
    SubmitRailChallenge {
        #[arg(long)]
        account: String,
        #[arg(long, default_value = "BTC")]
        asset: String,
        #[arg(long)]
        note_id: String,
        #[arg(long)]
        observed_dest: String,
        #[arg(long)]
        observed_amount: u64,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    /// Operator: attest BitVM2 pegout + Claim on Bitcoin testnet.
    SubmitBitvm2Attest {
        #[arg(long)]
        account: String,
        #[arg(long)]
        note_id: String,
        #[arg(long)]
        pegout_txid_hex: String,
        #[arg(long)]
        claim_txid_hex: String,
        #[arg(long)]
        proof_hex: String,
        #[arg(long)]
        statement_digest_hex: String,
        /// Comma-separated 13 public field elements (32-byte hex each).
        #[arg(long)]
        public_hex: String,
        #[arg(long, default_value_t = 2)]
        fee: u64,
        #[arg(long, default_value = "hmac")]
        scheme: String,
        #[arg(long, default_value = "")]
        keys_dir: String,
        #[arg(long, default_value_t = NETWORK_ID_PRIVATE_V22)]
        network_id: u32,
    },
    SnapshotSave {
        #[arg(long)]
        path: PathBuf,
    },
    SnapshotLoad {
        #[arg(long)]
        path: PathBuf,
    },
    /// Shamir split onto **this** keys_dir. Custodial if all shares land on one
    /// disk. Product path is `frost-dkg-part1/2/3` on each seat. Only pass
    /// `--legacy-shamir-sweep` to sweep an existing P2WPKH.
    VaultGen {
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long, value_delimiter = ',', default_value = "v1,v2,v3,v4,v5")]
        seats: Vec<String>,
        #[arg(long, default_value_t = false)]
        legacy_shamir_sweep: bool,
    },
    /// Dealerless FROST DKG round 1. Writes this seat's r1 secret (600) + public package.
    FrostDkgPart1 {
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long)]
        seat: String,
        #[arg(long, value_delimiter = ',', default_value = "v1,v2,v3,v4,v5,v6")]
        seats: Vec<String>,
        #[arg(long, default_value_t = 0)]
        t: u16,
    },
    /// DKG round 2. Needs every other seat's `frost-dkg-r1-<seat>` in keys_dir.
    FrostDkgPart2 {
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long)]
        seat: String,
        #[arg(long, value_delimiter = ',', default_value = "v1,v2,v3,v4,v5,v6")]
        seats: Vec<String>,
    },
    /// DKG round 3. Writes `vault-frost-share-<seat>` (600) + `vault-frost-pub.json`.
    FrostDkgPart3 {
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long)]
        seat: String,
        #[arg(long, value_delimiter = ',', default_value = "v1,v2,v3,v4,v5,v6")]
        seats: Vec<String>,
    },
    /// Write `keys/bitvm-abort-seed` (32 random bytes, mode 600). Same seed on every seat.
    BitvmAbortSeed {
        #[arg(long)]
        keys_dir: PathBuf,
    },
    /// Kickoff FROST round 1: nonce (600) + public commitment. Key never leaves the seat.
    FrostSignRound1 {
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long)]
        seat: String,
    },
    /// Kickoff FROST round 2. `--signing-package` is JSON from the coordinator.
    FrostSignRound2 {
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long)]
        seat: String,
        #[arg(long, value_delimiter = ',', default_value = "v1,v2,v3,v4,v5,v6")]
        seats: Vec<String>,
        #[arg(long)]
        signing_package: PathBuf,
    },
    /// Aggregate ≥ t signature shares + public package. Coordinator holds no share file.
    FrostAggregate {
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long, value_delimiter = ',')]
        signers: Vec<String>,
        #[arg(long, value_delimiter = ',', default_value = "v1,v2,v3,v4,v5,v6")]
        seats: Vec<String>,
        #[arg(long)]
        signing_package: PathBuf,
        #[arg(long)]
        message_hex: String,
    },
    /// Coordinator: write SigningPackage JSON from public commitments + sighash.
    FrostMakePackage {
        #[arg(long)]
        keys_dir: PathBuf,
        #[arg(long, value_delimiter = ',')]
        signers: Vec<String>,
        #[arg(long, value_delimiter = ',', default_value = "v1,v2,v3,v4,v5,v6")]
        seats: Vec<String>,
        #[arg(long)]
        message_hex: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Unsigned FROST P2TR key-path pay (recovery). Prints sighash hex.
    BtcFrostKeypathUnsigned {
        #[arg(long)]
        utxo: String,
        #[arg(long)]
        dest: String,
        #[arg(long)]
        amount: u64,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
        #[arg(long)]
        vault_spk: String,
    },
    /// Attach 64-byte aggregated Schnorr to a key-path pay. Prints raw tx hex.
    BtcFrostKeypathFinish {
        #[arg(long)]
        utxo: String,
        #[arg(long)]
        dest: String,
        #[arg(long)]
        amount: u64,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
        #[arg(long)]
        vault_spk: String,
        #[arg(long)]
        sig_hex: String,
    },
    /// Unsigned dest-lock Kickoff (P2TR key-path). Coordinator holds no share.
    BtcFrostKickoffUnsigned {
        #[arg(long)]
        utxo: String,
        #[arg(long)]
        dest: String,
        #[arg(long)]
        amount: u64,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
        #[arg(long)]
        vault_spk: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        abort_seed_file: PathBuf,
    },
    /// Attach 64-byte aggregated Schnorr to a dest-lock Kickoff. Prints raw tx hex.
    BtcFrostKickoffFinish {
        #[arg(long)]
        utxo: String,
        #[arg(long)]
        dest: String,
        #[arg(long)]
        amount: u64,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
        #[arg(long)]
        vault_spk: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        abort_seed_file: PathBuf,
        #[arg(long)]
        sig_hex: String,
    },
    /// Print the NUMS P2TR pool (script-path Kickoff, no FROST).
    BtcNumsPool,
    /// BitVM2 instance address (NUMS + CSV operator payout + n-of-n Challenge). No watch key.
    BtcBitvm2Instance {
        #[arg(long)]
        operator_xonly: String,
        #[arg(long, value_delimiter = ',', required = true)]
        setup_xonly: Vec<String>,
        #[arg(long, default_value_t = 144)]
        csv: u32,
        #[arg(long, default_value_t = 72)]
        assert_csv: u32,
        #[arg(long)]
        lamport_pk_json: Option<PathBuf>,
    },
    /// Spend the BitVM2 instance via PayoutOptimistic (CSV + operator). No watch key.
    BtcBitvm2Payout {
        #[arg(long)]
        utxo: String,
        #[arg(long)]
        operator_secret_file: PathBuf,
        #[arg(long, value_delimiter = ',', required = true)]
        setup_xonly: Vec<String>,
        #[arg(long)]
        dest: String,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
        #[arg(long, default_value_t = 144)]
        csv: u32,
        #[arg(long, default_value_t = 72)]
        assert_csv: u32,
    },
    /// Assemble a setup n-of-n Challenge tx (pays Assert connector, not an arbitrary dest).
    BtcBitvm2Challenge {
        #[arg(long)]
        utxo: String,
        #[arg(long)]
        operator_xonly: String,
        #[arg(long, value_delimiter = ',', required = true)]
        setup_secret_files: Vec<PathBuf>,
        #[arg(long, default_value = "")]
        digest: String,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
        #[arg(long, default_value_t = 144)]
        csv: u32,
        #[arg(long, default_value_t = 72)]
        assert_csv: u32,
        #[arg(long)]
        lamport_pk_json: Option<PathBuf>,
    },
    /// Instance → dest-lock (user CSV take). Claimer key only. No watch. No n-of-n. No front.
    BtcBitvm2UserKickoff {
        #[arg(long)]
        utxo: String,
        #[arg(long)]
        claimer_secret_file: PathBuf,
        #[arg(long, value_delimiter = ',', required = true)]
        setup_xonly: Vec<String>,
        #[arg(long)]
        dest: String,
        #[arg(long)]
        amount: u64,
        #[arg(long)]
        abort_commit_hex: String,
        #[arg(long, default_value_t = 6)]
        claim_csv: u32,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
        #[arg(long, default_value_t = 144)]
        csv: u32,
        #[arg(long, default_value_t = 72)]
        assert_csv: u32,
    },
    /// Setup-once: presign Challenge→Assert or abort→instance. Optional --wipe secrets.
    BtcBitvm2Presign {
        #[arg(long)]
        utxo: String,
        #[arg(long)]
        operator_xonly: String,
        #[arg(long, value_delimiter = ',', required = true)]
        setup_secret_files: Vec<PathBuf>,
        #[arg(long, default_value = "challenge_assert")]
        kind: String,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
        #[arg(long, default_value_t = 144)]
        csv: u32,
        #[arg(long, default_value_t = 72)]
        assert_csv: u32,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = false)]
        wipe: bool,
    },
    /// Print a presigned Challenge/abort tx (no secrets).
    BtcBitvm2BroadcastPresign {
        #[arg(long)]
        json: PathBuf,
    },
    BtcNumsKickoff {
        #[arg(long)]
        utxo: String,
        #[arg(long)]
        dest: String,
        #[arg(long)]
        dest_secret_file: PathBuf,
        #[arg(long, default_value = "")]
        watch_secret_file: PathBuf,
        #[arg(long)]
        amount: u64,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
        #[arg(long)]
        from: String,
        #[arg(long)]
        abort_seed_file: PathBuf,
    },
    /// Spend retired v1 NUMS (anyone-CHECKSIG) into v2 dest+watch NUMS.
    BtcNumsMigrateV1 {
        #[arg(long)]
        utxo: String,
        #[arg(long)]
        dest_secret_file: PathBuf,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
    },
    /// Dest-lock ELSE take after CSV. User key only.
    BtcDestLockTake {
        #[arg(long)]
        privhex_file: PathBuf,
        #[arg(long)]
        prev_txid: String,
        #[arg(long, default_value_t = 0)]
        vout: u32,
        #[arg(long)]
        sats: u64,
        #[arg(long)]
        dest: String,
        #[arg(long)]
        from: String,
        #[arg(long)]
        amount: u64,
        #[arg(long)]
        abort_seed_file: PathBuf,
        #[arg(long, default_value_t = 1000)]
        fee: u64,
    },
}

fn load_abort_seed(path: &std::path::Path) -> Result<[u8; 32]> {
    load_secret32(path, "abort seed")
}

fn load_secret32(path: &std::path::Path, what: &str) -> Result<[u8; 32]> {
    let raw = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    if raw.len() == 32 {
        let mut s = [0u8; 32];
        s.copy_from_slice(&raw);
        return Ok(s);
    }
    let txt = std::str::from_utf8(&raw).unwrap_or("").trim();
    let h = txt.trim_start_matches("0x");
    if h.len() == 64 {
        let b = hex::decode(h)?;
        if b.len() == 32 {
            let mut s = [0u8; 32];
            s.copy_from_slice(&b);
            return Ok(s);
        }
    }
    if raw.len() < 32 {
        bail!("{what} must be 32 bytes");
    }
    let mut s = [0u8; 32];
    s.copy_from_slice(&raw[..32]);
    Ok(s)
}

async fn rpc_on_split<R, W>(reader: R, mut writer: W, msg: WireMsg, skip_hello: bool) -> Result<WireMsg>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let line = serde_json::to_string(&msg)? + "\n";
    writer.write_all(line.as_bytes()).await?;
    let mut lines = BufReader::new(reader).lines();
    // Mesh inbound used to fan ProposeLedger onto ctl sockets. Skip gossip
    // until a directed reply (Status / Account / Ok / Err) arrives.
    for _ in 0..64 {
        let Some(resp) = lines.next_line().await? else {
            bail!("no response");
        };
        let m: WireMsg = serde_json::from_str(&resp)?;
        match m {
            WireMsg::Hello { .. } if skip_hello => continue,
            WireMsg::ProposeLedger { .. }
            | WireMsg::ProposeHeader { .. }
            | WireMsg::NeedLedgerBody { .. }
            | WireMsg::LedgerBody { .. }
            | WireMsg::LedgerBodyById { .. }
            | WireMsg::NeedTxsByIds { .. }
            | WireMsg::TxsByIds { .. }
            | WireMsg::Pong { .. }
            | WireMsg::LedgerOk { .. }
            | WireMsg::SkipPacker { .. }
            | WireMsg::ViewChange { .. }
            | WireMsg::OnlineSetVote { .. }
            | WireMsg::NeedLedgers { .. }
            | WireMsg::NeedTxs { .. }
            | WireMsg::TxsBatch { .. }
            | WireMsg::LedgersBatch { .. }
            | WireMsg::SubmitTx { .. } => continue,
            other => return Ok(other),
        }
    }
    bail!("no useful response")
}

async fn rpc(addr: &str, msg: WireMsg) -> Result<WireMsg> {
    let stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("connect {addr}"))?;
    let (reader, writer) = stream.into_split();
    rpc_on_split(reader, writer, msg, true).await
}

const DASH_RPC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// `rpc` with a deadline: an older node may never answer a message it cannot parse.
async fn rpc_timeout(addr: &str, msg: WireMsg, t: std::time::Duration) -> Result<WireMsg> {
    match tokio::time::timeout(t, rpc(addr, msg)).await {
        Ok(r) => r,
        Err(_) => bail!("timeout after {}s", t.as_secs()),
    }
}

/// Panel-grid renderer for `dash` (htop-style). ctl-only.
mod dash_grid {
    use fd_pl::health::dash::{human_bytes, human_dur, sparkline};
    use serde_json::Value;

    #[derive(Clone, Copy, PartialEq, Debug)]
    pub enum Mode {
        Dark,
        Light,
        Plain,
    }

    #[derive(Clone, Copy, PartialEq)]
    pub enum C {
        Text,
        Label,
        Title,
        Border,
        Good,
        Warn,
        Bad,
        Dim,
        Bar,
        Key,
    }

    pub type Line = Vec<(C, String)>;

    pub fn t(c: C, s: impl Into<String>) -> (C, String) {
        (c, s.into())
    }

    fn sgr(mode: Mode, c: C) -> String {
        let dark = mode == Mode::Dark;
        let (fg, bg, bold): (u8, Option<u8>, bool) = match c {
            C::Bar => (255, Some(if dark { 23 } else { 24 }), false),
            C::Key => (16, Some(44), true),
            other => {
                let fg = if dark {
                    match other {
                        C::Text => 252,
                        C::Label => 250,
                        C::Title => 87,
                        C::Border => 30,
                        C::Good => 78,
                        C::Warn => 221,
                        C::Bad => 203,
                        _ => 242,
                    }
                } else {
                    match other {
                        C::Text => 235,
                        C::Label => 238,
                        C::Title => 24,
                        C::Border => 31,
                        C::Good => 28,
                        C::Warn => 130,
                        C::Bad => 160,
                        _ => 246,
                    }
                };
                (fg, if dark { Some(234) } else { None }, other == C::Title)
            }
        };
        let bgs = bg.map(|b| format!("48;5;{b}")).unwrap_or_else(|| "49".into());
        format!("\x1b[{};38;5;{fg};{bgs}m", if bold { 1 } else { 22 })
    }

    pub fn width(l: &Line) -> usize {
        l.iter().map(|(_, s)| s.chars().count()).sum()
    }

    /// Cut to `w` columns (… marks a cut), then pad with spaces in `pad` colour.
    pub fn fit(l: &Line, w: usize) -> Line {
        let mut out = Vec::new();
        let mut used = 0;
        let total = width(l);
        for (c, s) in l {
            if used >= w {
                break;
            }
            let n = s.chars().count();
            if used + n <= w && !(total > w && used + n == w) {
                out.push((*c, s.clone()));
                used += n;
            } else {
                let keep = w.saturating_sub(used).saturating_sub(1);
                let mut s2: String = s.chars().take(keep).collect();
                s2.push('…');
                used += s2.chars().count();
                out.push((*c, s2));
                break;
            }
        }
        if used < w {
            out.push((C::Text, " ".repeat(w - used)));
        }
        out
    }

    pub fn paint(mode: Mode, l: &Line) -> String {
        if mode == Mode::Plain {
            return l.iter().map(|(_, s)| s.as_str()).collect::<String>().trim_end().to_string();
        }
        let mut o = String::new();
        let mut last: Option<C> = None;
        for (c, s) in l {
            if last != Some(*c) {
                o.push_str(&sgr(mode, *c));
                last = Some(*c);
            }
            o.push_str(s);
        }
        o.push_str("\x1b[0m");
        o
    }

    pub struct Panel {
        pub title: String,
        pub lines: Vec<Line>,
    }

    fn boxed(p: &Panel, w: usize, h: usize) -> Vec<Line> {
        let w = w.max(12);
        let inner = w - 4;
        let head = format!("┌─┤ ");
        let tail_len = w.saturating_sub(head.chars().count() + p.title.chars().count() + 3);
        let mut o: Vec<Line> = vec![vec![
            t(C::Border, head),
            t(C::Title, p.title.clone()),
            t(C::Border, format!(" ├{}┐", "─".repeat(tail_len))),
        ]];
        let body = h.saturating_sub(2);
        let n = p.lines.len();
        for i in 0..body {
            let l: Line = if i + 1 == body && n > body {
                vec![t(C::Dim, format!("… +{} more", n - body + 1))]
            } else {
                p.lines.get(i).cloned().unwrap_or_default()
            };
            let mut row = vec![t(C::Border, "│ ")];
            row.extend(fit(&l, inner));
            row.push(t(C::Border, " │"));
            o.push(row);
        }
        o.push(vec![t(C::Border, format!("└{}┘", "─".repeat(w - 2)))]);
        o
    }

    /// Lay panels out in rows (`cols` per row) inside `w` × `h` (h = None:
    /// unlimited). Rows shrink (tallest first) so the frame fits.
    pub fn layout(panels: &[Panel], cols: usize, w: usize, h: Option<usize>) -> Vec<Line> {
        let rows: Vec<&[Panel]> = panels.chunks(cols.max(1)).collect();
        let mut hs: Vec<usize> = rows.iter().map(|r| r.iter().map(|p| p.lines.len()).max().unwrap_or(0) + 2).collect();
        if let Some(h) = h {
            while hs.iter().sum::<usize>() > h {
                let (i, &m) = hs.iter().enumerate().max_by_key(|(_, &v)| v).unwrap();
                if m <= 3 {
                    break;
                }
                hs[i] -= 1;
            }
            // Spare rows go to the last panel row (peers / warnings).
            let sum: usize = hs.iter().sum();
            if sum < h {
                if let Some(l) = hs.last_mut() {
                    *l += h - sum;
                }
            }
        }
        let mut out = Vec::new();
        for (r, rh) in rows.iter().zip(hs) {
            let n = r.len();
            let base = w / cols.max(1);
            let boxes: Vec<Vec<Line>> = r
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let pw = if i + 1 == n { w - base * (n - 1) } else { base };
                    boxed(p, pw, rh)
                })
                .collect();
            for y in 0..rh {
                let mut l = Line::new();
                for b in &boxes {
                    l.extend(b[y].iter().cloned());
                }
                out.push(l);
            }
        }
        if let Some(h) = h {
            out.truncate(h);
        }
        out
    }

    fn u(v: &Value, p: &str) -> Option<u64> {
        v.pointer(p).and_then(|x| x.as_u64())
    }
    fn s<'a>(v: &'a Value, p: &str) -> &'a str {
        v.pointer(p).and_then(|x| x.as_str()).unwrap_or("?")
    }
    fn num(n: Option<u64>) -> String {
        n.map(|x| x.to_string()).unwrap_or_else(|| "?".into())
    }
    fn hhmmss(ms: u64) -> String {
        let t = (ms / 1000) % 86_400;
        format!("{:02}:{:02}:{:02}", t / 3600, (t / 60) % 60, t % 60)
    }
    fn kv(k: &str, v: Vec<(C, String)>) -> Line {
        let mut l = vec![t(C::Label, format!("{k:<15}"))];
        l.extend(v);
        l
    }
    fn kv1(k: &str, v: impl Into<String>) -> Line {
        kv(k, vec![t(C::Text, v)])
    }
    fn age(ms: u64) -> String {
        match ms {
            0..=999 => format!("{ms}ms"),
            1000..=119_999 => format!("{}s", ms / 1000),
            120_000..=7_199_999 => format!("{}m", ms / 60_000),
            7_200_000..=172_799_999 => format!("{}h", ms / 3_600_000),
            _ => format!("{}d", ms / 86_400_000),
        }
    }
    fn barl(frac: f64, w: usize, c: C) -> Vec<(C, String)> {
        let f = frac.clamp(0.0, 1.0);
        let n = (f * w as f64).round() as usize;
        vec![t(C::Dim, "["), t(c, "|".repeat(n)), t(C::Dim, format!("{}]", " ".repeat(w - n)))]
    }
    fn short_hash(h: &str) -> String {
        if h.len() > 12 { format!("{}…{}", &h[..6], &h[h.len() - 4..]) } else { h.into() }
    }

    /// Collapse repeated warnings (digits ignored when matching), newest first.
    pub fn collapse_warnings(a: &[Value]) -> Vec<(u64, String, String, usize)> {
        let mut out: Vec<(u64, String, String, usize, String)> = Vec::new();
        for e in a {
            let msg = s(e, "/msg").to_string();
            let lvl = s(e, "/level").to_string();
            let key: String = format!("{lvl}|{}", msg.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect::<String>());
            let at = u(e, "/at_ms").unwrap_or(0);
            if let Some(x) = out.iter_mut().find(|x| x.4 == key) {
                x.3 += 1;
                if at >= x.0 {
                    x.0 = at;
                    x.2 = msg;
                }
            } else {
                out.push((at, lvl, msg, 1, key));
            }
        }
        out.sort_by(|a, b| b.0.cmp(&a.0));
        out.into_iter().map(|(a, l, m, n, _)| (a, l, m, n)).collect()
    }

    fn warn_lines(a: &[Value]) -> Vec<Line> {
        let mut w = Vec::new();
        for (at, lvl, msg, n) in collapse_warnings(a) {
            let (c, tag) = match lvl.as_str() {
                "ERROR" => (C::Bad, "ERR "),
                "WARN" => (C::Warn, "WARN"),
                _ => (C::Dim, "INFO"),
            };
            let mut l = vec![t(C::Dim, format!("{} ", hhmmss(at))), t(c, format!("{tag} ")), t(C::Text, msg)];
            if n > 1 {
                l.push(t(C::Dim, format!(" ×{n}")));
            }
            w.push(l);
        }
        if w.is_empty() {
            w.push(vec![t(C::Good, "none")]);
        }
        w
    }

    /// Inner width a panel gets at total width `w` with `cols` columns.
    fn inner(w: usize, cols: usize) -> usize {
        (w / cols.max(1)).saturating_sub(4)
    }

    /// Node panel "Balance" rows (read-only `/balance` block; integer FPL).
    pub fn balance_lines(h: &Value) -> Vec<Line> {
        use fd_pl::health::balance::fpl;
        let Some(b) = h.get("balance").filter(|b| b.is_object()) else {
            return vec![kv("Balance", vec![t(C::Dim, "n/a")])];
        };
        let g = |p: &str| u(b, p).unwrap_or(0);
        let mut l = vec![
            vec![t(C::Label, "── Balance ──")],
            kv1("Liquid", format!("{} FPL", fpl(g("/liquid")))),
            kv1("Bonded", format!("{} FPL", fpl(g("/bonded")))),
            kv1("Pending rwd", format!("{} FPL", fpl(g("/pending_rewards")))),
            kv1("Seat rwd", format!("{} FPL", fpl(g("/seat_claimable")))),
            kv1(
                "Seat payout",
                b.get("seat_payout").and_then(|x| x.as_str()).unwrap_or("(none: claim-seat)").to_string(),
            ),
        ];
        if g("/unbonding") > 0 {
            l.push(kv("Unbonding", vec![t(C::Warn, format!("{} FPL", fpl(g("/unbonding"))))]));
        }
        if let Some(a) = b.get("bond_account").and_then(|x| x.as_str()) {
            let v = b.get("bond_account_liquid").and_then(|x| x.as_u64());
            l.push(kv1("Bond acct", format!("{a} {}", v.map(|n| format!("{} FPL", fpl(n))).unwrap_or_default())));
        }
        l
    }

    pub fn health_panels(h: &Value, w: usize, cols: usize) -> Vec<Panel> {
        let iw = inner(w, cols);
        let bw = iw.saturating_sub(15 + 14).clamp(5, 24);
        let status = s(h, "/node/status");
        let sc = match status {
            "active" => C::Good,
            "syncing" | "observer" => C::Warn,
            _ => C::Bad,
        };
        let node = Panel {
            title: "Node".into(),
            lines: vec![
                kv1("ID", s(h, "/node/id")),
                kv1("Version", s(h, "/node/version")),
                kv1("Network", format!("{} ({})", num(u(h, "/node/network_id")), s(h, "/node/network"))),
                kv1("Uptime", u(h, "/node/uptime_secs").map(human_dur).unwrap_or("?".into())),
                kv("Status", vec![t(sc, format!("● {}", status.to_uppercase()))]),
                kv1("Role", s(h, "/node/role")),
            ],
        };
        let mut node = node;
        node.lines.extend(balance_lines(h));
        let lag = u(h, "/chain/lag").unwrap_or(0);
        let lc = if lag < 3 { C::Good } else if lag < fd_pl::health::SYNCING_LAG { C::Warn } else { C::Bad };
        let spark: Vec<u64> = h
            .pointer("/chain/blocks_per_min_spark")
            .and_then(|a| a.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_u64()).collect())
            .unwrap_or_default();
        let spark = &spark[spark.len().saturating_sub(iw.saturating_sub(15))..];
        let bpm = h.pointer("/chain/blocks_per_min").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let local = u(h, "/chain/local_height").unwrap_or(0);
        let mut chain = vec![
            kv1("Local height", num(u(h, "/chain/local_height"))),
            kv1("Network tip", num(u(h, "/chain/network_tip"))),
            kv("Lag", vec![t(lc, format!("{lag} blocks"))]),
            kv1("Blocks/min", format!("{bpm:.1}  (5 min)")),
            kv(&format!("Last {} min", spark.len()), vec![t(C::Good, sparkline(spark))]),
        ];
        if let Some(lb) = h.pointer("/chain/last_block").filter(|v| !v.is_null()) {
            let a = u(h, "/chain/tip_advanced_age_ms").or(u(lb, "/age_ms")).unwrap_or(0);
            chain.push(kv1("Last block", format!("{} ago  by {}", age(a), s(lb, "/packer"))));
            chain.push(kv("Tip hash", vec![t(C::Dim, short_hash(s(lb, "/hash")))]));
        }
        let missed = u(h, "/consensus/missed_turns").unwrap_or(0);
        let limit = u(h, "/consensus/missed_turns_limit").unwrap_or(20).max(1);
        let mc = if missed == 0 { C::Good } else if missed * 2 < limit { C::Warn } else { C::Bad };
        let lp = h
            .pointer("/consensus/last_proposal")
            .filter(|v| !v.is_null())
            .map(|p| {
                let ph = u(p, "/height").unwrap_or(0);
                format!("#{ph} ({} blocks ago)", local.saturating_sub(ph))
            })
            .unwrap_or_else(|| "none in window".into());
        let mut mt = vec![t(mc, format!("{missed}/{limit} "))];
        mt.extend(barl(missed as f64 / limit as f64, bw, mc));
        let cons = Panel {
            title: "Consensus".into(),
            lines: vec![
                kv1("Epoch", format!("{}  (progress {}%)", num(u(h, "/consensus/epoch")), num(u(h, "/consensus/epoch_progress_pct")))),
                kv1("Proposals", format!("{} this epoch", num(u(h, "/consensus/proposals_this_epoch")))),
                kv1("Last proposal", lp),
                kv1("Votes", format!("{} this epoch", num(u(h, "/consensus/votes_this_epoch")))),
                kv("Missed turns", mt),
                kv1("Bond", format!("{} FPL", num(u(h, "/consensus/bond")))),
                kv1("Mempool", format!("{} / {}", num(u(h, "/mempool/size")), num(u(h, "/mempool/max")))),
            ],
        };
        // Peers: live first, stale (tip 0 or no pong for 2 min) dimmed last.
        let mut peers = vec![vec![
            t(C::Label, "Connected  "),
            t(C::Good, num(u(h, "/peers/count"))),
            t(C::Text, format!("  (out {} · in {})", num(u(h, "/peers/outbound")), num(u(h, "/peers/inbound")))),
        ]];
        let mut live = Vec::new();
        let mut stale: Vec<String> = Vec::new();
        if let Some(a) = h.pointer("/peers/seats").and_then(|x| x.as_array()) {
            for p in a {
                let tip = u(p, "/tip").unwrap_or(0);
                let pong = u(p, "/pong_age_ms");
                let idw = iw.saturating_sub(20).clamp(8, 24);
                let id: String = s(p, "/id").chars().take(idw).collect();
                if tip == 0 || pong.map_or(false, |a| a > 120_000) {
                    stale.push(id);
                } else {
                    let c = if local.saturating_sub(tip) >= fd_pl::health::SYNCING_LAG { C::Warn } else { C::Text };
                    live.push(vec![
                        t(c, format!("{id:<idw$} {tip:>9}  ")),
                        t(C::Dim, format!("{:>6}", pong.map(age).unwrap_or("-".into()))),
                    ]);
                }
            }
        }
        peers.extend(live);
        if !stale.is_empty() {
            // Retired / stale seats (tip 0 or no pong for 2 min): one dim line.
            peers.push(vec![t(C::Dim, format!("stale ({}): {}", stale.len(), stale.join(" ")))]);
        }
        let mut sys = Vec::new();
        let sb = iw.saturating_sub(6 + 18).clamp(5, 24);
        let lvl = |f: f64| if f > 0.9 { C::Bad } else if f > 0.75 { C::Warn } else { C::Good };
        if let Some(c) = h.pointer("/system/process/cpu_pct").and_then(|x| x.as_f64()) {
            let cpus = u(h, "/system/process/cpus").unwrap_or(1).max(1) as f64;
            let mut l = vec![t(C::Label, "CPU   ")];
            l.extend(barl(c / 100.0 / cpus, sb, lvl(c / 100.0 / cpus)));
            l.push(t(C::Text, format!(" {c:.0}% of {} cores", cpus as u64)));
            sys.push(l);
        }
        if let Some(r) = u(h, "/system/process/rss_bytes") {
            let tot = u(h, "/system/process/mem_total_bytes").unwrap_or(0);
            let f = if tot > 0 { r as f64 / tot as f64 } else { 0.0 };
            let mut l = vec![t(C::Label, "Mem   ")];
            l.extend(barl(f, sb, lvl(f)));
            l.push(t(C::Text, format!(" {}/{}", human_bytes(r), human_bytes(tot))));
            sys.push(l);
        }
        if let (Some(free), Some(total)) = (u(h, "/system/disk/free_bytes"), u(h, "/system/disk/total_bytes")) {
            let f = if total > 0 { total.saturating_sub(free) as f64 / total as f64 } else { 0.0 };
            let mut l = vec![t(C::Label, "Disk  ")];
            l.extend(barl(f, sb, lvl(f)));
            l.push(t(C::Text, format!(" {} free", human_bytes(free))));
            sys.push(l);
        }
        if let Some(l) = h.pointer("/system/process/loadavg").and_then(|x| x.as_array()) {
            let l: Vec<String> = l.iter().filter_map(|x| x.as_f64()).map(|x| format!("{x:.2}")).collect();
            sys.push(vec![t(C::Label, "Load  "), t(C::Text, l.join(" "))]);
        }
        if sys.is_empty() {
            sys.push(vec![t(C::Dim, "n/a")]);
        }
        let warns = h.pointer("/warnings").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        vec![
            node,
            Panel { title: "Chain".into(), lines: chain },
            cons,
            Panel { title: "Peers".into(), lines: peers },
            Panel { title: "System".into(), lines: sys },
            Panel { title: "Recent warnings".into(), lines: warn_lines(&warns) },
        ]
    }

    pub fn fallback_panels(b: &Value, why: &str) -> Vec<Panel> {
        let id = s(b, "/id");
        let seat = b
            .pointer("/validators")
            .and_then(|x| x.as_array())
            .and_then(|a| a.iter().find(|v| v.get("id").and_then(|x| x.as_str()) == Some(id)));
        let mut cons = vec![kv1("Epoch", num(u(b, "/epoch")))];
        if let Some(v) = seat {
            let jailed = v.get("jailed").and_then(|x| x.as_bool()).unwrap_or(false);
            cons.push(kv1("Bond", num(u(v, "/bond"))));
            cons.push(kv("Jailed", vec![t(if jailed { C::Bad } else { C::Good }, jailed.to_string())]));
            cons.push(kv1("Missed turns", format!("{}/20", num(u(v, "/v2/missed_turns")))));
            cons.push(kv1("Lottery ready", v.get("lottery_ready").and_then(|x| x.as_bool()).unwrap_or(false).to_string()));
        }
        let mut sys = Vec::new();
        if let (Some(a), Some(tot)) = (u(b, "/disk/avail_bytes"), u(b, "/disk/total_bytes")) {
            sys.push(vec![t(C::Label, "Disk  "), t(C::Text, format!("{} free of {}", human_bytes(a), human_bytes(tot)))]);
        } else {
            sys.push(vec![t(C::Dim, "n/a")]);
        }
        vec![
            Panel {
                title: "Node".into(),
                lines: vec![
                    kv1("ID", id),
                    kv1("Version", s(b, "/product_version")),
                    kv1("Network", num(u(b, "/network_id"))),
                    kv("Status", vec![t(C::Warn, "legacy status (no health)")]),
                ],
            },
            Panel {
                title: "Chain".into(),
                lines: vec![
                    kv1("Local height", num(u(b, "/tip_height"))),
                    kv1("Best peer tip", num(u(b, "/best_peer_tip"))),
                    kv1("Lag", num(u(b, "/tip_lag"))),
                    kv1("Mempool", num(u(b, "/mempool"))),
                ],
            },
            Panel { title: "Consensus".into(), lines: cons },
            Panel { title: "Peers".into(), lines: vec![vec![t(C::Dim, "n/a on this node version")]] },
            Panel { title: "System".into(), lines: sys },
            Panel {
                title: "Recent warnings".into(),
                lines: vec![
                    vec![t(C::Warn, format!("health endpoint unavailable ({why})"))],
                    vec![t(C::Dim, "showing legacy status; upgrade the node for the full view")],
                ],
            },
        ]
    }

    /// Title bar + panels + footer, every row exactly `w` wide.
    pub fn frame(
        panels: &[Panel],
        title_l: String,
        title_r: String,
        footer_note: &str,
        w: usize,
        h: Option<usize>,
    ) -> Vec<Line> {
        let cols = if w >= 120 { 3 } else if w >= 80 { 2 } else { 1 };
        let mut out = Vec::new();
        let gap = w.saturating_sub(title_l.chars().count() + title_r.chars().count());
        out.push(fit(&vec![t(C::Bar, format!("{title_l}{}{title_r}", " ".repeat(gap)))], w));
        let body_h = h.map(|h| h.saturating_sub(2));
        let ordered: Vec<&Panel> = if cols == 2 {
            // Node|Chain, Consensus|System, Peers|Warnings
            [0, 1, 2, 4, 3, 5].iter().filter_map(|&i| panels.get(i)).collect()
        } else {
            panels.iter().collect()
        };
        let owned: Vec<Panel> = ordered.into_iter().map(|p| Panel { title: p.title.clone(), lines: p.lines.clone() }).collect();
        let body = layout(&owned, cols, w, body_h);
        let used = body.len();
        out.extend(body);
        if let Some(bh) = body_h {
            for _ in used..bh {
                out.push(vec![t(C::Text, " ".repeat(w))]);
            }
        }
        let mut f = vec![t(C::Key, " q "), t(C::Bar, " Quit  "), t(C::Key, " r "), t(C::Bar, " Refresh  ")];
        f.push(t(C::Bar, footer_note.to_string()));
        let fw = width(&f);
        if fw < w {
            f.push(t(C::Bar, " ".repeat(w - fw)));
        }
        out.push(fit(&f, w));
        out
    }

    pub fn panels_for(cols_w: usize) -> usize {
        if cols_w >= 120 { 3 } else if cols_w >= 80 { 2 } else { 1 }
    }
}

/// `falcon-pl-ctl dash` (installer: `falcon-node dash`).
mod dash {
    use super::*;
    use super::dash_grid::{self as g, Mode};

    pub enum Frame {
        Health(serde_json::Value),
        Fallback(serde_json::Value, String),
        Down(String),
    }

    pub async fn fetch(addr: &str) -> Frame {
        let why = match rpc_timeout(addr, WireMsg::HealthReq { warnings: 20 }, DASH_RPC_TIMEOUT).await {
            Ok(WireMsg::Health { mut body }) => {
                crate::bal::ensure(addr, &mut body).await;
                return Frame::Health(body);
            }
            Ok(WireMsg::Err { msg }) => msg,
            Ok(other) => format!("unexpected reply {:?}", std::mem::discriminant(&other)),
            Err(e) => e.to_string(),
        };
        let why: String = why.chars().take(80).collect();
        match rpc_timeout(addr, WireMsg::StatusReq { include_accounts: false, brief: true }, DASH_RPC_TIMEOUT).await {
            Ok(WireMsg::Status { body }) => Frame::Fallback(body, why),
            Ok(_) => Frame::Down(why),
            Err(e) => Frame::Down(format!("{e:#}")),
        }
    }

    fn now_label() -> String {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let t = ms % 86_400;
        format!("{:02}:{:02}:{:02} UTC", t / 3600, (t / 60) % 60, t % 60)
    }

    /// Terminal size from stdout (TIOCGWINSZ), else COLUMNS/LINES.
    pub fn term_size() -> (Option<usize>, Option<usize>) {
        // SAFETY: TIOCGWINSZ fills a winsize struct we own.
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            if libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
                return (Some(ws.ws_col as usize), Some(ws.ws_row as usize));
            }
        }
        let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse().ok()).filter(|&n: &usize| n > 0);
        (env("COLUMNS"), env("LINES"))
    }

    pub fn render(f: &Frame, mode: Mode, addr: &str, interval: f64, w: usize, h: Option<usize>) -> Vec<String> {
        let now = now_label();
        let cols = g::panels_for(w);
        let (panels, id, extra, note) = match f {
            Frame::Health(b) => (
                g::health_panels(b, w, cols),
                b.pointer("/node/id").and_then(|x| x.as_str()).unwrap_or("?").to_string(),
                format!(
                    "network {} · epoch {} · ",
                    b.pointer("/node/network_id").and_then(|x| x.as_u64()).map(|x| x.to_string()).unwrap_or("?".into()),
                    b.pointer("/consensus/epoch").and_then(|x| x.as_u64()).map(|x| x.to_string()).unwrap_or("?".into())
                ),
                " full health view",
            ),
            Frame::Fallback(b, why) => (
                g::fallback_panels(b, why),
                b.get("id").and_then(|x| x.as_str()).unwrap_or("?").to_string(),
                format!("network {} · ", b.get("network_id").and_then(|x| x.as_u64()).unwrap_or(0)),
                " fallback: legacy status (node has no health endpoint)",
            ),
            Frame::Down(e) => (
                vec![g::Panel {
                    title: "Node".into(),
                    lines: vec![
                        vec![g::t(g::C::Bad, format!("node RPC not answering: {e}"))],
                        vec![g::t(g::C::Dim, "retrying…")],
                    ],
                }],
                "?".into(),
                String::new(),
                " node down",
            ),
        };
        let tl = format!(" falcon-node dash — {id} @ {addr}");
        let tr = format!("{extra}{now} · refresh {interval}s ");
        g::frame(&panels, tl, tr, note, w, h).iter().map(|l| g::paint(mode, l)).collect()
    }

    /// Raw, no-echo stdin while the dash runs; restored on drop.
    struct RawTerm(Option<libc::termios>);
    impl RawTerm {
        fn enter() -> Self {
            // SAFETY: tcgetattr/tcsetattr on fd 0 with a valid termios buffer.
            unsafe {
                if libc::isatty(0) != 1 {
                    return RawTerm(None);
                }
                let mut t: libc::termios = std::mem::zeroed();
                if libc::tcgetattr(0, &mut t) != 0 {
                    return RawTerm(None);
                }
                let orig = t;
                t.c_lflag &= !(libc::ICANON | libc::ECHO);
                t.c_cc[libc::VMIN] = 1;
                t.c_cc[libc::VTIME] = 0;
                libc::tcsetattr(0, libc::TCSANOW, &t);
                RawTerm(Some(orig))
            }
        }
    }
    impl Drop for RawTerm {
        fn drop(&mut self) {
            if let Some(t) = self.0 {
                // SAFETY: restoring the attributes read in `enter`.
                unsafe {
                    libc::tcsetattr(0, libc::TCSANOW, &t);
                }
            }
            print!("\x1b[?25h\x1b[?1049l");
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
    }

    /// Seat-reward claim from the dash (`c`): signs `ClaimSeat` with the
    /// local seat key, exactly like `falcon-pl-ctl claim-seat`.
    pub struct ClaimCfg {
        pub keys_dir: String,
        pub scheme: String,
        pub network_id: u32,
    }

    /// Destination check before signing: the node's own rule (non-empty,
    /// <= 128, not a `#seat` sender) plus a printable, space-free id.
    pub fn claim_dest_ok(raw: &str) -> Result<String, String> {
        let to = raw.trim();
        if to.is_empty() {
            return Err("destination is empty".into());
        }
        if to.len() > 128 {
            return Err("destination is longer than 128 characters".into());
        }
        if !to.bytes().all(|b| b.is_ascii_graphic()) {
            return Err("destination must be printable ASCII without spaces".into());
        }
        if fd_pl::seat_of_sender(to).is_some() {
            return Err("destination cannot be a #seat sender".into());
        }
        Ok(to.to_string())
    }

    /// (seat id, pot) when this node is a validator with seat rewards.
    pub fn claim_target(f: &Frame) -> Result<(String, u64), String> {
        let Frame::Health(b) = f else {
            return Err("node health not available".into());
        };
        if b.pointer("/node/role").and_then(|x| x.as_str()) != Some("validator") {
            return Err("this node is not a validator seat".into());
        }
        let id = b.pointer("/node/id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let pot = b.pointer("/balance/seat_claimable").and_then(|x| x.as_u64()).unwrap_or(0);
        if id.is_empty() {
            return Err("seat id unknown".into());
        }
        if pot == 0 {
            return Err("seat reward pot is empty".into());
        }
        Ok((id, pot))
    }

    fn draw(lines: &[String]) {
        use std::io::Write;
        let mut out = String::from("\x1b[H\x1b[0m\x1b[2J");
        for l in lines {
            out.push_str(l);
            out.push_str("\r\n");
        }
        print!("{out}");
        let _ = std::io::stdout().flush();
    }

    #[cfg(test)]
    pub async fn claim_flow_for_test(
        addr: &str,
        cfg: &ClaimCfg,
        f: &Frame,
        krx: &mut tokio::sync::mpsc::UnboundedReceiver<u8>,
    ) -> String {
        claim_flow(addr, cfg, f, krx).await
    }

    /// Interactive claim. Returns the one-line outcome for the dash footer.
    async fn claim_flow(
        addr: &str,
        cfg: &ClaimCfg,
        f: &Frame,
        krx: &mut tokio::sync::mpsc::UnboundedReceiver<u8>,
    ) -> String {
        use fd_pl::health::balance::fpl;
        if cfg.keys_dir.is_empty() {
            return "claim: no --keys-dir (run via 'falcon-node dash')".into();
        }
        let (seat, pot) = match claim_target(f) {
            Ok(x) => x,
            Err(e) => return format!("claim: {e}"),
        };
        let head = vec![
            " Claim seat rewards".to_string(),
            String::new(),
            format!(" seat           {seat}"),
            format!(" seat pot       {} FPL", fpl(pot)),
        ];
        let mut typed = String::new();
        let mut err = String::new();
        let to = loop {
            let mut l = head.clone();
            l.push(format!(" destination    {typed}_"));
            l.push(String::new());
            l.push(" type the account to credit · Enter continue · Esc cancel".into());
            if !err.is_empty() {
                l.push(format!(" ! {err}"));
            }
            draw(&l);
            match krx.recv().await {
                None | Some(3) | Some(27) => return "claim cancelled".into(),
                Some(b'\r') | Some(b'\n') => match claim_dest_ok(&typed) {
                    Ok(t) => break t,
                    Err(e) => err = e,
                },
                Some(127) | Some(8) => {
                    typed.pop();
                }
                Some(b) if b.is_ascii_graphic() && typed.len() < 128 => typed.push(b as char),
                _ => {}
            }
        };
        let mut l = head.clone();
        l.push(format!(" destination    {to}"));
        l.push(String::new());
        l.push(format!(" Move {} FPL (minus the fee) from seat {seat} to {to}? [y/N]", fpl(pot)));
        draw(&l);
        match krx.recv().await {
            Some(b'y') | Some(b'Y') => {}
            _ => return "claim cancelled".into(),
        }
        l.push(" submitting…".into());
        draw(&l);
        let body = TxBody::ClaimSeat { validator: seat.clone(), to: to.clone() };
        let msg = match submit_seat_body(addr, &seat, 0, &cfg.scheme, &cfg.keys_dir, cfg.network_id, body).await {
            Ok((tx, _)) => format!("claim submitted: tx {} ({} FPL pot -> {to})", tx.tx_id, fpl(pot)),
            Err(e) => format!("claim failed: {}", format!("{e:#}").chars().take(160).collect::<String>()),
        };
        l.pop();
        l.push(format!(" {msg}"));
        l.push(" any key returns to the dash".into());
        draw(&l);
        let _ = krx.recv().await;
        msg
    }

    pub async fn run(addr: &str, interval: f64, once: bool, no_color: bool, light: bool, claim: &ClaimCfg) -> Result<()> {
        use std::io::{IsTerminal, Read, Write};
        let tty = std::io::stdout().is_terminal();
        let mode = if !tty || no_color || std::env::var_os("NO_COLOR").is_some() {
            Mode::Plain
        } else if light || std::env::var("FALCON_DASH_THEME").map_or(false, |v| v == "light") {
            Mode::Light
        } else {
            Mode::Dark
        };
        if once || !tty {
            let f = fetch(addr).await;
            let (w, _) = term_size();
            for l in render(&f, mode, addr, interval, w.unwrap_or(132), None) {
                println!("{l}");
            }
            return match f {
                Frame::Down(e) => bail!("node RPC not answering: {e}"),
                _ => Ok(()),
            };
        }
        let interval = std::time::Duration::from_secs_f64(interval.clamp(0.5, 60.0));
        let (ktx, mut krx) = tokio::sync::mpsc::unbounded_channel::<u8>();
        let _raw = RawTerm::enter();
        std::thread::spawn(move || {
            let mut b = [0u8; 1];
            let mut stdin = std::io::stdin();
            while let Ok(1) = stdin.read(&mut b) {
                if ktx.send(b[0]).is_err() {
                    break;
                }
            }
        });
        print!("\x1b[?1049h\x1b[?25l");
        let mut notice = String::new();
        loop {
            let f = fetch(addr).await;
            let (w, h) = term_size();
            let (w, h) = (w.unwrap_or(120), h.unwrap_or(40));
            let mut out = String::from("\x1b[H");
            let mut lines = render(&f, mode, addr, interval.as_secs_f64(), w, Some(h.saturating_sub(1)));
            let hint = if claim.keys_dir.is_empty() { "" } else { " · c claim seat rewards" };
            let foot: String = format!(" {notice}{}{hint}", if notice.is_empty() { "q quit" } else { " · q quit" })
                .chars()
                .take(w)
                .collect();
            lines.push(foot);
            let n = lines.len();
            for (i, l) in lines.into_iter().enumerate() {
                out.push_str(&l);
                if i + 1 < n {
                    out.push_str("\r\n");
                }
            }
            out.push_str("\x1b[0m\x1b[J");
            print!("{out}");
            std::io::stdout().flush()?;
            let deadline = tokio::time::sleep(interval);
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    _ = &mut deadline => break,
                    k = krx.recv() => match k {
                        Some(b'q') | Some(b'Q') | Some(3) | None => return Ok(()),
                        Some(b'r') | Some(b' ') => break,
                        Some(b'c') | Some(b'C') => {
                            notice = claim_flow(addr, claim, &f, &mut krx).await;
                            break;
                        }
                        _ => {}
                    },
                    _ = tokio::signal::ctrl_c() => return Ok(()),
                }
            }
        }
    }
}

/// `falcon-pl-ctl web` (installer: `falcon-node web`).
mod web {
    use super::*;
    use fd_pl::health_web::{check_bind, host_ok, parse_head, response, History, Sample, HISTORY_CAP, INDEX_HTML};
    use std::sync::Arc;

    fn unix_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    async fn health(addr: &str) -> Result<serde_json::Value, String> {
        match rpc_timeout(addr, WireMsg::HealthReq { warnings: 20 }, DASH_RPC_TIMEOUT).await {
            Ok(WireMsg::Health { mut body }) => {
                crate::bal::ensure(addr, &mut body).await;
                Ok(body)
            }
            Ok(WireMsg::Err { msg }) => Err(format!("node has no health endpoint ({msg}); upgrade the node")),
            Ok(_) => Err("unexpected reply".into()),
            Err(e) => Err(format!("{e:#}")),
        }
    }

    fn json_resp(status: &str, v: &serde_json::Value) -> Vec<u8> {
        response(status, "application/json", serde_json::to_string(v).unwrap_or_default().as_bytes())
    }

    async fn handle(
        mut sock: TcpStream,
        node: Arc<String>,
        hist: Arc<parking_lot::Mutex<History>>,
        allow_remote: bool,
        sample_secs: u64,
    ) {
        let mut buf = Vec::with_capacity(1024);
        let mut chunk = [0u8; 1024];
        let read = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                use tokio::io::AsyncReadExt;
                let n = sock.read(&mut chunk).await.ok()?;
                if n == 0 {
                    return None;
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    return Some(i);
                }
                if buf.len() > 8192 {
                    return None;
                }
            }
        })
        .await;
        let out = match read {
            Ok(Some(end)) => {
                let head = String::from_utf8_lossy(&buf[..end]).into_owned();
                match parse_head(&head) {
                    None => response("400 Bad Request", "text/plain", b"bad request"),
                    Some(r) if !host_ok(r.host.as_deref(), allow_remote) => {
                        response("421 Misdirected Request", "text/plain", b"host not allowed (loopback only)")
                    }
                    Some(r) if r.method != "GET" && r.method != "HEAD" => {
                        response("405 Method Not Allowed", "text/plain", b"read-only")
                    }
                    Some(r) => {
                        let mut o = match r.path.as_str() {
                            "/" | "/index.html" => response("200 OK", "text/html; charset=utf-8", INDEX_HTML.as_bytes()),
                            "/health/v1" => match health(&node).await {
                                Ok(b) => json_resp("200 OK", &b),
                                Err(e) => json_resp("502 Bad Gateway", &serde_json::json!({"error": e})),
                            },
                            "/history/v1" => {
                                let j = hist.lock().to_json(sample_secs);
                                json_resp("200 OK", &j)
                            }
                            _ => response("404 Not Found", "text/plain", b"not found"),
                        };
                        if r.method == "HEAD" {
                            if let Some(i) = o.windows(4).position(|w| w == b"\r\n\r\n") {
                                o.truncate(i + 4);
                            }
                        }
                        o
                    }
                }
            }
            _ => return,
        };
        let _ = sock.write_all(&out).await;
        let _ = sock.shutdown().await;
    }

    pub async fn run(node_addr: &str, listen: &str, allow_remote: bool, sample_secs: u64) -> Result<()> {
        let bind = check_bind(listen, allow_remote).map_err(|e| anyhow::anyhow!(e))?;
        let l = tokio::net::TcpListener::bind(bind).await.with_context(|| format!("bind {bind}"))?;
        let bound = l.local_addr()?;
        let sample_secs = sample_secs.clamp(1, 3600);
        let hist = Arc::new(parking_lot::Mutex::new(History::new(HISTORY_CAP)));
        let node = Arc::new(node_addr.to_string());
        {
            let (hist, node) = (hist.clone(), node.clone());
            tokio::spawn(async move {
                let mut iv = tokio::time::interval(std::time::Duration::from_secs(sample_secs));
                loop {
                    iv.tick().await;
                    if let Ok(b) = health(&node).await {
                        hist.lock().push(Sample::from_health(unix_ms(), &b));
                    }
                }
            });
        }
        eprintln!("falcon-node web: http://{bound}/  (node {node_addr}; Ctrl-C stops)");
        if !bound.ip().is_loopback() {
            eprintln!("WARNING: listening on {bound} with no authentication (--allow-remote)");
        }
        loop {
            tokio::select! {
                a = l.accept() => {
                    if let Ok((s, _)) = a {
                        tokio::spawn(handle(s, node.clone(), hist.clone(), allow_remote, sample_secs));
                    }
                }
                _ = tokio::signal::ctrl_c() => return Ok(()),
            }
        }
    }
}

/// Operator balance (read-only): node `balance` health block on 2.9.79+, else
/// built from the existing `AccountQuery` + brief `StatusReq`. No key files.
mod bal {
    use super::*;
    use serde_json::Value;

    async fn account(addr: &str, id: &str) -> Option<Value> {
        match rpc_timeout(addr, WireMsg::AccountQuery { account: id.to_string() }, DASH_RPC_TIMEOUT).await {
            Ok(WireMsg::Account { body }) => Some(body),
            _ => None,
        }
    }

    async fn status(addr: &str) -> Option<Value> {
        match rpc_timeout(addr, WireMsg::StatusReq { include_accounts: false, brief: true }, DASH_RPC_TIMEOUT).await {
            Ok(WireMsg::Status { body }) => Some(body),
            _ => None,
        }
    }

    /// Balance block for `id` from the existing account + status queries.
    pub async fn from_queries(addr: &str, id: &str, st: Option<Value>) -> Result<Value> {
        let acct = account(addr, id).await;
        let st = match st { Some(s) => Some(s), None => status(addr).await };
        if acct.is_none() && st.is_none() {
            bail!("node {addr} did not answer the account/status query");
        }
        let mut b = fd_pl::health::balance::from_account_status(id, acct.as_ref(), st.as_ref());
        if let Some(ba) = b.get("bond_account").and_then(|x| x.as_str()).map(str::to_string) {
            if let Some(a) = account(addr, &ba).await {
                b["bond_account_liquid"] = a.get("balance").cloned().unwrap_or(Value::Null);
            }
        }
        Ok(b)
    }

    /// Add `/balance` to a health body from an older node (no-op on 2.9.79+).
    pub async fn ensure(addr: &str, h: &mut Value) {
        if h.get("balance").map_or(false, |b| b.is_object()) {
            return;
        }
        let id = h.pointer("/node/id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if id.is_empty() {
            return;
        }
        if let Ok(b) = from_queries(addr, &id, None).await {
            h["balance"] = b;
        }
    }

    pub async fn fetch(addr: &str, account: Option<&str>) -> Result<Value> {
        if let Some(a) = account.filter(|a| !a.is_empty()) {
            return from_queries(addr, a, None).await;
        }
        if let Ok(WireMsg::Health { body }) = rpc_timeout(addr, WireMsg::HealthReq { warnings: 0 }, DASH_RPC_TIMEOUT).await {
            if let Some(b) = body.get("balance").filter(|b| b.is_object()) {
                return Ok(b.clone());
            }
            if let Some(id) = body.pointer("/node/id").and_then(|x| x.as_str()) {
                return from_queries(addr, id, None).await;
            }
        }
        let st = status(addr).await.with_context(|| format!("node {addr} not answering"))?;
        let id = st.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        if id.is_empty() {
            bail!("node status has no id; pass --account");
        }
        from_queries(addr, &id, Some(st)).await
    }
}

async fn rpc_unix(path: &str, msg: WireMsg) -> Result<WireMsg> {
    let stream = UnixStream::connect(path)
        .await
        .with_context(|| format!("connect unix {path}"))?;
    let (reader, writer) = stream.into_split();
    rpc_on_split(reader, writer, msg, false).await
}

/// Operator mutations: prefer `--admin-sock` once the TCP wire is locked.
async fn rpc_admin(addr: &str, admin_sock: &str, msg: WireMsg) -> Result<WireMsg> {
    if !admin_sock.is_empty() {
        rpc_unix(admin_sock, msg).await
    } else {
        rpc(addr, msg).await
    }
}

async fn next_sequence(addr: &str, account: &str) -> Result<u64> {
    let r = rpc(
        addr,
        WireMsg::AccountQuery {
            account: account.to_string(),
        },
    )
    .await?;
    match r {
        WireMsg::Account { body } => body
            .get("sequence")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("account response missing sequence")),
        WireMsg::Err { msg } => bail!(msg),
        other => bail!("unexpected account response: {other:?}"),
    }
}

/// Seat payout: sign `body` as `<validator>#seat` with the seat key loaded
/// from `keys_dir` (key material stays in memory; nothing is printed).
async fn submit_seat_body(
    addr: &str,
    validator: &str,
    fee: u64,
    scheme: &str,
    keys_dir: &str,
    network_id: u32,
    body: TxBody,
) -> Result<(Tx, WireMsg)> {
    let keys = load_keys(scheme, keys_dir, validator).await?;
    let seq = next_sequence(addr, &fd_pl::seat_sender(validator)).await?;
    let need_fee = suggested_fee(addr, fee).await;
    let tx = Tx::sign_seat_body(&keys, validator, seq, need_fee, network_id, body)
        .map_err(|e| anyhow::anyhow!(e))?;
    let msg = WireMsg::SubmitTx { tx: tx.clone() };
    match rpc(addr, msg.clone()).await? {
        WireMsg::Err { msg } => anyhow::bail!("{msg}"),
        other => {
            fan_same_tx(addr, &msg).await;
            Ok((tx, other))
        }
    }
}

async fn resolve_seq(addr: &str, account: &str, sequence: u64) -> Result<u64> {
    if sequence == 0 {
        next_sequence(addr, account).await
    } else {
        Ok(sequence)
    }
}

async fn load_keys(scheme: &str, keys_dir: &str, account: &str) -> Result<KeyRing> {
    let scheme = CryptoScheme::parse(scheme).context("scheme")?;
    if !keys_dir.is_empty() {
        KeyRing::load_dir_slim(keys_dir, &[account]).context("keys_dir")
    } else if scheme == CryptoScheme::HmacDev {
        let mut k = KeyRing::new(scheme);
        k.generate_id(account);
        Ok(k)
    } else {
        bail!("falcon-512 requires --keys-dir");
    }
}

fn prev_spk_for_privhex(
    privhex_file: &PathBuf,
    prev_spk: &str,
    secret: &[u8; 32],
) -> Result<Vec<u8>> {
    if !prev_spk.is_empty() {
        return hex::decode(prev_spk.trim()).context("prev_spk");
    }
    let json_path = privhex_file.with_extension("json");
    if json_path.exists() {
        if let Ok(raw) = std::fs::read_to_string(&json_path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(spk) = v.get("p2wpkh_spk").and_then(|x| x.as_str()) {
                    if !spk.is_empty() {
                        return hex::decode(spk.trim()).context("p2wpkh_spk");
                    }
                }
                if let Some(addr) = v.get("address").and_then(|x| x.as_str()) {
                    let a = addr.trim().to_ascii_lowercase();
                    if a.starts_with("tb1q") || a.starts_with("bc1q") || a.starts_with("bcrt1q") {
                        let pk = fd_pl::vault::pubkey_from_secret(secret).map_err(|e| anyhow::anyhow!(e))?;
                        let h = fd_pl::vault::hash160(&pk);
                        return fd_pl::btc_tx::p2wpkh_spk(&h).map_err(|e| anyhow::anyhow!(e));
                    }
                }
            }
        }
    }
    let pk = fd_pl::vault::pubkey_from_secret(secret).map_err(|e| anyhow::anyhow!(e))?;
    let h = fd_pl::vault::hash160(&pk);
    fd_pl::btc_tx::p2pkh_spk(&h).map_err(|e| anyhow::anyhow!(e))
}

async fn suggested_fee(addr: &str, fee: u64) -> u64 {
    match rpc(
        addr,
        WireMsg::StatusReq {
            include_accounts: false,
            brief: false,
        },
    )
    .await
    {
        Ok(WireMsg::Status { body }) => body
            .get("suggested_min_fee")
            .and_then(|v| v.as_u64())
            .unwrap_or(fee)
            .max(fee),
        _ => fee,
    }
}

async fn submit_body(
    addr: &str,
    account: &str,
    destination: &str,
    amount: u64,
    fee: u64,
    sequence: u64,
    scheme: &str,
    keys_dir: &str,
    network_id: u32,
    body: TxBody,
) -> Result<(Tx, WireMsg)> {
    let keys = load_keys(scheme, keys_dir, account).await?;
    let seq = resolve_seq(addr, account, sequence).await?;
    let need_fee = suggested_fee(addr, fee).await;
    let tx = Tx::sign_body(
        &keys,
        account,
        seq,
        destination,
        amount,
        need_fee,
        network_id,
        body,
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    let msg = WireMsg::SubmitTx { tx: tx.clone() };
    let fan_msg = msg.clone();
    let r = if let Ok(sock) = std::env::var("FPL_ADMIN_SOCK") {
        if sock.is_empty() {
            rpc(addr, msg).await?
        } else {
            rpc_unix(&sock, msg).await?
        }
    } else {
        rpc(addr, msg).await?
    };
    match r {
        WireMsg::Err { msg } => anyhow::bail!("{msg}"),
        other => {
            fan_same_tx(addr, &fan_msg).await;
            Ok((tx, other))
        }
    }
}

/// Sign consecutive execution-header batches and push them all before fanning.
/// A packer seals whatever is already in the mempool (up to 128 or the 1s budget).
/// Sending them one ctl process apart lets each batch seal alone.
async fn submit_exec_wave(
    addr: &str,
    account: &str,
    json_file: &std::path::Path,
    fee: u64,
    sequence: u64,
    scheme: &str,
    keys_dir: &str,
    network_id: u32,
) -> Result<()> {
    let raw = std::fs::read_to_string(json_file)?;
    let chunks: Vec<serde_json::Value> = serde_json::from_str(&raw)?;
    if chunks.is_empty() {
        bail!("empty exec wave");
    }
    if chunks.len() > 128 {
        bail!("exec wave {} exceeds 128", chunks.len());
    }
    let keys = load_keys(scheme, keys_dir, account).await?;
    let base = resolve_seq(addr, account, sequence).await?;
    let need_fee = suggested_fee(addr, fee).await;
    let mut txs = Vec::with_capacity(chunks.len());
    for (i, chunk) in chunks.iter().enumerate() {
        let headers_json = serde_json::to_string(chunk)?;
        let tx = Tx::sign_body(
            &keys,
            account,
            base + i as u64,
            "",
            0,
            need_fee,
            network_id,
            TxBody::EthExecHeaders { headers_json },
        )
        .map_err(|e| anyhow::anyhow!(e))?;
        txs.push(tx);
    }
    let admin = std::env::var("FPL_ADMIN_SOCK").unwrap_or_default();
    let mut accepted = 0u64;
    for tx in &txs {
        let msg = WireMsg::SubmitTx { tx: tx.clone() };
        match rpc_admin(addr, &admin, msg).await {
            Ok(WireMsg::Err { msg })
                if msg.contains("duplicate") || msg.contains("already") || msg.contains("stale") =>
            {
                accepted += 1;
            }
            Ok(WireMsg::Err { msg }) => {
                bail!("seq {} not accepted: {msg}", tx.sequence);
            }
            Ok(_) => accepted += 1,
            Err(e) => bail!("seq {} submit: {e}", tx.sequence),
        }
    }
    let peers: Vec<String> = std::env::var("FPL_PACKERS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != addr && *s != "127.0.0.1:19301")
        .map(str::to_string)
        .collect();
    let mut joins = Vec::new();
    for peer in peers {
        let batch = txs.clone();
        joins.push(tokio::spawn(async move {
            for tx in batch {
                match rpc(&peer, WireMsg::SubmitTx { tx }).await {
                    Ok(WireMsg::Err { msg })
                        if msg.contains("duplicate")
                            || msg.contains("stale")
                            || msg.contains("already") => {}
                    Ok(_) => {}
                    Err(e) => eprintln!("fan {peer}: {e}"),
                }
            }
        }));
    }
    for join in joins {
        let _ = join.await;
    }
    let end = base + accepted.saturating_sub(1);
    println!("wave txs={accepted} seq={base}..{end}");
    Ok(())
}

/// One signature, delivered to every packer. Hubs do not forward SubmitTx.
async fn fan_same_tx(primary: &str, msg: &WireMsg) {
    let Ok(list) = std::env::var("FPL_PACKERS") else {
        return;
    };
    for addr in list.split(',').map(str::trim).filter(|s| !s.is_empty() && *s != primary) {
        match rpc(addr, msg.clone()).await {
            Ok(WireMsg::Err { msg })
                if msg.contains("duplicate")
                    || msg.contains("stale")
                    || msg.contains("already") => {}
            Ok(_) => {}
            Err(e) => eprintln!("fan {addr}: {e}"),
        }
    }
}

async fn pay_signed(
    addr: &str,
    from: &str,
    to: &str,
    amount: u64,
    fee: u64,
    sequence: u64,
    scheme: &str,
    keys_dir: &str,
    network_id: u32,
) -> Result<(Tx, WireMsg)> {
    submit_body(
        addr,
        from,
        to,
        amount,
        fee,
        sequence,
        scheme,
        keys_dir,
        network_id,
        TxBody::Pay,
    )
    .await
}

async fn pay_asset_signed(
    addr: &str,
    from: &str,
    to: &str,
    asset: &str,
    amount: u64,
    fee: u64,
    sequence: u64,
    scheme: &str,
    keys_dir: &str,
    network_id: u32,
) -> Result<(Tx, WireMsg)> {
    submit_body(
        addr,
        from,
        to,
        amount,
        fee,
        sequence,
        scheme,
        keys_dir,
        network_id,
        TxBody::AssetPay {
            asset: asset.trim().to_ascii_uppercase(),
        },
    )
    .await
}

/// Exit status when the node answered a request with `{"type":"err",...}` (#58).
/// The reply is still printed on stdout; only the exit status changes.
const EXIT_NODE_REFUSED: i32 = 3;
static NODE_REFUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True when a serialised node reply is a refusal (`type == "err"`).
fn reply_is_refusal(v: &serde_json::Value) -> bool {
    v.get("type").and_then(|t| t.as_str()) == Some("err")
}

/// Print a node reply as pretty JSON and remember a refusal so `main` exits non-zero.
fn print_reply<T: serde::Serialize>(r: &T) -> Result<()> {
    let v = serde_json::to_value(r)?;
    println!("{}", serde_json::to_string_pretty(&v)?);
    if reply_is_refusal(&v) {
        NODE_REFUSED.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    run().await?;
    if NODE_REFUSED.load(std::sync::atomic::Ordering::SeqCst) {
        eprintln!("falcon-pl-ctl: node refused the request (exit {EXIT_NODE_REFUSED})");
        std::process::exit(EXIT_NODE_REFUSED);
    }
    Ok(())
}

async fn run() -> Result<()> {
    let args = Args::parse();
    match args.cmd {
        Cmd::GenKeys {
            keys_dir,
            scheme,
            validators,
            load_wallets,
        } => {
            let scheme = CryptoScheme::parse(&scheme).context("scheme")?;
            let mut k = KeyRing::new(scheme);
            k.generate_many(validators.iter().map(|s| s.as_str()));
            let users = fd_pl::genesis_account_ids(load_wallets);
            k.generate_many(users.iter().map(|s| s.as_str()));
            k.save_dir(&keys_dir)?;
            println!(
                "wrote scheme={} validators={:?} users={} → {}",
                scheme.as_str(),
                validators,
                users.len(),
                keys_dir.display()
            );
        }
        Cmd::VaultGen {
            keys_dir,
            seats,
            legacy_shamir_sweep,
        } => {
            use fd_pl::vault::{
                p2wpkh_testnet_address, pubkey_from_secret, split_secret, threshold,
                vault_pub_from_secret, write_share,
            };
            use sha2::{Digest, Sha256};
            use std::io::Read;
            if !legacy_shamir_sweep {
                bail!(
                    "vault-gen writes every Shamir share into one keys_dir (custodial). \
                     Run frost-dkg-part1/2/3 on each seat. Pass --legacy-shamir-sweep only \
                     to sweep the existing P2WPKH after shares are already split."
                );
            }
            let n = seats.len() as u32;
            let t = threshold(n);
            let mut secret = [0u8; 32];
            std::fs::File::open("/dev/urandom")
                .context("urandom")?
                .read_exact(&mut secret)
                .context("urandom read")?;
            secret[0] &= 0x3f;
            let shares = split_secret(&secret, n, t, 0, b"fpl-2300-vault-gen").map_err(|e| anyhow::anyhow!(e))?;
            for (seat, sh) in seats.iter().zip(shares.iter()) {
                write_share(&keys_dir, seat, sh).map_err(|e| anyhow::anyhow!(e))?;
            }
            let pk = pubkey_from_secret(&secret).map_err(|e| anyhow::anyhow!(e))?;
            let vp = vault_pub_from_secret(&secret, 0, n, t).map_err(|e| anyhow::anyhow!(e))?;
            let addr = p2wpkh_testnet_address(&pk).map_err(|e| anyhow::anyhow!(e))?;
            let pub_path = keys_dir.join("vault-pub.json");
            let body = serde_json::json!({
                "epoch": vp.epoch,
                "n": vp.n,
                "t": vp.t,
                "pubkey_hex": vp.pubkey_hex,
                "spk_hex": vp.spk_hex,
                "address": addr,
                "network": "testnet",
                "script_version": 1,
            });
            std::fs::write(&pub_path, serde_json::to_string_pretty(&body)? + "\n")?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&pub_path, std::fs::Permissions::from_mode(0o644));
            }
            // Zero secret in this process; never print it.
            secret.fill(0);
            let _ = Sha256::digest(secret);
            println!("vault-pub {}", pub_path.display());
            println!("address {addr}");
            println!("n={n} t={t} seats={}", seats.join(","));
            println!("shares written under {} (mode 600). secret not printed.", keys_dir.display());
        }
        Cmd::FrostDkgPart1 {
            keys_dir,
            seat,
            seats,
            t,
        } => {
            let t = if t == 0 {
                fd_pl::vault::threshold(seats.len() as u32) as u16
            } else {
                t
            };
            let p = fd_pl::frost_vault::dkg_part1(&seats, &seat, t, &keys_dir)
                .map_err(|e| anyhow::anyhow!(e))?;
            println!("r1 public {}", p.display());
            println!("seat {seat} n={} t={t} (r1 secret is 600, not printed)", seats.len());
        }
        Cmd::FrostDkgPart2 {
            keys_dir,
            seat,
            seats,
        } => {
            let out = fd_pl::frost_vault::dkg_part2(&seats, &seat, &keys_dir)
                .map_err(|e| anyhow::anyhow!(e))?;
            println!("r2 packages {} for seat {seat}", out.len());
        }
        Cmd::FrostDkgPart3 {
            keys_dir,
            seat,
            seats,
        } => {
            let fp = fd_pl::frost_vault::dkg_part3(&seats, &seat, &keys_dir)
                .map_err(|e| anyhow::anyhow!(e))?;
            println!("share {}", fd_pl::frost_vault::share_path(&keys_dir, &seat).display());
            println!("pub {}", fd_pl::frost_vault::pubkey_path(&keys_dir).display());
            println!("address {}", fp.address);
            println!("n={} t={} scheme={}", fp.n, fp.t, fp.scheme);
        }
        Cmd::FrostSignRound1 { keys_dir, seat } => {
            let p = fd_pl::frost_vault::sign_round1(&keys_dir, &seat).map_err(|e| anyhow::anyhow!(e))?;
            println!("commit {}", p.display());
        }
        Cmd::FrostSignRound2 {
            keys_dir,
            seat,
            seats,
            signing_package,
        } => {
            let raw = std::fs::read_to_string(&signing_package)?;
            let pkg: frost_secp256k1_tr::SigningPackage =
                serde_json::from_str(&raw).context("signing package")?;
            let p = fd_pl::frost_vault::sign_round2(&keys_dir, &seats, &seat, &pkg)
                .map_err(|e| anyhow::anyhow!(e))?;
            println!("sigshare {}", p.display());
        }
        Cmd::FrostAggregate {
            keys_dir,
            signers,
            seats,
            signing_package,
            message_hex,
        } => {
            let _ = message_hex;
            let raw = std::fs::read_to_string(&signing_package)?;
            let pkg: frost_secp256k1_tr::SigningPackage =
                serde_json::from_str(&raw).context("signing package")?;
            let pub_pkg = fd_pl::frost_vault::read_pubkey_package(&keys_dir)
                .map_err(|e| anyhow::anyhow!(e))?;
            let sig = fd_pl::frost_vault::aggregate_shares(
                &seats,
                &signers,
                &keys_dir,
                &pub_pkg,
                &pkg,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!("{}", hex::encode(sig));
        }
        Cmd::FrostMakePackage {
            keys_dir,
            signers,
            seats,
            message_hex,
            out,
        } => {
            let msg = hex::decode(message_hex.trim().trim_start_matches("0x")).context("message_hex")?;
            let pkg = fd_pl::frost_vault::make_signing_package(&seats, &signers, &keys_dir, &msg)
                .map_err(|e| anyhow::anyhow!(e))?;
            std::fs::write(&out, serde_json::to_string(&pkg)? + "\n")?;
            println!("signing-package {}", out.display());
        }
        Cmd::BtcFrostKeypathUnsigned {
            utxo,
            dest,
            amount,
            fee,
            vault_spk,
        } => {
            let parts: Vec<&str> = utxo.split(':').collect();
            if parts.len() != 3 {
                bail!("utxo must be txid:vout:sats");
            }
            let vault = hex::decode(vault_spk.trim()).context("vault_spk")?;
            let (_k, dest_spk) = fd_pl::btc_tx::decode_address(&dest).map_err(|e| anyhow::anyhow!(e))?;
            let (pre, outs) = fd_pl::frost_vault::keypath_pay_sighash(
                parts[0],
                parts[1].parse()?,
                parts[2].parse()?,
                &dest_spk,
                amount,
                fee,
                &vault,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "sighash": hex::encode(pre),
                    "outputs": outs.len(),
                    "change": outs.get(1).map(|o| o.sats).unwrap_or(0),
                }))?
            );
        }
        Cmd::BtcFrostKeypathFinish {
            utxo,
            dest,
            amount,
            fee,
            vault_spk,
            sig_hex,
        } => {
            let parts: Vec<&str> = utxo.split(':').collect();
            if parts.len() != 3 {
                bail!("utxo must be txid:vout:sats");
            }
            let vault = hex::decode(vault_spk.trim()).context("vault_spk")?;
            let (_k, dest_spk) = fd_pl::btc_tx::decode_address(&dest).map_err(|e| anyhow::anyhow!(e))?;
            let (_pre, outs) = fd_pl::frost_vault::keypath_pay_sighash(
                parts[0],
                parts[1].parse()?,
                parts[2].parse()?,
                &dest_spk,
                amount,
                fee,
                &vault,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            let sig = hex::decode(sig_hex.trim().trim_start_matches("0x")).context("sig")?;
            let raw = fd_pl::frost_vault::keypath_pay_finish(
                parts[0],
                parts[1].parse()?,
                parts[2].parse()?,
                &outs,
                &vault,
                &sig,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!("{raw}");
        }
        Cmd::BtcFrostKickoffUnsigned {
            utxo,
            dest,
            amount,
            fee,
            vault_spk,
            from,
            abort_seed_file,
        } => {
            let parts: Vec<&str> = utxo.split(':').collect();
            if parts.len() != 3 {
                bail!("utxo must be txid:vout:sats");
            }
            let vault = hex::decode(vault_spk.trim()).context("vault_spk")?;
            let seed = load_abort_seed(&abort_seed_file)?;
            let u = fd_pl::frost_vault::kickoff_unsigned(
                parts[0],
                parts[1].parse()?,
                parts[2].parse()?,
                &dest,
                amount,
                fee,
                &vault,
                &from,
                &seed,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "sighash": hex::encode(u.sighash),
                    "note_id": u.note_id,
                    "claim_spk": hex::encode(&u.claim_spk),
                    "amount": u.amount,
                    "change": u.change,
                    "fee": u.fee,
                }))?
            );
        }
        Cmd::BtcFrostKickoffFinish {
            utxo,
            dest,
            amount,
            fee,
            vault_spk,
            from,
            abort_seed_file,
            sig_hex,
        } => {
            let parts: Vec<&str> = utxo.split(':').collect();
            if parts.len() != 3 {
                bail!("utxo must be txid:vout:sats");
            }
            let vault = hex::decode(vault_spk.trim()).context("vault_spk")?;
            let seed = load_abort_seed(&abort_seed_file)?;
            let u = fd_pl::frost_vault::kickoff_unsigned(
                parts[0],
                parts[1].parse()?,
                parts[2].parse()?,
                &dest,
                amount,
                fee,
                &vault,
                &from,
                &seed,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            let sig = hex::decode(sig_hex.trim().trim_start_matches("0x")).context("sig")?;
            let raw = fd_pl::frost_vault::kickoff_finish(
                parts[0],
                parts[1].parse()?,
                parts[2].parse()?,
                &u.outputs,
                &vault,
                &sig,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!("{raw}");
        }
        Cmd::BtcNumsPool => {
            let spk = fd_pl::bitvm::nums_pool_spk().map_err(|e| anyhow::anyhow!(e))?;
            let addr = fd_pl::bitvm::nums_pool_address().map_err(|e| anyhow::anyhow!(e))?;
            let v1 = fd_pl::bitvm::nums_pool_address_v1().map_err(|e| anyhow::anyhow!(e))?;
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "address": addr,
                    "v1_address": v1,
                    "spk": hex::encode(spk),
                    "internal_xonly": hex::encode(fd_pl::bitvm::nums_internal_xonly()),
                    "watch_xonly": hex::encode(fd_pl::bitvm::watch_xonly()),
                    "key_path": "unspendable-NUMS",
                    "kickoff": "dest+watch",
                }))?
            );
        }
        Cmd::BtcBitvm2Instance {
            operator_xonly,
            setup_xonly,
            csv,
            assert_csv,
            lamport_pk_json,
        } => {
            let mut keys = fd_pl::bitvm2_instance::InstanceKeys::from_hex(
                &operator_xonly,
                &setup_xonly,
                csv,
                assert_csv,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            if let Some(p) = lamport_pk_json.as_ref().filter(|p| p.as_os_str().len() > 0) {
                let pk: fd_pl_bitvm2::winternitz::LamportPk =
                    serde_json::from_str(&std::fs::read_to_string(p)?)?;
                keys = keys.with_lamport(pk);
            }
            let addr = fd_pl::bitvm2_instance::instance_address(&keys)
                .map_err(|e| anyhow::anyhow!(e))?;
            let spk = fd_pl::bitvm2_instance::instance_spk(&keys)
                .map_err(|e| anyhow::anyhow!(e))?;
            let tree = fd_pl::bitvm2_instance::instance_tree(&keys)
                .map_err(|e| anyhow::anyhow!(e))?;
            let asp = fd_pl::bitvm2_instance::assert_spk(&keys)
                .map_err(|e| anyhow::anyhow!(e))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "address": addr,
                    "spk": hex::encode(spk),
                    "assert_spk": hex::encode(asp),
                    "merkle_root": tree.merkle_root_hex,
                    "nums_tag": tree.nums_tag,
                    "csv_blocks": csv,
                    "assert_csv_blocks": assert_csv,
                    "key_path": "unspendable-NUMS",
                    "watch_key": "none",
                    "kickoff": "claimer-CHECKSIG dest-lock (clock starts on dest-lock)",
                    "challenge": "setup-n-of-n-presign-once-to-assert-or-instance",
                    "leaves": tree.leaves.iter().map(|l| serde_json::json!({
                        "kind": l.kind,
                        "chunk_id": l.chunk_id,
                        "tapleaf_hash": l.tapleaf_hash_hex,
                    })).collect::<Vec<_>>(),
                    "notes": tree.notes,
                }))?
            );
        }
        Cmd::BtcBitvm2Payout { .. } => {
            bail!("operator-to-self payout removed; use btc-bitvm2-user-kickoff (dest-lock)");
        }
        Cmd::BtcBitvm2Challenge {
            utxo,
            operator_xonly,
            setup_secret_files,
            digest,
            fee,
            csv,
            assert_csv,
            lamport_pk_json,
        } => {
            let parts: Vec<&str> = utxo.split(':').collect();
            if parts.len() != 3 {
                bail!("utxo must be txid:vout:sats");
            }
            let mut setup_x = Vec::new();
            let mut secrets = Vec::new();
            for p in &setup_secret_files {
                let sk = load_secret32(p, "setup secret")?;
                let x = fd_pl::btc_tx::xonly_from_secret(&sk).map_err(|e| anyhow::anyhow!(e))?;
                setup_x.push(x);
                secrets.push(sk);
            }
            let mut keys = fd_pl::bitvm2_instance::InstanceKeys::from_hex(
                &operator_xonly,
                &setup_x.iter().map(hex::encode).collect::<Vec<_>>(),
                csv,
                assert_csv,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            if let Some(p) = lamport_pk_json.as_ref().filter(|p| p.as_os_str().len() > 0) {
                let pk: fd_pl_bitvm2::winternitz::LamportPk =
                    serde_json::from_str(&std::fs::read_to_string(p)?)?;
                keys = keys.with_lamport(pk);
            }
            let dig = if digest.trim().is_empty() {
                None
            } else {
                let b = hex::decode(digest.trim()).context("digest")?;
                if b.len() != 32 {
                    bail!("digest must be 32 bytes");
                }
                let mut d = [0u8; 32];
                d.copy_from_slice(&b);
                Some(d)
            };
            let mut sigs = Vec::new();
            for sk in &secrets {
                sigs.push(
                    fd_pl::bitvm2_instance::sign_challenge_partial(
                        &keys,
                        sk,
                        parts[0],
                        parts[1].parse()?,
                        parts[2].parse()?,
                        fee,
                        dig.as_ref(),
                    )
                    .map_err(|e| anyhow::anyhow!(e))?,
                );
            }
            let raw = fd_pl::bitvm2_instance::assemble_challenge(
                &keys,
                &sigs,
                parts[0],
                parts[1].parse()?,
                parts[2].parse()?,
                fee,
                dig.as_ref(),
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!("{raw}");
        }
        Cmd::BtcBitvm2UserKickoff {
            utxo,
            claimer_secret_file,
            setup_xonly,
            dest,
            amount,
            abort_commit_hex,
            claim_csv,
            fee,
            csv,
            assert_csv,
        } => {
            let parts: Vec<&str> = utxo.split(':').collect();
            if parts.len() != 3 {
                bail!("utxo must be txid:vout:sats");
            }
            let abort_raw = hex::decode(abort_commit_hex.trim()).context("abort commit")?;
            if abort_raw.len() != 32 {
                bail!("abort commit must be 32 bytes");
            }
            let mut abort = [0u8; 32];
            abort.copy_from_slice(&abort_raw);
            let claimer = load_secret32(&claimer_secret_file, "claimer secret")?;
            let claimer_x = fd_pl::btc_tx::xonly_from_secret(&claimer).map_err(|e| anyhow::anyhow!(e))?;
            let keys = fd_pl::bitvm2_instance::InstanceKeys::from_hex(
                &hex::encode(claimer_x),
                &setup_xonly,
                csv,
                assert_csv,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            let raw = fd_pl::bitvm2_instance::sign_kickoff(
                &keys,
                &claimer,
                parts[0],
                parts[1].parse()?,
                parts[2].parse()?,
                &dest,
                amount,
                &abort,
                claim_csv,
                fee,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!("{raw}");
        }
        Cmd::BtcBitvm2Presign {
            utxo,
            operator_xonly,
            setup_secret_files,
            kind,
            fee,
            csv,
            assert_csv,
            out,
            wipe,
        } => {
            let parts: Vec<&str> = utxo.split(':').collect();
            if parts.len() != 3 {
                bail!("utxo must be txid:vout:sats");
            }
            let mut setup_x = Vec::new();
            let mut secrets = Vec::new();
            for p in &setup_secret_files {
                let sk = load_secret32(p, "setup secret")?;
                let x = fd_pl::btc_tx::xonly_from_secret(&sk).map_err(|e| anyhow::anyhow!(e))?;
                setup_x.push(x);
                secrets.push(sk);
            }
            let keys = fd_pl::bitvm2_instance::InstanceKeys::from_hex(
                &operator_xonly,
                &setup_x.iter().map(hex::encode).collect::<Vec<_>>(),
                csv,
                assert_csv,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            let presign = fd_pl::bitvm2_instance::presign_setup_spend(
                &keys,
                &secrets,
                parts[0],
                parts[1].parse()?,
                parts[2].parse()?,
                fee,
                &kind,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            for s in secrets.iter_mut() {
                s.fill(0);
            }
            std::fs::write(&out, serde_json::to_string_pretty(&presign)?)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o600));
            }
            if wipe {
                for p in &setup_secret_files {
                    fd_pl::bitvm2_instance::wipe_secret_file(p).map_err(|e| anyhow::anyhow!(e))?;
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "wrote": out.display().to_string(),
                    "kind": presign.kind,
                    "output_spk": presign.output_spk_hex,
                    "wiped_setup_secrets": wipe,
                    "note": "setup n-of-n is not used on user Kickoff; this tx is Challenge/abort only",
                }))?
            );
        }
        Cmd::BtcBitvm2BroadcastPresign { json } => {
            let p: fd_pl::bitvm2_instance::PresignedSpend =
                serde_json::from_str(&std::fs::read_to_string(&json)?)?;
            println!("{}", p.raw_hex);
        }
        Cmd::BtcNumsKickoff {
            utxo,
            dest,
            dest_secret_file,
            watch_secret_file,
            amount,
            fee,
            from,
            abort_seed_file,
        } => {
            let parts: Vec<&str> = utxo.split(':').collect();
            if parts.len() != 3 {
                bail!("utxo must be txid:vout:sats");
            }
            let seed = load_abort_seed(&abort_seed_file)?;
            let dest_sk = load_secret32(&dest_secret_file, "dest secret")?;
            let watch_path = if watch_secret_file.as_os_str().is_empty() {
                PathBuf::from(std::env::var("FPL_WATCH_SECRET").unwrap_or_else(|_| {
                    format!(
                        "{}/keys/bitvm-watch.secret",
                        std::env::var("PL_ROOT").unwrap_or_else(|_| {
                            format!("{}/falcon-pl-public-testnet-2300", std::env::var("HOME").unwrap_or_default())
                        })
                    )
                }))
            } else {
                watch_secret_file
            };
            let watch_sk = load_secret32(&watch_path, "watch secret")?;
            let raw = fd_pl::bitvm::nums_dest_lock_kickoff(
                parts[0],
                parts[1].parse()?,
                parts[2].parse()?,
                &dest,
                &dest_sk,
                &watch_sk,
                amount,
                fee,
                &from,
                &seed,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!("{raw}");
        }
        Cmd::BtcNumsMigrateV1 {
            utxo,
            dest_secret_file,
            fee,
        } => {
            let parts: Vec<&str> = utxo.split(':').collect();
            if parts.len() != 3 {
                bail!("utxo must be txid:vout:sats");
            }
            let dest_sk = load_secret32(&dest_secret_file, "dest secret")?;
            let raw = fd_pl::bitvm::nums_migrate_v1_to_v2(
                parts[0],
                parts[1].parse()?,
                parts[2].parse()?,
                &dest_sk,
                fee,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!("{raw}");
        }
        Cmd::BtcDestLockTake {
            privhex_file,
            prev_txid,
            vout,
            sats,
            dest,
            from,
            amount,
            abort_seed_file,
            fee,
        } => {
            let hex_s = std::fs::read_to_string(&privhex_file)?.trim().to_string();
            let raw = hex::decode(hex_s.trim()).context("privhex")?;
            if raw.len() != 32 {
                bail!("privhex must be 32 bytes");
            }
            let mut secret = [0u8; 32];
            secret.copy_from_slice(&raw);
            let seed = load_abort_seed(&abort_seed_file)?;
            let note = fd_pl::bridge::btc_withdraw_note_id(&from, amount, &dest);
            let abort = fd_pl::bitvm::abort_preimage_from_seed(&seed, &note);
            let dest_h160 = fd_pl::bitvm::dest_h160_from_address(&dest).map_err(|e| anyhow::anyhow!(e))?;
            let commit = fd_pl::bitvm::abort_commit(&abort);
            let ws = fd_pl::bitvm::claim_witness_script(
                &commit,
                &dest_h160,
                fd_pl::bitvm::CLAIM_CSV_BLOCKS,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            let (_k, dest_spk) = fd_pl::btc_tx::decode_address(&dest).map_err(|e| anyhow::anyhow!(e))?;
            let tx = fd_pl::vault::sign_dest_lock_take(
                &secret,
                &prev_txid,
                vout,
                sats,
                &dest_spk,
                &ws,
                fd_pl::bitvm::CLAIM_CSV_BLOCKS,
                fee,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            secret.fill(0);
            println!("{tx}");
        }
        Cmd::BitvmAbortSeed { keys_dir } => {
            let mut seed = [0u8; 32];
            std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut seed)?;
            fd_pl::bitvm::write_abort_seed(&keys_dir, &seed).map_err(|e| anyhow::anyhow!(e))?;
            seed.fill(0);
            println!(
                "wrote {} (mode 600, not printed). copy the same 32 bytes to every seat.",
                fd_pl::bitvm::abort_seed_path(&keys_dir).display()
            );
        }
        Cmd::Status { accounts } => {
            let r = rpc(
                &args.addr,
                WireMsg::StatusReq {
                    include_accounts: accounts,
                    brief: false,
                },
            )
            .await?;
            print_reply(&r)?;
        }
        Cmd::Health { warnings } => {
            let r = rpc_timeout(&args.addr, WireMsg::HealthReq { warnings }, DASH_RPC_TIMEOUT).await?;
            print_reply(&r)?;
        }
        Cmd::Dash { interval, once, no_color, light, keys_dir, scheme, network_id } => {
            let claim = dash::ClaimCfg { keys_dir, scheme, network_id };
            dash::run(&args.addr, interval, once, no_color, light, &claim).await?;
        }
        Cmd::Web { listen, allow_remote, sample_secs } => {
            web::run(&args.addr, &listen, allow_remote, sample_secs).await?;
        }
        Cmd::Balance { account, json } => {
            let b = bal::fetch(&args.addr, account.as_deref()).await?;
            if json {
                println!("{}", serde_json::to_string_pretty(&b)?);
            } else {
                println!("{}", fd_pl::health::balance::one_line(&b));
            }
        }
        Cmd::Account { account } => {
            let r = rpc(&args.addr, WireMsg::AccountQuery { account }).await?;
            print_reply(&r)?;
        }
        Cmd::GenWallet {
            keys_dir,
            account,
            scheme,
        } => {
            let scheme = CryptoScheme::parse(&scheme).context("scheme")?;
            let mut k = if keys_dir.join("pl_keys.json").exists() {
                KeyRing::load_dir(&keys_dir)?
            } else {
                KeyRing::new(scheme)
            };
            if k.public_hex(&account).is_none() {
                k.generate_id(&account);
            }
            k.save_dir(&keys_dir)?;
            println!(
                "wallet {} scheme={} → {}",
                account,
                scheme.as_str(),
                keys_dir.display()
            );
        }
        Cmd::Faucet {
            to,
            amount,
            fee,
            sequence,
            scheme,
            keys_dir,
            network_id,
            key_hash,
        } => {
            let body = if key_hash.trim().is_empty() {
                TxBody::Pay
            } else {
                TxBody::PayToKey {
                    key_hash: key_hash.trim().to_ascii_lowercase(),
                }
            };
            let (tx, r) = submit_body(
                &args.addr,
                FAUCET_ACCOUNT,
                &to,
                amount,
                fee,
                sequence,
                &scheme,
                &keys_dir,
                network_id,
                body,
            )
            .await?;
            println!("faucet tx_id={} seq={}", tx.tx_id, tx.sequence);
            print_reply(&r)?;
        }
        Cmd::Seal { force } => {
            let msg = if force {
                WireMsg::SealForce
            } else {
                WireMsg::SealNow
            };
            let r = rpc_admin(&args.addr, &args.admin_sock, msg).await?;
            print_reply(&r)?;
        }
        Cmd::Pay {
            from,
            to,
            amount,
            fee,
            sequence,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = pay_signed(
                &args.addr,
                &from,
                &to,
                amount,
                fee,
                sequence,
                &scheme,
                &keys_dir,
                network_id,
            )
            .await?;
            println!("tx_id={} seq={} fee={}", tx.tx_id, tx.sequence, tx.fee);
            print_reply(&r)?;
        }
        Cmd::PayAsset {
            from,
            to,
            asset,
            amount,
            fee,
            sequence,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = pay_asset_signed(
                &args.addr,
                &from,
                &to,
                &asset,
                amount,
                fee,
                sequence,
                &scheme,
                &keys_dir,
                network_id,
            )
            .await?;
            println!("tx_id={} seq={} fee={}", tx.tx_id, tx.sequence, tx.fee);
            print_reply(&r)?;
        }
        Cmd::VaultOpen {
            account,
            destination,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let dest = destination.clone();
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                &destination,
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::VaultOpen {
                    destination: dest,
                },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::VaultLock {
            account,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::VaultLock,
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::Claim {
            account,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::Claim,
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::CreatePool {
            pool_id,
            asset_a,
            asset_b,
            account,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::CreatePool {
                    pool_id,
                    asset_a,
                    asset_b,
                },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::AddLiq {
            pool_id,
            provider,
            amt_a,
            amt_b,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &provider,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::AddLiquidity {
                    pool_id,
                    amt_a,
                    amt_b,
                },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::Swap {
            pool_id,
            who,
            token_in,
            amount_in,
            min_out,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &who,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::Swap {
                    pool_id,
                    token_in,
                    amount_in,
                    min_out,
                },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::CreateMarket {
            market_id,
            asset,
            account,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::CreateMarket { market_id, asset },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::LendSupply {
            market_id,
            who,
            amount,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &who,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::LendSupply { market_id, amount },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::LendWithdraw {
            market_id,
            who,
            shares,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &who,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::LendWithdraw { market_id, shares },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::LendBorrow {
            market_id,
            who,
            amount,
            collateral_fpl,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &who,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::LendBorrow {
                    market_id,
                    amount,
                    collateral_fpl,
                },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::LendRepay {
            market_id,
            who,
            amount,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &who,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::LendRepay { market_id, amount },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::LendSetPrice {
            market_id,
            who,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &who,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::LendSetPrice { market_id },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::RemoveLiq {
            pool_id,
            provider,
            lp_burn,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &provider,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::RemoveLiquidity { pool_id, lp_burn },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::SwapRoute {
            who,
            token_in,
            token_out,
            amount_in,
            min_out,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &who,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::SwapRoute {
                    token_in,
                    token_out,
                    amount_in,
                    min_out,
                },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::CreditAsset {
            account,
            asset,
            amount,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::CreditAsset { asset, amount },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::Bond {
            id,
            from,
            amount,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let vid = id.clone();
            let (tx, r) = submit_body(
                &args.addr,
                &from,
                &id,
                amount,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::Bond { validator: vid },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::Pop { id, bond_account, keys_dir, network_id } => {
            let vk = KeyRing::load_dir_slim(&keys_dir, &[id.as_str()]).context("keys_dir")?;
            let public_key = vk.public_hex(&id).context("validator public key not in --keys-dir")?;
            let pop_sig = vk
                .sign(&id, &fd_pl::params_v2::pop_message(network_id, &id, &bond_account))
                .map_err(|e| anyhow::anyhow!("pop sign: {e:?}"))?;
            println!("{}", serde_json::to_string_pretty(&serde_json::json!({
                "id": id, "bond_account": bond_account, "network_id": network_id,
                "public_key": public_key, "pop_sig": pop_sig,
            }))?);
        }
        Cmd::BondV2 { id, from, amount, validator_keys, pop, fee, scheme, keys_dir, network_id } => {
            let (public_key, pop_sig) = if !pop.is_empty() {
                let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&pop).context("--pop file")?)
                    .context("--pop json")?;
                if j["id"].as_str() != Some(id.as_str()) || j["bond_account"].as_str() != Some(from.as_str()) {
                    bail!("--pop is for id={} bond_account={}, not {id}/{from}", j["id"], j["bond_account"]);
                }
                if j["network_id"].as_u64() != Some(u64::from(network_id)) {
                    bail!("--pop network_id {} != {network_id}", j["network_id"]);
                }
                (j["public_key"].as_str().context("pop public_key")?.to_string(),
                 j["pop_sig"].as_str().context("pop pop_sig")?.to_string())
            } else {
                if validator_keys.is_empty() {
                    bail!("pass --validator-keys <dir> or --pop <file>");
                }
                let vk = KeyRing::load_dir_slim(&validator_keys, &[id.as_str()]).context("validator_keys")?;
                let public_key = vk.public_hex(&id).context("validator public key not in --validator-keys")?;
                let pop_sig = vk
                    .sign(&id, &fd_pl::params_v2::pop_message(network_id, &id, &from))
                    .map_err(|e| anyhow::anyhow!("pop sign: {e:?}"))?;
                (public_key, pop_sig)
            };
            let (tx, r) = submit_body(&args.addr, &from, &id, amount, fee, 0, &scheme, &keys_dir, network_id,
                TxBody::BondV2 { validator: id.clone(), public_key, pop_sig }).await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::ApproveValidator { id, pubkey, expires, from, fee, scheme, keys_dir, network_id } => {
            let pk = if let Some(f) = pubkey.strip_prefix('@') {
                std::fs::read_to_string(f).context("pubkey file")?.trim().to_string()
            } else {
                pubkey.trim().to_string()
            };
            let expires_height = if let Some(n) = expires.strip_prefix('+') {
                let st = rpc(&args.addr, WireMsg::StatusReq { include_accounts: false, brief: true }).await?;
                let tip = match &st {
                    WireMsg::Status { body } => body["tip_height"].as_u64().context("status tip_height")?,
                    WireMsg::Err { msg } => bail!("{msg}"),
                    other => bail!("unexpected status reply: {other:?}"),
                };
                tip + n.parse::<u64>().context("expires +N")?
            } else {
                expires.parse::<u64>().context("expires")?
            };
            let (tx, r) = submit_body(&args.addr, &from, "", 0, fee, 0, &scheme, &keys_dir, network_id,
                TxBody::ApproveValidator { id, public_key: pk, expires_height }).await?;
            println!("tx_id={} expires_height={expires_height} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::RevokeApproval { id, from, fee, scheme, keys_dir, network_id } => {
            let (tx, r) = submit_body(&args.addr, &from, "", 0, fee, 0, &scheme, &keys_dir, network_id,
                TxBody::RevokeApproval { id }).await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::Reactivate { id, from, fee, scheme, keys_dir, network_id } => {
            let signer = if from.is_empty() { id.clone() } else { from };
            let (tx, r) = submit_body(&args.addr, &signer, "", 0, fee, 0, &scheme, &keys_dir, network_id,
                TxBody::Reactivate { validator: id }).await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::ClaimSeat { validator, to, fee, scheme, keys_dir, network_id } => {
            let (tx, r) = submit_seat_body(&args.addr, &validator, fee, &scheme, &keys_dir, network_id,
                TxBody::ClaimSeat { validator: validator.clone(), to }).await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::SetSeatPayout { validator, payout_account, clear, fee, scheme, keys_dir, network_id } => {
            let p = payout_account.trim().to_string();
            if clear == !p.is_empty() {
                anyhow::bail!("give exactly one of --payout-account <account> or --clear");
            }
            let (tx, r) = submit_seat_body(&args.addr, &validator, fee, &scheme, &keys_dir, network_id,
                TxBody::SetSeatPayout { validator: validator.clone(), payout_account: (!clear).then_some(p) }).await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::SetArchive { id, on, from, fee, scheme, keys_dir, network_id } => {
            let signer = if from.is_empty() { id.clone() } else { from };
            let (tx, r) = submit_body(&args.addr, &signer, "", 0, fee, 0, &scheme, &keys_dir, network_id,
                TxBody::SetArchive { validator: id, on }).await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::RevokeArchive { id, from, fee, scheme, keys_dir, network_id } => {
            let (tx, r) = submit_body(&args.addr, &from, "", 0, fee, 0, &scheme, &keys_dir, network_id,
                TxBody::RevokeArchive { id }).await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::Unbond {
            id,
            amount,
            from,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let signer = if from.is_empty() {
                id.clone()
            } else {
                from
            };
            let (tx, r) = submit_body(
                &args.addr,
                &signer,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::Unbond {
                    validator: id,
                    amount,
                },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::CompleteUnbond {
            id,
            from,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let signer = if from.is_empty() {
                id.clone()
            } else {
                from
            };
            let (tx, r) = submit_body(
                &args.addr,
                &signer,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::CompleteUnbond { validator: id },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::Unjail {
            id,
            from,
            amount,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let vid = id.clone();
            let (tx, r) = submit_body(
                &args.addr,
                &from,
                &id,
                amount,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::Unjail { validator: vid },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::WatcherCredit {
            account,
            work,
            slot: _,
            presence_slots,
            full_presence,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let presence = if full_presence {
                fd_pl::economy::WATCHER_SLOTS_PER_EPOCH
            } else {
                presence_slots
            };
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::WatcherCredit {
                    work_units: work,
                    presence_slots: presence,
                },
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::Operator { account, body, amount, fee, scheme, keys_dir, network_id } => {
            let b: TxBody = serde_json::from_str(&body)?;
            if !matches!(
                b,
                TxBody::OperatorRegister { .. }
                    | TxBody::OperatorHeartbeat { .. }
                    | TxBody::OperatorUnbond
                    | TxBody::OperatorGraphBind { .. }
                    | TxBody::OperatorSlash { .. }
                    | TxBody::Btc4PegIn { .. }
                    | TxBody::Bitvm2AttestV2 { .. }
            ) {
                bail!("--body must be an operator_* or bitvm2_attest_v2 tx body");
            }
            let (tx, r) = submit_body(&args.addr, &account, "", amount, fee, 0, &scheme, &keys_dir, network_id, b).await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::Heartbeat {
            account,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::WatcherHeartbeat,
            )
            .await?;
            println!("tx_id={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::WatcherWork {
            account,
            asset,
            count,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            if count == 0 {
                bail!("count must be > 0");
            }
            if asset.eq_ignore_ascii_case("BTC") {
                bail!(
                    "BTC rail now requires real 80-byte Bitcoin headers; use submit-btc-header"
                );
            }
            let st = rpc(
                &args.addr,
                WireMsg::StatusReq {
                    include_accounts: false,
                    brief: false,
                },
            )
            .await?;
            let body = match st {
                WireMsg::Status { body } => body,
                WireMsg::Err { msg } => bail!(msg),
                other => bail!("unexpected status: {other:?}"),
            };
            let mut tip_h = 0u64;
            let mut tip_hash = String::new();
            if let Some(rails) = body.get("rails").and_then(|v| v.as_array()) {
                for r in rails {
                    if r.get("asset").and_then(|v| v.as_str()) == Some(asset.as_str()) {
                        tip_h = r.get("tip_height").and_then(|v| v.as_u64()).unwrap_or(0);
                        tip_hash = r
                            .get("tip_hash")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                    }
                }
            }
            let mut seq = resolve_seq(&args.addr, &account, 0).await?;
            let mut accepted = 0u64;
            let mut last_tx = String::new();
            let mut parent = tip_hash;
            let mut height = tip_h.saturating_add(1);
            for i in 0..count {
                let hash = fd_pl::crypto::sha256_hex(
                    format!("fpl-watcher-work|{account}|{asset}|{height}|{i}").as_bytes(),
                );
                let (tx, r) = submit_body(
                    &args.addr,
                    &account,
                    "",
                    0,
                    fee,
                    seq,
                    &scheme,
                    &keys_dir,
                    network_id,
                    TxBody::RailHeader {
                        asset: asset.clone(),
                        height,
                        hash: hash.clone(),
                        parent_hash: parent.clone(),
                        merkle_root: hash.clone(),
                        raw: String::new(),
                    },
                )
                .await?;
                match r {
                    WireMsg::Ok { .. } => {
                        accepted += 1;
                        last_tx = tx.tx_id;
                        seq += 1;
                        parent = hash;
                        height += 1;
                    }
                    WireMsg::Err { msg } => bail!(
                        "header {height} rejected after {accepted} accepted: {msg}"
                    ),
                    other => bail!("unexpected submit reply: {other:?}"),
                }
            }
            println!(
                "watcher-work account={account} asset={asset} accepted={accepted}/{count} from_height={} last_tx={last_tx}",
                tip_h + 1
            );
        }
        Cmd::Tick { dt_ms } => {
            let r = rpc_admin(&args.addr, &args.admin_sock, WireMsg::Tick { dt_ms }).await?;
            print_reply(&r)?;
        }
        Cmd::ForceSettle => {
            let r = rpc_admin(&args.addr, &args.admin_sock, WireMsg::ForceSettleEpoch).await?;
            print_reply(&r)?;
        }
        Cmd::HardReset { reason } => {
            let r = rpc_admin(
                &args.addr,
                &args.admin_sock,
                WireMsg::OperatorHardReset { reason },
            )
            .await?;
            print_reply(&r)?;
        }
        Cmd::RailHeader {
            asset,
            height,
            hash,
            parent_hash,
            merkle_root,
            watcher,
            raw,
        } => {
            let r = rpc_admin(
                &args.addr,
                &args.admin_sock,
                WireMsg::RailHeader {
                    asset,
                    height,
                    hash,
                    parent_hash,
                    merkle_root,
                    watcher,
                    raw,
                },
            )
            .await?;
            print_reply(&r)?;
        }
        Cmd::SubmitBtcHeader {
            account,
            asset,
            raw,
            height,
            fee,
            scheme,
            keys_dir,
            network_id,
            sequence,
        } => {
            let parsed = fd_pl::btc_spv::parse_header_hex(&raw).map_err(|e| anyhow::anyhow!(e))?;
            if !fd_pl::btc_spv::hash_meets_target(&parsed.hash_internal, parsed.bits) {
                bail!("header does not meet target");
            }
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                "",
                0,
                fee,
                sequence,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::RailHeader {
                    asset: asset.clone(),
                    height,
                    hash: parsed.hash_display.clone(),
                    parent_hash: parsed.prev_display.clone(),
                    merkle_root: parsed.merkle_display.clone(),
                    raw: parsed.raw_hex,
                },
            )
            .await?;
            println!(
                "submit-btc-header tx={} hash={} height={height} {}",
                tx.tx_id,
                parsed.hash_display,
                serde_json::to_string_pretty(&r)?
            );
        }
        Cmd::SubmitBtcBackfill {
            account,
            file,
            fee,
            scheme,
            keys_dir,
            network_id,
            sequence,
        } => {
            let raw = std::fs::read_to_string(&file)?;
            let headers: Vec<fd_pl::bridge::RailHeader> =
                serde_json::from_str(&raw).map_err(|e| anyhow::anyhow!(e))?;
            if headers.is_empty() {
                bail!("backfill file is empty");
            }
            let headers_json = serde_json::to_string(&headers)?;
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                "",
                0,
                fee,
                sequence,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::BtcHeaderBackfill { headers_json },
            )
            .await?;
            let lo = headers.iter().map(|h| h.height).min().unwrap_or(0);
            let hi = headers.iter().map(|h| h.height).max().unwrap_or(0);
            println!(
                "submit-btc-backfill tx={} heights={lo}..{hi} n={} {}",
                tx.tx_id,
                headers.len(),
                serde_json::to_string_pretty(&r)?
            );
        }
        Cmd::SubmitEvmHeader {
            account,
            asset,
            height,
            hash,
            parent_hash,
            merkle_root,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let asset_u = asset.to_ascii_uppercase();
            if asset_u != "ETH" && asset_u != "USDC" && asset_u != "BNB" {
                bail!("submit-evm-header asset must be ETH, USDC, or BNB");
            }
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::RailHeader {
                    asset: asset_u,
                    height,
                    hash: hash.clone(),
                    parent_hash,
                    merkle_root,
                    raw: String::new(),
                },
            )
            .await?;
            println!(
                "submit-evm-header tx={} hash={} height={height} {}",
                tx.tx_id,
                hash,
                serde_json::to_string_pretty(&r)?
            );
        }
        Cmd::RailDeposit {
            asset,
            to,
            amount,
            external_txid,
            block_hash,
            height,
            lock_id,
            merkle_root,
            watcher,
            test_fixture,
            raw_tx,
            merkle_path,
            merkle_index,
            token,
        } => {
            let proof = if test_fixture {
                let lock = if lock_id.is_empty() {
                    fd_pl::bridge::genesis_lock_id(&asset)
                } else {
                    lock_id
                };
                let txid = if external_txid.is_empty() {
                    format!("test-tx-{}-{}", to, amount)
                } else {
                    external_txid
                };
                let bh = if block_hash.is_empty() {
                    format!("block-{height}")
                } else {
                    block_hash
                };
                test_deposit_proof(&txid, amount, &lock, height, &bh)
            } else {
                let path: Vec<String> = if merkle_path.is_empty() {
                    vec![]
                } else {
                    merkle_path
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .collect()
                };
                BridgeProof {
                    external_txid,
                    block_hash,
                    height,
                    merkle_path: path,
                    merkle_index,
                    lock_id,
                    parent_hash: String::new(),
                    merkle_root,
                    external_to: token,
                    raw_tx,
                }
            };
            let r = rpc_admin(
                &args.addr,
                &args.admin_sock,
                WireMsg::RailDeposit {
                    asset,
                    to,
                    amount,
                    proof,
                    watcher,
                },
            )
            .await?;
            print_reply(&r)?;
        }
        Cmd::RailWithdraw {
            asset,
            from,
            amount,
            external_to,
            vault_sig,
            signed_btc_tx,
        } => {
            let r = rpc_admin(
                &args.addr,
                &args.admin_sock,
                WireMsg::RailWithdraw {
                    asset,
                    from,
                    amount,
                    external_to,
                    vault_sig,
                    signed_btc_tx,
                },
            )
            .await?;
            print_reply(&r)?;
        }
        Cmd::RailBitvm2Attest {
            note_id,
            operator,
            pegout_txid_hex,
            claim_txid_hex,
            proof_hex,
            statement_digest_hex,
            public_hex,
        } => {
            let pubs: Vec<String> = public_hex
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect();
            let r = rpc_admin(
                &args.addr,
                &args.admin_sock,
                WireMsg::RailBitvm2Attest {
                    note_id,
                    operator,
                    pegout_txid_hex,
                    claim_txid_hex,
                    proof_hex,
                    public_hex: pubs,
                    statement_digest_hex,
                },
            )
            .await?;
            print_reply(&r)?;
        }
        Cmd::RailBitvm2Pegout {
            note_id,
            operator,
            pegout_txid_hex,
        } => {
            let r = rpc_admin(
                &args.addr,
                &args.admin_sock,
                WireMsg::RailBitvm2Pegout {
                    note_id,
                    operator,
                    pegout_txid_hex,
                },
            )
            .await?;
            print_reply(&r)?;
        }
        Cmd::BtcDepositTx {
            privhex_file,
            prev_txid,
            vout,
            sats,
            dest_account,
            vault_spk,
            prev_spk,
            fee,
        } => {
            let hex_s = std::fs::read_to_string(&privhex_file)?.trim().to_string();
            let raw = hex::decode(hex_s.trim()).context("privhex")?;
            if raw.len() != 32 {
                bail!("privhex must be 32 bytes");
            }
            let mut secret = [0u8; 32];
            secret.copy_from_slice(&raw);
            let vault = hex::decode(vault_spk.trim()).context("vault_spk")?;
            let prev = prev_spk_for_privhex(&privhex_file, &prev_spk, &secret)?;
            let tx = fd_pl::vault::sign_user_deposit_tx(
                &secret,
                &prev_txid,
                vout,
                sats,
                &prev,
                &vault,
                &dest_account,
                None,
                fee,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            secret.fill(0);
            println!("{tx}");
        }
        Cmd::BtcUserVaultAddress {
            privhex_file,
            account,
            seed_file,
            nonce_hex,
        } => {
            let secret = load_secret32(&privhex_file, "user secret")?;
            let seed = load_secret32(&seed_file, "take seed")?;
            let pk = fd_pl::vault::pubkey_from_secret(&secret).map_err(|e| anyhow::anyhow!(e))?;
            let h160 = fd_pl::vault::hash160(&pk);
            let nonce = if nonce_hex.trim().is_empty() {
                fd_pl::bitvm::sha256_32(account.as_bytes())
            } else {
                let b = hex::decode(nonce_hex.trim())?;
                if b.len() != 32 {
                    bail!("nonce 32 bytes");
                }
                let mut n = [0u8; 32];
                n.copy_from_slice(&b);
                n
            };
            let v = fd_pl::user_vault::build_for_deposit(
                &seed,
                &account,
                &h160,
                &nonce,
                fd_pl::user_vault::USER_VAULT_CSV,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "address": fd_pl::user_vault::testnet_address(&v).map_err(|e| anyhow::anyhow!(e))?,
                    "spk": hex::encode(fd_pl::user_vault::spk(&v).map_err(|e| anyhow::anyhow!(e))?),
                    "take_commit": hex::encode(v.take_commit),
                    "user_h160": hex::encode(v.user_h160),
                    "nonce": hex::encode(v.nonce),
                    "csv": v.csv_blocks,
                    "watch_key": "none",
                    "n_of_n": "none",
                    "operator_front": "none",
                    "spend": "user CHECKSIG + take_preimage (after burn) OR CSV refund (user CHECKSIG)",
                }))?
            );
        }
        Cmd::BtcUserVaultDeposit {
            privhex_file,
            prev_txid,
            vout,
            sats,
            account,
            seed_file,
            prev_spk,
            nonce_hex,
            fee,
        } => {
            let secret = load_secret32(&privhex_file, "user secret")?;
            let seed = load_secret32(&seed_file, "take seed")?;
            let nonce = if nonce_hex.trim().is_empty() {
                fd_pl::bitvm::sha256_32(account.as_bytes())
            } else {
                let b = hex::decode(nonce_hex.trim())?;
                let mut n = [0u8; 32];
                n.copy_from_slice(&b);
                n
            };
            let prev = hex::decode(prev_spk.trim()).context("prev_spk")?;
            let (tx, v) = fd_pl::user_vault::sign_deposit(
                &secret, &prev_txid, vout, sats, &prev, &account, &nonce, &seed, fee,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!(
                "{}",
                serde_json::json!({
                    "tx": tx,
                    "address": fd_pl::user_vault::testnet_address(&v).unwrap_or_default(),
                    "take_commit": hex::encode(v.take_commit),
                })
            );
        }
        Cmd::BtcUserVaultTake {
            privhex_file,
            utxo,
            account,
            seed_file,
            dest,
            nonce_hex,
            preimage_hex,
            fee,
        } => {
            let parts: Vec<&str> = utxo.split(':').collect();
            if parts.len() != 3 {
                bail!("utxo txid:vout:sats");
            }
            let secret = load_secret32(&privhex_file, "user secret")?;
            let seed = load_secret32(&seed_file, "take seed")?;
            let pk = fd_pl::vault::pubkey_from_secret(&secret).map_err(|e| anyhow::anyhow!(e))?;
            let h160 = fd_pl::vault::hash160(&pk);
            let nonce = if nonce_hex.trim().is_empty() {
                fd_pl::bitvm::sha256_32(account.as_bytes())
            } else {
                let b = hex::decode(nonce_hex.trim())?;
                let mut n = [0u8; 32];
                n.copy_from_slice(&b);
                n
            };
            let v = fd_pl::user_vault::build_for_deposit(
                &seed,
                &account,
                &h160,
                &nonce,
                fd_pl::user_vault::USER_VAULT_CSV,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            let pre = if preimage_hex.trim().is_empty() {
                let dest20 = fd_pl::btc_spv::dest_id20(&account);
                fd_pl::user_vault::take_preimage(&seed, &dest20, &h160, &nonce)
            } else {
                let b = hex::decode(preimage_hex.trim())?;
                let mut p = [0u8; 32];
                p.copy_from_slice(&b);
                p
            };
            let (_k, dest_spk) = fd_pl::btc_tx::decode_address(&dest).map_err(|e| anyhow::anyhow!(e))?;
            let tx = fd_pl::user_vault::sign_take(
                &secret,
                &v,
                &pre,
                parts[0],
                parts[1].parse()?,
                parts[2].parse()?,
                &dest_spk,
                fee,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!("{tx}");
        }
        Cmd::BtcUserVaultRefund {
            privhex_file,
            utxo,
            account,
            seed_file,
            dest,
            nonce_hex,
            fee,
        } => {
            let parts: Vec<&str> = utxo.split(':').collect();
            if parts.len() != 3 {
                bail!("utxo txid:vout:sats");
            }
            let secret = load_secret32(&privhex_file, "user secret")?;
            let seed = load_secret32(&seed_file, "take seed")?;
            let pk = fd_pl::vault::pubkey_from_secret(&secret).map_err(|e| anyhow::anyhow!(e))?;
            let h160 = fd_pl::vault::hash160(&pk);
            let nonce = if nonce_hex.trim().is_empty() {
                fd_pl::bitvm::sha256_32(account.as_bytes())
            } else {
                let b = hex::decode(nonce_hex.trim())?;
                let mut n = [0u8; 32];
                n.copy_from_slice(&b);
                n
            };
            let v = fd_pl::user_vault::build_for_deposit(
                &seed,
                &account,
                &h160,
                &nonce,
                fd_pl::user_vault::USER_VAULT_CSV,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            let (_k, dest_spk) = fd_pl::btc_tx::decode_address(&dest).map_err(|e| anyhow::anyhow!(e))?;
            let tx = fd_pl::user_vault::sign_refund(
                &secret,
                &v,
                parts[0],
                parts[1].parse()?,
                parts[2].parse()?,
                &dest_spk,
                fee,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!("{tx}");
        }
        Cmd::BtcFbtcPoolCeremony { keys_dir, csv } => {
            let meta = fd_pl::fbtc_pool::ceremony(&keys_dir, csv).map_err(|e| anyhow::anyhow!(e))?;
            println!("{}", serde_json::to_string_pretty(&meta)?);
        }
        Cmd::BtcFbtcPoolAddress {
            keys_dir,
            abort_commit_hex,
            abort_xonly,
            csv,
        } => {
            let p = if keys_dir.as_os_str().is_empty() {
                fd_pl::fbtc_pool::from_hexes(&abort_commit_hex, &abort_xonly, csv)
                    .map_err(|e| anyhow::anyhow!(e))?
            } else {
                fd_pl::fbtc_pool::load_pool_json(&keys_dir.join("pool.json"))
                    .map_err(|e| anyhow::anyhow!(e))?
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&fd_pl::fbtc_pool::pool_json(&p).map_err(|e| anyhow::anyhow!(e))?)?
            );
        }
        Cmd::BtcFbtcPoolDeposit {
            privhex_file,
            prev_txid,
            vout,
            sats,
            account,
            keys_dir,
            prev_spk,
            fee,
        } => {
            let secret = load_secret32(&privhex_file, "user secret")?;
            let p = fd_pl::fbtc_pool::load_pool_json(&keys_dir.join("pool.json"))
                .map_err(|e| anyhow::anyhow!(e))?;
            let prev = prev_spk_for_privhex(&privhex_file, &prev_spk, &secret)?;
            let tx = fd_pl::fbtc_pool::sign_deposit(
                &secret, &prev_txid, vout, sats, &prev, &p, &account, fee,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!("{tx}");
        }
        Cmd::BtcFbtcPoolPegout {
            privhex_file,
            utxo,
            dest,
            amount,
            keys_dir,
            fee,
        } => {
            let secret = load_secret32(&privhex_file, "user secret")?;
            let p = fd_pl::fbtc_pool::load_pool_json(&keys_dir.join("pool.json"))
                .map_err(|e| anyhow::anyhow!(e))?;
            let (txid, vout, sats) =
                fd_pl::fbtc_pool::parse_outpoint(&utxo).map_err(|e| anyhow::anyhow!(e))?;
            let (_k, dest_spk) = fd_pl::btc_tx::decode_address(&dest).map_err(|e| anyhow::anyhow!(e))?;
            let tx = fd_pl::fbtc_pool::sign_pegout(
                &p, &secret, &txid, vout, sats, &dest_spk, amount, fee,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            println!("{tx}");
        }
        Cmd::BtcFbtcPoolPresignAbort {
            keys_dir,
            utxo,
            fee,
            out,
        } => {
            let p = fd_pl::fbtc_pool::load_pool_json(&keys_dir.join("pool.json"))
                .map_err(|e| anyhow::anyhow!(e))?;
            let abort_sk = load_secret32(&keys_dir.join("abort.privhex"), "abort secret")?;
            let (txid, vout, sats) =
                fd_pl::fbtc_pool::parse_outpoint(&utxo).map_err(|e| anyhow::anyhow!(e))?;
            let presign = fd_pl::fbtc_pool::presign_abort(&p, &abort_sk, &txid, vout, sats, fee)
                .map_err(|e| anyhow::anyhow!(e))?;
            let json = serde_json::to_string_pretty(&presign)?;
            let dest = out.unwrap_or_else(|| keys_dir.join("abort.presign.json"));
            std::fs::write(&dest, json.as_bytes())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600));
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "wrote": dest.display().to_string(),
                    "pool_address": presign.pool_address,
                    "return_sats": presign.return_sats,
                    "txid": presign.txid,
                    "vout": presign.vout,
                    "note": "outputs bound to pool script; abort key cannot steal",
                }))?
            );
        }
        Cmd::BtcFbtcPoolAbort {
            keys_dir,
            presign_json,
            preimage_hex,
        } => {
            let p = fd_pl::fbtc_pool::load_pool_json(&keys_dir.join("pool.json"))
                .map_err(|e| anyhow::anyhow!(e))?;
            let presign_path = presign_json.unwrap_or_else(|| keys_dir.join("abort.presign.json"));
            let presign =
                fd_pl::fbtc_pool::load_presign(&presign_path).map_err(|e| anyhow::anyhow!(e))?;
            let pre = if preimage_hex.trim().is_empty() {
                load_secret32(&keys_dir.join("abort.preimage"), "abort preimage")?
            } else {
                let b = hex::decode(preimage_hex.trim())?;
                if b.len() != 32 {
                    bail!("preimage 32 bytes");
                }
                let mut o = [0u8; 32];
                o.copy_from_slice(&b);
                o
            };
            let tx = fd_pl::fbtc_pool::assemble_abort(&p, &presign, &pre)
                .map_err(|e| anyhow::anyhow!(e))?;
            println!("{tx}");
        }
        Cmd::BtcPayTx {
            privhex_file,
            prev_txid,
            vout,
            sats,
            dest,
            amount,
            prev_spk,
            fee,
        } => {
            let hex_s = std::fs::read_to_string(&privhex_file)?.trim().to_string();
            let raw = hex::decode(hex_s.trim()).context("privhex")?;
            if raw.len() != 32 {
                bail!("privhex must be 32 bytes");
            }
            let mut secret = [0u8; 32];
            secret.copy_from_slice(&raw);
            let prev = prev_spk_for_privhex(&privhex_file, &prev_spk, &secret)?;
            let tx = fd_pl::vault::sign_user_pay_tx(
                &secret,
                &prev_txid,
                vout,
                sats,
                &prev,
                &dest,
                amount,
                fee,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            secret.fill(0);
            println!("{tx}");
        }
        Cmd::BtcClaimTx {
            privhex_file,
            prev_txid,
            vout,
            sats,
            statement_digest_hex,
            prev_spk,
            fee,
        } => {
            let hex_s = std::fs::read_to_string(&privhex_file)?.trim().to_string();
            let raw = hex::decode(hex_s.trim()).context("privhex")?;
            if raw.len() != 32 {
                bail!("privhex must be 32 bytes");
            }
            let mut secret = [0u8; 32];
            secret.copy_from_slice(&raw);
            let prev = prev_spk_for_privhex(&privhex_file, &prev_spk, &secret)?;
            let tx = fd_pl::vault::sign_user_claim_tx(
                &secret,
                &prev_txid,
                vout,
                sats,
                &prev,
                &statement_digest_hex,
                fee,
            )
            .map_err(|e| anyhow::anyhow!(e))?;
            secret.fill(0);
            println!("{tx}");
        }
        Cmd::BtcSignWithdraw {
            keys_dir,
            seats,
            dest,
            amount,
            fee,
            utxo,
            vault_spk,
        } => {
            let mut shares = Vec::new();
            for seat in &seats {
                shares.push(
                    fd_pl::vault::read_share(&keys_dir, seat).map_err(|e| anyhow::anyhow!(e))?,
                );
            }
            let mut utxos = Vec::new();
            for u in &utxo {
                let parts: Vec<&str> = u.split(':').collect();
                if parts.len() != 3 {
                    bail!("utxo must be txid:vout:sats");
                }
                utxos.push(fd_pl::vault::VaultUtxo {
                    txid: parts[0].to_string(),
                    vout: parts[1].parse()?,
                    sats: parts[2].parse()?,
                    height: 0,
                });
            }
            let plan = fd_pl::vault::coin_select(&utxos, amount, &dest, fee)
                .map_err(|e| anyhow::anyhow!(e))?;
            let vsig = fd_pl::vault::sign_plan(&shares, &plan).map_err(|e| anyhow::anyhow!(e))?;
            let raw = fd_pl::vault::sign_vault_spend_tx(&shares, &plan, &vault_spk)
                .map_err(|e| anyhow::anyhow!(e))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "vault_sig": vsig,
                    "signed_btc_tx": raw,
                    "amount": plan.amount,
                    "fee": plan.fee,
                    "change": plan.change,
                    "dest": plan.dest,
                }))?
            );
        }
        Cmd::SubmitRailDeposit {
            account,
            asset,
            to,
            amount,
            external_txid,
            block_hash,
            height,
            merkle_root,
            raw_tx,
            raw_tx_file,
            merkle_path,
            merkle_path_file,
            merkle_index,
            token,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let raw_tx = if !raw_tx_file.is_empty() {
                std::fs::read_to_string(&raw_tx_file)?
                    .trim()
                    .trim_start_matches("0x")
                    .to_string()
            } else {
                raw_tx
            };
            let path: Vec<String> = if !merkle_path_file.is_empty() {
                let s = std::fs::read_to_string(&merkle_path_file)?;
                if let Ok(v) = serde_json::from_str::<Vec<String>>(&s) {
                    v
                } else {
                    s.lines()
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty())
                        .collect()
                }
            } else if merkle_path.is_empty() {
                vec![]
            } else {
                merkle_path
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect()
            };
            let proof = BridgeProof {
                external_txid,
                block_hash,
                height,
                merkle_path: path,
                merkle_index,
                lock_id: String::new(),
                parent_hash: String::new(),
                merkle_root,
                external_to: token,
                raw_tx,
            };
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                &to,
                amount,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::RailDeposit {
                    asset,
                    to: to.clone(),
                    amount,
                    proof,
                },
            )
            .await?;
            println!("submit-rail-deposit tx={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::SubmitEthV3Deposit {
            account,
            asset,
            deposit_id,
            credit_to,
            external_txid,
            block_hash,
            height,
            raw_tx,
            raw_tx_file,
            merkle_path,
            merkle_path_file,
            merkle_index,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let raw_tx = if !raw_tx_file.is_empty() {
                std::fs::read_to_string(&raw_tx_file)?
                    .trim()
                    .trim_start_matches("0x")
                    .to_string()
            } else {
                raw_tx.trim().trim_start_matches("0x").to_string()
            };
            let path: Vec<String> = if !merkle_path_file.is_empty() {
                let s = std::fs::read_to_string(&merkle_path_file)?;
                if let Ok(v) = serde_json::from_str::<Vec<String>>(&s) {
                    v
                } else {
                    s.lines()
                        .map(|l| l.trim().to_string())
                        .filter(|l| !l.is_empty())
                        .collect()
                }
            } else if merkle_path.is_empty() {
                vec![]
            } else {
                merkle_path
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect()
            };
            let proof = BridgeProof {
                external_txid,
                block_hash,
                height,
                merkle_path: path,
                merkle_index,
                lock_id: String::new(),
                parent_hash: String::new(),
                merkle_root: String::new(),
                external_to: String::new(),
                raw_tx,
            };
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                &credit_to,
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::EthV3Deposit {
                    asset,
                    deposit_id,
                    credit_to: credit_to.clone(),
                    proof,
                },
            )
            .await?;
            println!(
                "submit-eth-v3-deposit tx={} {}",
                tx.tx_id,
                serde_json::to_string_pretty(&r)?
            );
        }
        Cmd::QcLeaves { keys_dir } => {
            let mut paths: Vec<PathBuf> = std::fs::read_dir(&keys_dir)
                .with_context(|| format!("keys_dir {}", keys_dir.display()))?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.ends_with(".pub.json"))
                        .unwrap_or(false)
                })
                .collect();
            paths.sort();
            let mut rows = Vec::new();
            for path in paths {
                let raw = match std::fs::read_to_string(&path) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("skip {}: {e}", path.display());
                        continue;
                    }
                };
                let v: serde_json::Value = match serde_json::from_str(&raw) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("skip {}: {e}", path.display());
                        continue;
                    }
                };
                let pk = v.get("pk_hex").and_then(|x| x.as_str()).unwrap_or("").trim();
                if pk.is_empty() {
                    eprintln!("skip {}: no pk_hex", path.display());
                    continue;
                }
                let id = v
                    .get("id")
                    .and_then(|x| x.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        path.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("seat")
                            .trim_end_matches(".pub.json")
                            .to_string()
                    });
                let leaf = match fd_pl::qc_header::committee_leaf_hexes(&[pk.to_string()]) {
                    Ok(v) if !v.is_empty() => v[0].clone(),
                    Ok(_) => {
                        eprintln!("skip {id}: empty leaf");
                        continue;
                    }
                    Err(e) => {
                        eprintln!("skip {id}: {e}");
                        continue;
                    }
                };
                rows.push(serde_json::json!({"id": id, "leaf": leaf}));
            }
            println!("{}", serde_json::to_string_pretty(&rows)?);
        }
        Cmd::SubmitRailWithdraw {
            account,
            asset,
            amount,
            external_to,
            vault_sig,
            signed_btc_tx,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                "",
                amount,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::RailWithdraw {
                    asset,
                    amount,
                    external_to,
                    vault_sig,
                    signed_btc_tx,
                },
            )
            .await?;
            println!("submit-rail-withdraw tx={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::SubmitBtcPoolMove {
            account,
            raw_tx,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::BtcPoolMove { raw_tx },
            )
            .await?;
            println!("submit-btc-pool-move tx={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::SubmitEthLcBootstrap {
            account,
            json_file,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let json = std::fs::read_to_string(&json_file)?;
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::EthLcBootstrap { json },
            )
            .await?;
            println!("submit-eth-lc-bootstrap tx={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::SubmitEthLcUpdate {
            account,
            json_file,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let json = std::fs::read_to_string(&json_file)?;
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::EthLcUpdate { json },
            )
            .await?;
            println!("submit-eth-lc-update tx={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::SubmitEthExecHeaders {
            account,
            json_file,
            fee,
            scheme,
            keys_dir,
            network_id,
            sequence,
        } => {
            let headers_json = std::fs::read_to_string(&json_file)?;
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                "",
                0,
                fee,
                sequence,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::EthExecHeaders { headers_json },
            )
            .await?;
            println!("submit-eth-exec-headers tx={} {}", tx.tx_id, serde_json::to_string_pretty(&r)?);
        }
        Cmd::SubmitEthExecWave {
            account,
            json_file,
            fee,
            scheme,
            keys_dir,
            network_id,
            sequence,
        } => {
            submit_exec_wave(
                &args.addr,
                &account,
                &json_file,
                fee,
                sequence,
                &scheme,
                &keys_dir,
                network_id,
            )
            .await?;
        }
        Cmd::SubmitRailChallenge {
            account,
            asset,
            note_id,
            observed_dest,
            observed_amount,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::RailChallenge {
                    asset,
                    note_id,
                    observed_dest,
                    observed_amount,
                },
            )
            .await?;
            println!(
                "submit-rail-challenge tx={} {}",
                tx.tx_id,
                serde_json::to_string_pretty(&r)?
            );
        }
        Cmd::SubmitBitvm2Attest {
            account,
            note_id,
            pegout_txid_hex,
            claim_txid_hex,
            proof_hex,
            statement_digest_hex,
            public_hex,
            fee,
            scheme,
            keys_dir,
            network_id,
        } => {
            let pubs: Vec<String> = public_hex
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let (tx, r) = submit_body(
                &args.addr,
                &account,
                "",
                0,
                fee,
                0,
                &scheme,
                &keys_dir,
                network_id,
                TxBody::Bitvm2Attest {
                    note_id,
                    pegout_txid_hex,
                    claim_txid_hex,
                    proof_hex,
                    public_hex: pubs,
                    statement_digest_hex,
                },
            )
            .await?;
            println!(
                "submit-bitvm2-attest tx={} {}",
                tx.tx_id,
                serde_json::to_string_pretty(&r)?
            );
        }
        Cmd::SnapshotSave { path } => {
            let r = rpc_admin(
                &args.addr,
                &args.admin_sock,
                WireMsg::SnapshotSave {
                    path: path.display().to_string(),
                },
            )
            .await?;
            print_reply(&r)?;
        }
        Cmd::SnapshotLoad { path } => {
            let r = rpc_admin(
                &args.addr,
                &args.admin_sock,
                WireMsg::SnapshotLoad {
                    path: path.display().to_string(),
                },
            )
            .await?;
            print_reply(&r)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod dash_claim_tests {
    use super::dash::*;
    use serde_json::json;

    fn health(role: &str, pot: u64) -> Frame {
        Frame::Health(json!({"node": {"id": "nodesafe", "role": role}, "balance": {"seat_claimable": pot}}))
    }

    #[test]
    fn dest_validation() {
        assert_eq!(claim_dest_ok("  alice ").unwrap(), "alice");
        assert!(claim_dest_ok("").is_err());
        assert!(claim_dest_ok("   ").is_err());
        assert!(claim_dest_ok("al ice").is_err());
        assert!(claim_dest_ok("nodesafe#seat").is_err());
        assert!(claim_dest_ok(&"a".repeat(129)).is_err());
        assert!(claim_dest_ok("bad\u{7}").is_err());
        assert!(claim_dest_ok("scott.reynolds.123").is_ok());
    }

    #[test]
    fn target_needs_validator_with_pot() {
        assert_eq!(claim_target(&health("validator", 42)).unwrap(), ("nodesafe".into(), 42));
        assert!(claim_target(&health("observer", 42)).is_err());
        assert!(claim_target(&health("validator", 0)).is_err());
        assert!(claim_target(&Frame::Down("x".into())).is_err());
    }

    fn keys(bytes: &[u8]) -> tokio::sync::mpsc::UnboundedReceiver<u8> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        for b in bytes {
            tx.send(*b).unwrap();
        }
        rx
    }

    fn cfg(k: &str) -> ClaimCfg {
        ClaimCfg { keys_dir: k.into(), scheme: "falcon-512".into(), network_id: 2300 }
    }

    #[tokio::test]
    async fn flow_refuses_and_cancels_without_submitting() {
        // Unroutable addr: any submit attempt would fail loudly, not "cancelled".
        let addr = "127.0.0.1:1";
        let f = health("validator", 42);
        assert!(claim_flow_for_test(addr, &cfg(""), &f, &mut keys(b"")).await.contains("--keys-dir"));
        assert!(claim_flow_for_test(addr, &cfg("/k"), &health("observer", 5), &mut keys(b"")).await.contains("not a validator"));
        assert_eq!(claim_flow_for_test(addr, &cfg("/k"), &f, &mut keys(b"ab\x1b")).await, "claim cancelled");
        // Invalid (#seat) is refused, edited with backspace, then 'n' at the confirmation.
        let mut k = keys(b"x#seat\r\x7f\x7f\x7f\x7f\x7f\x7f\x7fbob\rn");
        assert_eq!(claim_flow_for_test(addr, &cfg("/k"), &f, &mut k).await, "claim cancelled");
    }

    #[tokio::test]
    async fn flow_confirmed_reports_error_without_keys() {
        let f = health("validator", 42);
        let mut k = keys(b"bob\ryq");
        let out = claim_flow_for_test("127.0.0.1:1", &cfg("/nonexistent-keys"), &f, &mut k).await;
        assert!(out.starts_with("claim failed:"), "{out}");
    }
}
