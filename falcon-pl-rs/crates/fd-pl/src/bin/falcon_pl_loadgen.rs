// falcon-pl-loadgen — steady background Pay load for a live Falcon PL testnet.
//
// Round-robin 1-FPL transfers between dedicated test wallets, each wallet
// pinned to one seat (so its sequence chain lands in one mempool), with
// per-wallet in-flight caps + gap resync, faucet auto-refill, and back-off
// when the hub mempool keeps growing or the tip stalls.
//
// All settings come from a KEY=VALUE env file (path = $LOADGEN_CONFIG or
// argv[1]). Dynamic keys (RATE, PAUSE, MEMPOOL_MAX, STALL_SECS, AMOUNT, FEE,
// MAX_INFLIGHT, FLOOR, REFILL) are re-read every 10 s — edit the file to
// change the rate; no restart needed. `init` subcommand creates wallets.
//
//   falcon-pl-loadgen init <keys_dir> <prefix> <count>
//   falcon-pl-loadgen run  <config.env>
//
// Never prints secret keys.

use anyhow::{bail, Context, Result};
use fd_pl::crypto::{CryptoScheme, KeyRing};
use fd_pl::{Tx, WireMsg};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Semaphore};

/// `sequence stale want N got M` → N. None for any other submit error.
fn stale_want(msg: &str) -> Option<u64> {
    let rest = msg.split("sequence stale want ").nth(1)?;
    rest.split_whitespace().next()?.parse().ok()
}

fn ts() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let s = d.as_secs();
    let (h, m, sec) = ((s / 3600) % 24, (s / 60) % 60, s % 60);
    let days = s / 86400;
    // civil date from days since epoch
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let dd = doy - (153 * mp + 2) / 5 + 1;
    let mm = if mp < 10 { mp + 3 } else { mp - 9 };
    let yy = if mm <= 2 { y + 1 } else { y };
    format!("{yy:04}-{mm:02}-{dd:02}T{h:02}:{m:02}:{sec:02}Z")
}

macro_rules! log {
    ($($t:tt)*) => {{ println!("[{}] {}", ts(), format!($($t)*)); }};
}

// ---------------------------------------------------------------- config
#[derive(Clone, Debug)]
struct Cfg {
    rate: f64,
    pause: bool,
    rpcs: Vec<String>,
    status_rpc: String,
    prefix: String,
    wallets: usize,
    keys: PathBuf,
    faucet_keys: PathBuf,
    faucet_account: String,
    network_id: u32,
    amount: u64,
    fee: u64,
    floor: u64,
    refill: u64,
    mempool_max: u64,
    stall_secs: u64,
    max_inflight: u64,
    gap_secs: u64,
    workers: usize,
    report_secs: u64,
}

fn read_kv(path: &PathBuf) -> Result<BTreeMap<String, String>> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut m = BTreeMap::new();
    for line in raw.lines() {
        let l = line.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = l.split_once('=') {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            m.insert(k.trim().trim_start_matches("export ").to_string(), v.to_string());
        }
    }
    Ok(m)
}

fn load_cfg(path: &PathBuf) -> Result<Cfg> {
    let m = read_kv(path)?;
    let g = |k: &str, d: &str| m.get(k).cloned().unwrap_or_else(|| d.to_string());
    let rpcs: Vec<String> = g("LOADGEN_RPCS", "127.0.0.1:19301")
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Ok(Cfg {
        rate: g("LOADGEN_RATE", "3").parse().unwrap_or(3.0),
        pause: matches!(g("LOADGEN_PAUSE", "0").as_str(), "1" | "true" | "yes"),
        status_rpc: g("LOADGEN_STATUS_RPC", &rpcs[0]),
        rpcs,
        prefix: g("LOADGEN_PREFIX", "loadgen"),
        wallets: g("LOADGEN_WALLETS", "14").parse().unwrap_or(14),
        keys: PathBuf::from(g("LOADGEN_KEYS", "keys")),
        faucet_keys: PathBuf::from(g("LOADGEN_FAUCET_KEYS", "")),
        faucet_account: g("LOADGEN_FAUCET_ACCOUNT", "faucet"),
        network_id: g("LOADGEN_NETWORK_ID", "2300").parse().unwrap_or(2300),
        amount: g("LOADGEN_AMOUNT", "1").parse().unwrap_or(1),
        fee: g("LOADGEN_FEE", "2").parse().unwrap_or(2),
        floor: g("LOADGEN_FLOOR", "20000").parse().unwrap_or(20000),
        refill: g("LOADGEN_REFILL", "100000").parse().unwrap_or(100000),
        mempool_max: g("LOADGEN_MEMPOOL_MAX", "1500").parse().unwrap_or(1500),
        stall_secs: g("LOADGEN_STALL_SECS", "120").parse().unwrap_or(120),
        max_inflight: g("LOADGEN_MAX_INFLIGHT", "64").parse().unwrap_or(64),
        gap_secs: g("LOADGEN_GAP_SECS", "60").parse().unwrap_or(60),
        workers: g("LOADGEN_WORKERS", "32").parse().unwrap_or(32),
        report_secs: g("LOADGEN_REPORT_SECS", "30").parse().unwrap_or(30),
    })
}

// ---------------------------------------------------------------- rpc
struct RpcConn {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl RpcConn {
    async fn connect(addr: &str) -> Result<Self> {
        let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr))
            .await
            .context("connect timeout")?
            .with_context(|| format!("connect {addr}"))?;
        let _ = stream.set_nodelay(true);
        let (r, writer) = stream.into_split();
        let mut reader = BufReader::with_capacity(1 << 20, r);
        let mut line = String::new();
        let n = tokio::time::timeout(Duration::from_secs(20), reader.read_line(&mut line))
            .await
            .context("hello timeout")??;
        if n == 0 {
            bail!("eof before hello from {addr}");
        }
        Ok(Self { reader, writer })
    }

    async fn call(&mut self, msg: &WireMsg) -> Result<WireMsg> {
        let line = serde_json::to_string(msg)? + "\n";
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;
        for _ in 0..32 {
            let mut resp = String::new();
            let n = self.reader.read_line(&mut resp).await?;
            if n == 0 {
                bail!("eof from peer");
            }
            let m: WireMsg = match serde_json::from_str(&resp) {
                Ok(m) => m,
                Err(_) => continue,
            };
            match m {
                WireMsg::Hello { .. } => continue,
                other => return Ok(other),
            }
        }
        bail!("no useful response")
    }
}

struct ConnPool {
    free: Mutex<BTreeMap<String, Vec<RpcConn>>>,
}

impl ConnPool {
    async fn call(&self, addr: &str, msg: &WireMsg, timeout_s: u64) -> Result<WireMsg> {
        let mut last = None;
        for _ in 0..2 {
            let c = {
                let mut g = self.free.lock().await;
                g.get_mut(addr).and_then(|v| v.pop())
            };
            let mut conn = match c {
                Some(c) => c,
                None => match RpcConn::connect(addr).await {
                    Ok(c) => c,
                    Err(e) => {
                        last = Some(e);
                        continue;
                    }
                },
            };
            match tokio::time::timeout(Duration::from_secs(timeout_s), conn.call(msg)).await {
                Ok(Ok(r)) => {
                    let mut g = self.free.lock().await;
                    let v = g.entry(addr.to_string()).or_default();
                    if v.len() < 16 {
                        v.push(conn);
                    }
                    return Ok(r);
                }
                Ok(Err(e)) => last = Some(e),
                Err(_) => last = Some(anyhow::anyhow!("rpc timeout {addr}")),
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("rpc failed")))
    }
}

// ---------------------------------------------------------------- state
struct Wallet {
    id: String,
    seat: usize,
    next_seq: u64,
    chain_seq: u64,
    chain_seq_changed: Instant,
    balance: u64,
    known: bool,
    synced: bool,
    resync: bool,
    refill_pending_until: Option<Instant>,
}

#[derive(Default)]
struct Mon {
    tip: u64,
    tip_changed: Option<Instant>,
    mempool: u64,
    mempool_hist: VecDeque<u64>,
    min_fee: u64,
    backoff: Option<String>,
    status_ok: bool,
}

struct Shared {
    cfg: Mutex<Cfg>,
    wallets: Mutex<Vec<Wallet>>,
    mon: Mutex<Mon>,
    pool: ConnPool,
    keys: KeyRing,
    ok: AtomicU64,
    dup: AtomicU64,
    err: AtomicU64,
    skipped: AtomicU64,
    last_err: Mutex<String>,
    paused: AtomicBool,
    faucet_next: Mutex<(u64, u64, Option<Instant>)>, // (next_local, last_chain, chain_changed)
}

fn init(keys_dir: &str, prefix: &str, count: usize) -> Result<()> {
    let dir = PathBuf::from(keys_dir);
    let mut k = if dir.join("pl_keys.json").exists() {
        KeyRing::load_dir(&dir)?
    } else {
        KeyRing::new(CryptoScheme::Falcon512)
    };
    let mut made = 0;
    for i in 1..=count {
        let id = format!("{prefix}{i:02}");
        if k.public_hex(&id).is_none() {
            k.generate_id(&id);
            made += 1;
        }
    }
    k.save_dir(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        for e in std::fs::read_dir(&dir)? {
            let p = e?.path();
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
    }
    println!("init: {made} new wallets ({prefix}01..{prefix}{count:02}) in {} (mode 700/600)", dir.display());
    Ok(())
}

async fn account(sh: &Shared, addr: &str, id: &str) -> Result<(u64, u64, bool)> {
    match sh
        .pool
        .call(addr, &WireMsg::AccountQuery { account: id.to_string() }, 15)
        .await?
    {
        WireMsg::Account { body } => Ok((
            body.get("sequence").and_then(|v| v.as_u64()).unwrap_or(0),
            body.get("balance").and_then(|v| v.as_u64()).unwrap_or(0),
            body.get("exists").and_then(|v| v.as_bool()).unwrap_or(false),
        )),
        WireMsg::Err { msg } => bail!("account {id}: {msg}"),
        other => bail!("account {id}: unexpected {:?}", std::mem::discriminant(&other)),
    }
}

async fn submit(sh: &Shared, addr: &str, tx: Tx) -> Result<String> {
    match sh.pool.call(addr, &WireMsg::SubmitTx { tx }, 15).await? {
        WireMsg::Ok { msg } => Ok(msg),
        WireMsg::Err { msg } => bail!("{msg}"),
        _ => bail!("unexpected reply"),
    }
}

// Faucet → wallet refill, signed with the faucet key (slim-loaded).
async fn refill(sh: Arc<Shared>, faucet: Arc<KeyRing>, to: String) {
    let cfg = sh.cfg.lock().await.clone();
    let addr = cfg.status_rpc.clone();
    let r: Result<String> = async {
        // Serialize faucet spends and pipeline sequences locally (mempool
        // accepts future seqs; same-seq resubmits would evict each other).
        let mut fst = sh.faucet_next.lock().await;
        let (chain_seq, bal, _) = account(&sh, &addr, &cfg.faucet_account).await?;
        if chain_seq != fst.1 || fst.2.is_none() {
            fst.1 = chain_seq;
            fst.2 = Some(Instant::now());
        }
        // Local pipeline ahead of chain with no chain progress for gap_secs →
        // assume dropped faucet txs and restart from the chain sequence.
        if fst.0 > chain_seq && fst.2.map(|t| t.elapsed().as_secs() >= cfg.gap_secs.max(60)).unwrap_or(false) {
            log!("faucet seq resync: local {} -> chain {}", fst.0, chain_seq);
            fst.0 = chain_seq;
            fst.2 = Some(Instant::now());
        }
        let seq = chain_seq.max(fst.0);
        if bal < cfg.refill + 1000 {
            bail!("faucet balance {bal} too low");
        }
        let fee = cfg.fee.max(sh.mon.lock().await.min_fee);
        let fa = cfg.faucet_account.clone();
        let to2 = to.clone();
        let nid = cfg.network_id;
        let amt = cfg.refill;
        let f2 = faucet.clone();
        let tx = tokio::task::spawn_blocking(move || Tx::sign_with(&f2, &fa, seq, &to2, amt, fee, nid))
            .await?
            .map_err(|e| anyhow::anyhow!(e))?;
        let r = submit(&sh, &addr, tx).await;
        if matches!(r.as_deref(), Ok("accepted")) {
            fst.0 = seq + 1;
        }
        r.map(|m| format!("{m} seq={seq}"))
    }
    .await;
    match r {
        Ok(m) => log!("refill {to} +{} from {}: {m}", cfg.refill, cfg.faucet_account),
        Err(e) => log!("WARN refill {to} failed: {e}"),
    }
}

async fn monitor(sh: Arc<Shared>) {
    loop {
        let cfg = sh.cfg.lock().await.clone();
        let r = sh
            .pool
            .call(&cfg.status_rpc, &WireMsg::StatusReq { include_accounts: false, brief: true }, 30)
            .await;
        let mut m = sh.mon.lock().await;
        match r {
            Ok(WireMsg::Status { body }) => {
                m.status_ok = true;
                let tip = body.get("tip_height").and_then(|v| v.as_u64()).unwrap_or(0);
                let mp = body.get("mempool").and_then(|v| v.as_u64()).unwrap_or(0);
                m.min_fee = body.get("suggested_min_fee").and_then(|v| v.as_u64()).unwrap_or(1);
                if tip != m.tip || m.tip_changed.is_none() {
                    m.tip = tip;
                    m.tip_changed = Some(Instant::now());
                }
                m.mempool = mp;
                m.mempool_hist.push_back(mp);
                while m.mempool_hist.len() > 6 {
                    m.mempool_hist.pop_front();
                }
            }
            _ => {
                m.status_ok = false;
            }
        }
        let stalled = m
            .tip_changed
            .map(|t| t.elapsed().as_secs() >= cfg.stall_secs)
            .unwrap_or(false);
        // "keeps growing past max": last 3 samples all above max and non-decreasing
        let n = m.mempool_hist.len();
        let growing = n >= 3 && {
            let v: Vec<u64> = m.mempool_hist.iter().rev().take(3).cloned().collect();
            v.iter().all(|x| *x > cfg.mempool_max) && v[0] >= v[2]
        };
        let way_over = m.mempool > cfg.mempool_max * 2;
        let prev = m.backoff.clone();
        if stalled {
            m.backoff = Some(format!("tip {} unchanged {}s", m.tip, m.tip_changed.unwrap().elapsed().as_secs()));
        } else if growing || way_over {
            m.backoff = Some(format!("mempool {} > {}", m.mempool, cfg.mempool_max));
        } else if !m.status_ok && prev.is_some() {
            // keep backoff while status unreachable
        } else if prev.is_some() && m.mempool > cfg.mempool_max / 2 {
            // hysteresis: hold backoff until mempool drains below half
        } else {
            m.backoff = None;
        }
        if prev != m.backoff {
            match &m.backoff {
                Some(r) => log!("BACKOFF start: {r}"),
                None => log!("BACKOFF end: tip={} mempool={}", m.tip, m.mempool),
            }
        }
        drop(m);
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn accounts_loop(sh: Arc<Shared>, faucet: Option<Arc<KeyRing>>) {
    loop {
        let cfg = sh.cfg.lock().await.clone();
        let stalled_chain = sh.mon.lock().await.backoff.as_deref().map(|b| b.starts_with("tip")).unwrap_or(false);
        let ids: Vec<(usize, String)> = sh.wallets.lock().await.iter().enumerate().map(|(i, w)| (i, w.id.clone())).collect();
        for (i, id) in ids {
            let seat = { sh.wallets.lock().await[i].seat };
            let addr = cfg.rpcs[seat % cfg.rpcs.len()].clone();
            // Query on the hub (authoritative committed state); fall back to the seat.
            let r = match account(&sh, &cfg.status_rpc, &id).await {
                Ok(v) => Ok(v),
                Err(_) => account(&sh, &addr, &id).await,
            };
            let Ok((seq, bal, exists)) = r else { continue };
            let mut need_refill = false;
            {
                let mut ws = sh.wallets.lock().await;
                let w = &mut ws[i];
                if seq != w.chain_seq || !w.synced {
                    w.chain_seq = seq;
                    w.chain_seq_changed = Instant::now();
                }
                if !w.synced {
                    w.next_seq = seq;
                    w.synced = true;
                }
                w.balance = bal;
                w.known = exists;
                if w.next_seq < seq {
                    w.next_seq = seq;
                }
                let stuck = w.next_seq > w.chain_seq
                    && w.chain_seq_changed.elapsed().as_secs() >= cfg.gap_secs
                    && !stalled_chain;
                if w.resync || stuck {
                    if w.next_seq != w.chain_seq {
                        log!(
                            "resync {}: next_seq {} -> chain {} ({})",
                            w.id,
                            w.next_seq,
                            w.chain_seq,
                            if w.resync { "submit error" } else { "no progress, gap suspected" }
                        );
                    }
                    w.next_seq = w.chain_seq;
                    w.chain_seq_changed = Instant::now();
                    w.resync = false;
                }
                let pending_ok = w.refill_pending_until.map(|t| Instant::now() >= t).unwrap_or(true);
                if bal < cfg.floor && pending_ok {
                    w.refill_pending_until = Some(Instant::now() + Duration::from_secs(90));
                    need_refill = true;
                }
            }
            if need_refill {
                if let Some(f) = &faucet {
                    tokio::spawn(refill(sh.clone(), f.clone(), id.clone()));
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

async fn config_loop(sh: Arc<Shared>, path: PathBuf) {
    loop {
        tokio::time::sleep(Duration::from_secs(10)).await;
        match load_cfg(&path) {
            Ok(n) => {
                let mut c = sh.cfg.lock().await;
                if (n.rate - c.rate).abs() > 1e-9 || n.pause != c.pause || n.mempool_max != c.mempool_max
                    || n.max_inflight != c.max_inflight || n.amount != c.amount || n.fee != c.fee
                {
                    log!(
                        "config reload: rate {} -> {} pause {} -> {} mempool_max {} -> {} max_inflight {} -> {}",
                        c.rate, n.rate, c.pause, n.pause, c.mempool_max, n.mempool_max, c.max_inflight, n.max_inflight
                    );
                }
                // Static keys (rpcs, wallets, keys) need a restart.
                c.rate = n.rate;
                c.pause = n.pause;
                c.mempool_max = n.mempool_max;
                c.stall_secs = n.stall_secs;
                c.amount = n.amount;
                c.fee = n.fee;
                c.floor = n.floor;
                c.refill = n.refill;
                c.max_inflight = n.max_inflight;
                c.gap_secs = n.gap_secs;
                c.report_secs = n.report_secs;
            }
            Err(e) => log!("WARN config reload failed: {e}"),
        }
    }
}

async fn report_loop(sh: Arc<Shared>) {
    let mut last = Instant::now();
    let (mut lok, mut lerr, mut ldup) = (0u64, 0u64, 0u64);
    let mut ltip = 0u64;
    loop {
        let secs = sh.cfg.lock().await.report_secs.max(5);
        tokio::time::sleep(Duration::from_secs(secs)).await;
        let el = last.elapsed().as_secs_f64();
        last = Instant::now();
        let ok = sh.ok.load(Ordering::Relaxed);
        let err = sh.err.load(Ordering::Relaxed);
        let dup = sh.dup.load(Ordering::Relaxed);
        let skipped = sh.skipped.swap(0, Ordering::Relaxed);
        let cfg = sh.cfg.lock().await.clone();
        let m = sh.mon.lock().await;
        let (tip, mp, bo) = (m.tip, m.mempool, m.backoff.clone());
        drop(m);
        let ws = sh.wallets.lock().await;
        let inflight: u64 = ws.iter().map(|w| w.next_seq.saturating_sub(w.chain_seq)).sum();
        let minbal = ws.iter().map(|w| w.balance).min().unwrap_or(0);
        drop(ws);
        let le = std::mem::take(&mut *sh.last_err.lock().await);
        let blocks = if ltip > 0 { tip.saturating_sub(ltip) } else { 0 };
        log!(
            "rate_cfg={} paused={} sent_ok={} ({:.1}/s) dup={} err={} skipped_cap={} tip={} blocks={} ({:.1}/min) mempool={} inflight={} min_wallet_bal={} backoff={}{}",
            cfg.rate,
            cfg.pause,
            ok - lok,
            (ok - lok) as f64 / el,
            dup - ldup,
            err - lerr,
            skipped,
            tip,
            blocks,
            blocks as f64 * 60.0 / el,
            mp,
            inflight,
            minbal,
            bo.unwrap_or_else(|| "none".into()),
            if le.is_empty() { String::new() } else { format!(" last_err=\"{}\"", le) }
        );
        lok = ok;
        lerr = err;
        ldup = dup;
        ltip = tip;
    }
}

async fn run(cfg_path: PathBuf) -> Result<()> {
    let cfg = load_cfg(&cfg_path)?;
    let ids: Vec<String> = (1..=cfg.wallets).map(|i| format!("{}{i:02}", cfg.prefix)).collect();
    let id_refs: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
    let keys = KeyRing::load_dir_slim(&cfg.keys, &id_refs).context("load wallet keys")?;
    for id in &ids {
        if keys.public_hex(id).is_none() {
            bail!("keys dir missing wallet {id} (run init)");
        }
    }
    let faucet = if cfg.faucet_keys.as_os_str().is_empty() {
        None
    } else {
        Some(Arc::new(
            KeyRing::load_dir_slim(&cfg.faucet_keys, &[cfg.faucet_account.as_str()]).context("load faucet key")?,
        ))
    };
    log!(
        "start: wallets={} ({}01..) seats={:?} status={} rate={} tx/s faucet_refill={} floor={} refill={} mempool_max={} stall_secs={}",
        cfg.wallets, cfg.prefix, cfg.rpcs, cfg.status_rpc, cfg.rate, faucet.is_some(), cfg.floor, cfg.refill, cfg.mempool_max, cfg.stall_secs
    );
    let wallets: Vec<Wallet> = ids
        .iter()
        .enumerate()
        .map(|(i, id)| Wallet {
            id: id.clone(),
            seat: i % cfg.rpcs.len(),
            next_seq: 0,
            chain_seq: 0,
            chain_seq_changed: Instant::now(),
            balance: 0,
            known: false,
            synced: false,
            resync: false,
            refill_pending_until: None,
        })
        .collect();
    let sh = Arc::new(Shared {
        cfg: Mutex::new(cfg.clone()),
        wallets: Mutex::new(wallets),
        mon: Mutex::new(Mon { min_fee: 1, ..Default::default() }),
        pool: ConnPool { free: Mutex::new(BTreeMap::new()) },
        keys,
        ok: AtomicU64::new(0),
        dup: AtomicU64::new(0),
        err: AtomicU64::new(0),
        skipped: AtomicU64::new(0),
        last_err: Mutex::new(String::new()),
        paused: AtomicBool::new(false),
        faucet_next: Mutex::new((0, 0, None)),
    });
    tokio::spawn(monitor(sh.clone()));
    tokio::spawn(accounts_loop(sh.clone(), faucet));
    tokio::spawn(config_loop(sh.clone(), cfg_path.clone()));
    tokio::spawn(report_loop(sh.clone()));
    // wait for first account sync
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        if sh.wallets.lock().await.iter().all(|w| w.synced) {
            break;
        }
    }
    let sem = Arc::new(Semaphore::new(cfg.workers.max(1)));
    let mut rr = 0usize;
    let mut next = Instant::now();
    loop {
        let c = sh.cfg.lock().await.clone();
        let backoff = sh.mon.lock().await.backoff.clone();
        let pause = c.pause || c.rate <= 0.0 || backoff.is_some();
        if pause {
            if !sh.paused.swap(true, Ordering::Relaxed) {
                log!("submissions paused ({})", if c.pause || c.rate <= 0.0 { "config".to_string() } else { backoff.unwrap_or_default() });
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            next = Instant::now();
            continue;
        }
        if sh.paused.swap(false, Ordering::Relaxed) {
            log!("submissions resumed at {} tx/s", c.rate);
        }
        let interval = Duration::from_secs_f64(1.0 / c.rate.min(2000.0));
        let now = Instant::now();
        if next > now {
            tokio::time::sleep(next - now).await;
        }
        next += interval;
        if Instant::now() > next + Duration::from_secs(2) {
            next = Instant::now(); // don't burst after a hiccup
        }
        // pick a wallet with room
        let pick = {
            let mut ws = sh.wallets.lock().await;
            let n = ws.len();
            let mut chosen = None;
            for k in 0..n {
                let i = (rr + k) % n;
                let w = &ws[i];
                if !w.synced || w.balance < c.amount + c.fee + 10 {
                    continue;
                }
                if w.next_seq.saturating_sub(w.chain_seq) >= c.max_inflight {
                    continue;
                }
                chosen = Some(i);
                break;
            }
            match chosen {
                Some(i) => {
                    rr = i + 1;
                    let w = &mut ws[i];
                    let seq = w.next_seq;
                    w.next_seq += 1;
                    let to = ws[(i + 1) % n].id.clone();
                    Some((i, ws[i].id.clone(), to, seq, ws[i].seat))
                }
                None => None,
            }
        };
        let Some((wi, from, to, seq, seat)) = pick else {
            sh.skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let permit = match sem.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                // all workers busy: undo seq and count as skipped
                let mut ws = sh.wallets.lock().await;
                if ws[wi].next_seq == seq + 1 {
                    ws[wi].next_seq = seq;
                }
                sh.skipped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        let fee = c.fee.max(sh.mon.lock().await.min_fee);
        let addr = c.rpcs[seat % c.rpcs.len()].clone();
        let (amount, nid) = (c.amount, c.network_id);
        let sh2 = sh.clone();
        tokio::spawn(async move {
            let _p = permit;
            let sh3 = sh2.clone();
            let (f2, t2) = (from.clone(), to.clone());
            let tx = tokio::task::spawn_blocking(move || Tx::sign_with(&sh3.keys, &f2, seq, &t2, amount, fee, nid)).await;
            let res = match tx {
                Ok(Ok(tx)) => {
                    // Seat briefly behind a peer: retry the same signed tx.
                    let mut r = submit(&sh2, &addr, tx.clone()).await;
                    for _ in 0..4 {
                        let transient = match &r {
                            Ok(m) => m.contains("deferred"),
                            Err(e) => e.to_string().contains("deferred"),
                        };
                        if !transient {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(1500)).await;
                        r = submit(&sh2, &addr, tx.clone()).await;
                    }
                    r
                }
                Ok(Err(e)) => Err(anyhow::anyhow!("sign: {e}")),
                Err(e) => Err(anyhow::anyhow!("sign task: {e}")),
            };
            match res {
                Ok(m) if m == "accepted" => {
                    sh2.ok.fetch_add(1, Ordering::Relaxed);
                }
                Ok(m) if m == "duplicate" => {
                    sh2.dup.fetch_add(1, Ordering::Relaxed);
                }
                other => {
                    sh2.err.fetch_add(1, Ordering::Relaxed);
                    let msg = match other {
                        Ok(m) => m,
                        Err(e) => e.to_string(),
                    };
                    *sh2.last_err.lock().await = format!("{from}@{addr} seq {seq}: {}", msg.chars().take(160).collect::<String>());
                    // "want N got N-1" means this sequence already committed.
                    // Rewinding next_seq to the committed account sequence
                    // replays the whole in-flight window and the replays come
                    // back stale. Keep the counter, and only jump forward.
                    if let Some(want) = stale_want(&msg) {
                        let mut ws = sh2.wallets.lock().await;
                        if ws[wi].next_seq < want {
                            ws[wi].next_seq = want;
                        }
                    } else {
                        let mut ws = sh2.wallets.lock().await;
                        ws[wi].resync = true;
                    }
                }
            }
        });
    }
}

fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(|s| s.as_str()) {
        Some("init") if a.len() == 5 => init(&a[2], &a[3], a[4].parse().context("count")?),
        Some("run") if a.len() == 3 => {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build()?;
            rt.block_on(run(PathBuf::from(&a[2])))
        }
        _ => {
            eprintln!("usage: falcon-pl-loadgen init <keys_dir> <prefix> <count> | run <config.env>");
            std::process::exit(2)
        }
    }
}
