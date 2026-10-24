> **HISTORICAL.** This file is not the live Falcon PL **2300** dest-lock specification.
> Current: [BRIDGES_2300_STATUS.md](BRIDGES_2300_STATUS.md) and [README.md](README.md).
> Falcon Ledger **1001** lock-mint is shut down.

# Falcon PL — start-to-finish implementation read-up

**From the DAG diversion, to the ordered ledger, to measured throughput vs the original Falcon ~30 tx/s wall.**

| | |
|--|--|
| **Product** | Falcon PL (Falcon Participation Ledger) |
| **Ticker** | FPL |
| **Consensus** | Falcon Consensus |
| **Signatures** | Falcon-512 (NIST PQC) |
| **This cut** | **2.9.30** · pre-public beta **network_id 2300** |
| **Not** | Falcon Ledger / qXRP (RPCA) · XRPL fork · DAG · mainnet |

This is an implementation history with **measured** numbers. It is not a security proof and not a marketing sheet. Sources: [IDENTITY.md](IDENTITY.md), [POC.md](POC.md), [HARDWARE.md](HARDWARE.md), [FEATURES_AND_TOKENOMICS.md](FEATURES_AND_TOKENOMICS.md), 2200 private soak, 2300 multi-host beta.

---

## 1. Why we left the DAG

The first research line in this monorepo was **Falcon DAG**: Narwhal-style certificate DAG + federated UNL / Bullshark-like ordering experiments (`archive/dag/`, `archive/dag-python/`). That work asked a real question: can post-quantum Falcon signatures sit under a high-fan-in mempool DAG?

It was the wrong product shape for what we needed next.

| DAG line | What broke the product path |
|----------|-----------------------------|
| Certificate DAG + later total order | Two layers of “truth.” Operators could not point at **one tip hash**. |
| Narwhal/Bullshark-class research | Hard to explain, hard to operate, easy to fork-class when peers saw different DAGs. |
| Research crates mixed with product | Version and identity drift. |
| Throughput story | Theoretical DAG TPS is not a ledger a wallet can query. |

**Decision (locked):** freeze the DAG crates. Build a **greenfield ordered ledger**. One parent, one height, one hash. Bonded seats pack. Everyone verifies. That is **Falcon Consensus** on **Falcon PL**.

The archive stays for history. It is not in the workspace. Mixing DAG code back into `fd-pl` is a product error.

---

## 2. What we built instead

A **hash-linked ledger** of signed transactions.

```
bond → lottery rank → packer seals a non-empty ledger
                 → every seat verifies Falcon-512
                 → soft / hard OK
                 → tip advances
                 → silent packer is skipped (next rank)
                 → two hashes at one height from one packer → slash / jail
```

| Piece | Rule |
|-------|------|
| Truth | One chain. Parent hash + height + state root. |
| Who packs | Deterministic lottery from tip + bonded set + certified skips. Integer `seed % total_bond`. |
| Pack | Time budget (`pack_budget_ms`, default 1000 ms). **No empty seals.** |
| Committee (2.9+) | Freeze 6 seats (1 packer + 5 checkers). Commit **4 of those 6**. |
| Crypto | Falcon-512 on txs, votes, and seals. `hmac-dev` refused without `--allow-dev-crypto`. |
| Progress | Skip-QC walks a dead packer. View-change unsticks a height. No operator hard-reset for normal liveness. |
| Economy | Epoch emission. Validators / watchers / AMM / lend pots. Claims pull. |

This is **not** RPCA and **not** a 3-second open ledger. Tip can step in a few hundred milliseconds when there is work.

---

## 3. Two Falcons (do not collapse)

| Name | What it is | Consensus | What we measured |
|------|------------|-----------|------------------|
| **Falcon Ledger (qXRP)** | Separate XRPL-fork product, network 1001 | **RPCA** | Long soak; **~30 TPS sustained** then HTTP **503** / fee escalate |
| **Falcon PL (FPL)** | This crate | **Falcon Consensus** | Submit keep-up through **500 TPS**; strain **600–800**; host **OOM ~900** |

Same **Falcon-512** signatures. Different engine. The “30 tx/s” number is the **original Falcon Ledger API wall**, not a law of Falcon signatures.

---

## 4. Implementation timeline (what actually shipped)

Condensed. Full change control is [AMENDMENTS.md](AMENDMENTS.md).

| Era | What landed |
|-----|-------------|
| **DAG research** | Narwhal-style mempool DAG. Frozen under `archive/`. |
| **PL PoC** | Lottery pack, gossip mempool, Falcon-512 dual-host. [POC.md](POC.md) (2026-08-10). |
| **2.2–2.6** | Economy, epochs, vaults/AMM/lend/rails as protocol txs. First private fleets. |
| **2.7** | Public-safety surface: registry-only keys, signed votes, mesh admin lock, unix `--admin-sock`, integer lottery, Hello product+network. |
| **2.8** | Header gossip + body pull. Light vals. |
| **2.9.0–2.9.13** | 11-val 4-of-6 committee. Unique pending. Body push so remotes are not +14 s on QC. |
| **2.9.17–2.9.18** | Epoch work from incremental counters (not a RAM body window) so payday state roots agree. |
| **2.9.21–2.9.27** | Archive join-snap (tip + 128 ledgers), exclusive snap push, no snap treadmill, certified catch-up skips per-tx Falcon verify. |
| **2.9.28** | Seat binaries: `falcon-pl-archive` / `-hub` / `-val` (val RAM chain = 128). |
| **2.9.29** | `WatcherHeartbeat` — presence slot only, zero work. |
| **2.9.30** | `WatcherCredit` disabled. Native `CreditAsset` admin-only. Public mint = epoch only. |

Private soak lived on **network_id 2200**. Public-params beta is a **new genesis 2300**. We do not flip 2200.

---

## 5. What the 2200 soak actually proved

Multi-host: host1 (6c) + host2 (6c) + droplet (2c/4 GB). Falcon-512. 11 bonded seats. Soak inject **25 tx/s** by choice (not a max). Tip often **~6–10 ledgers/s** under that load, then cooled.

### 5.1 Consensus vs communication

**After the 2.9 fork-class fixes, the long soak did not fork.**

Core seats stayed on **one tip, one hash**. What looked like “the network is broken” was:

| Symptom | Cause | Class |
|---------|--------|--------|
| Vals hundreds behind | Falcon-512 verify + fat bodies on small iron | **Hardware / catch-up** |
| Droplet 2 vals | One 2-core box cannot verify two live Falcon seats | **Hardware** |
| host2 + 5 vals | 6 cores, five verifiers — lag 700–1000 | **Hardware** |
| Snap treadmill | Feeding vals a new tip every 256 ledgers | **Protocol hole** (fixed 2.9.27) |
| Join from 0 | Needed archive **join-snap**, not residual from height 1 | **Product path** (fixed 2.9.23–25) |
| Last 2200 freeze | Dead **v12** still won lottery; hubs slightly behind; 4-of-6 QC could not form | **Liveness**, not two histories |

Skip-QC of a **live** lagging packer worked (v12 skipped, another seat packed) when the committee at the tip still had votes. An **offline** packer plus two hubs off the tip is a communication / seating problem, not a fork.

**Honest line for outsiders:** we never finished the 2200 era with a split history on the core. We did spend days on **catch-up and iron**. That is why 2300 seats are: archive + hubs on big boxes, **one val on the 2-core droplet**.

### 5.2 Catch-up as product

A new or lagging val:

1. If `tip == 0` or lag ≥ 2048 → **join-snap** (tip state + 128 ledgers) from an archive.  
2. Then **certified residual** (NeedLedgers). Catch-up applies without per-tx Falcon verify; live pack still verifies.  
3. Light val keeps **128** ledgers in RAM.

Measured: droplet v5 on 2300 **0 → 262** in a few seconds, then voted live.

---

## 6. Throughput — measured, theoretical, vs 30 tx/s

### 6.1 The 30 tx/s baseline

Falcon Ledger (qXRP) testnet load (May 2026, separate product, RPCA):

- Sustained intake about **30 TPS** then **HTTP 503** / fee escalate.  
- Ledger close about **3–3.5 s**.  
- Long soak ~71 h at **0.3–6 TPS**.

That 30 is an **API / engine wall** on the old stack, not “Falcon-512 max.”

### 6.2 Measured on Falcon PL (PoC, 2026-08-10)

Dual-host, real Falcon-512, 3 validators ([POC.md](POC.md) §6):

| Test | Result | vs 30 TPS |
|------|--------|-----------|
| Endurance ~4.3 h | 20→50→80→**150** tx/s cycles, **~885k submit / 0 err** | **5.0×** (**+400%**) at the 150 spike |
| Break ramp keep-up | **200–500 TPS** clean (0 err) | **6.7×–16.7×** (**+567% to +1,567%**) |
| Soft wall | **600–800 TPS** (lag, backlog, still sealing) | **20–27×** (**+1,900% to +2,567%**) |
| Hard wall | **~900 TPS** Linux **OOM** (2× ~7.5 GB RSS on 15 GB) | Resource, **not** a consensus lie |
| Tip clock | Mean **~0.21 s** under soak | vs ~3.5 s RPCA ≈ **17×** faster close |

**Headline comparison (honest):** same signature family, different consensus. Clean **keep-up at 500 TPS** is **16.7×** the original Falcon testnet **30 TPS** wall — **1,567% higher**.

### 6.3 Theoretical envelope (do not quote as measured)

Let:

- `V` = Falcon-512 verify time on the packer  
- `P` = `pack_budget_ms` (1 s)  
- `Q` = QC / body fan time to the committee  
- `N` = txs in the ledger (cap 128 on current beta)

Then:

```
tps_pack  ≈  N / (pack_ms + Q)
tps_verify ≈  1000 / V_ms     if one core verifies the body sequentially
```

| If | Envelope |
|----|----------|
| In-process bench (~20k verify/s) | Verify is not the wall; network and RAM are |
| Live iron note (~38 ms/tx Falcon-512 on loaded seats) | ~**26 tx/s per verify core** if that cost is sequential and dominant |
| Full 128-tx ledger every **0.2 s** tip (PoC cadence) | **640 TPS** if QC stays that fast |
| Full 128-tx ledger every **1 s** pack + small QC | **~128 TPS** |
| Observed keep-up | **500 TPS** (small/medium ledgers, 1 s budget not always full) |

**Do not say “we are 640 TPS.”** That is a full-ledger, fast-QC *if*. We **did** submit **500 TPS** with 0 errors. We **did** choose **25 TPS** on the 11-val 2200 soak to prove agree + catch-up, not to max the meter.

### 6.4 2200 / 2300 soaks (later, heavier product)

| Run | Rate | Point |
|-----|------|--------|
| 2200 11-val Falcon-512 | **25 TPS** intentional | Agree + join-snap + role binaries. ~6–10 ledgers/s then cool. |
| 2300 pre-public beta | **1 tx / 3 s** | Public params (7d epoch). Keep tip alive. Not a max. |

25 TPS on 11 Falcon-512 vals is **not** slower than the old 30 TPS wall in any interesting way — it is a **chosen** soak. The engine already cleared **500**.

### 6.5 What limits us now

1. **Falcon-512 verify** on small boxes (one val per 2-core).  
2. **QC / body** to remotes (fixed the +14.5 s remote OK in 2.9.13).  
3. **RAM history** (20k ledger cap + val 128). The 900 TPS OOM was unbounded chain.  
4. **JSON-TCP** gossip — fine for a beta, not a DHT.

---

## 7. Money and watchers (implementation, not slideware)

- **Emission:** 30 bps of remaining treasury per **claimable** epoch. 7-day epoch on 2300. Epochs 1–7 emit **0**.  
- **Watcher:** `weight = work × (slots / 168)`. Heartbeat fills a slot. **Work = accepted rail header / deposit only.** `WatcherCredit` is **disabled** (2.9.30).  
- **CreditAsset** native mint is **admin-only**. Public mint = epoch.  
- On the 30 s 2200 soak we proved the loop: 168 rail headers × presence → **1 FPL** claim (treasury already crushed by 30 s emits). On 2300 that payday waits until epoch 8.

---

## 8. Where we are (2026-08-15)

**One chain: 2300 pre-public beta.** 2200 is stopped (snaps kept, id never flipped).

| Seat | Host | Role |
|------|------|------|
| v1 | host1 | archive + hub :19301 |
| v2 | host1 | hub :19302 |
| v3 | host2 | hub :19303 |
| v4 | host2 | val :19304 |
| v5 | droplet 2-core | **one** val :19305 |

Genesis: alice, bob, carol, dave, faucet + 5 vals + `watcher-browser`. No w00–w255 farm.

Idle: alice ↔ bob, 1 Pay / 3 s. Multi-host: all five same tip / hash / state root; droplet join-snap 0→262 then live votes.

**Consensus on this beta:** lockstep at low rate on three hosts. **Not** a 24 h / adversarial / WAN proof.

---

## 9. Do we need a new white paper?

**Yes — before a loud public announce. No — not to keep soaking.**

| Document | Job | Enough for |
|----------|-----|------------|
| This read-up | Internal start→finish + numbers | Team, advisors, next engineering |
| [POC.md](POC.md) | Aug 10–11 load campaign | Citing the 500 TPS ramp |
| [IDENTITY.md](IDENTITY.md) | Names | Always |
| [FEATURES_AND_TOKENOMICS.md](FEATURES_AND_TOKENOMICS.md) | Economy design | Tokenomics discussion |
| Faucet `/whitepaper` | **Falcon Ledger** marketing | **Wrong product** — do not reuse as FPL |
| **New FPL white paper** | Outsider story: why not DAG, what Falcon Consensus is, **measured vs 30 TPS**, limits | Public beta landing, press, researchers |

A new paper should be **short and honest**:

1. Why the DAG was archived.  
2. Falcon Consensus in one page.  
3. Table: 30 TPS API wall vs **500 TPS keep-up (16.7× / +1,567%)**.  
4. 2200 lesson: lag ≠ fork; one val per 2-core.  
5. 2300 is a **pre-public beta**, not mainnet, not a BFT theorem.

Do **not** write a new paper that claims 640 TPS, 7-day watcher payday already paid, or “we never had consensus bugs.” Early 2.9 **did** have fork *classes*; they were **fixed**; the long soak after that did not fork.

Soak 2300. Write the paper when you are ready to put a URL in front of strangers.
