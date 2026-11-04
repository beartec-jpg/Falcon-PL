//! Per-key token buckets (rps + tx-submit/min) and monthly quotas with usage metering.

use crate::config::{Config, Tier};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug)]
pub struct Bucket {
    tokens: f64,
    cap: f64,
    rate: f64,
    last: Instant,
}

impl Bucket {
    pub fn new(rate_per_sec: f64, cap: f64, now: Instant) -> Self {
        Self { tokens: cap, cap, rate: rate_per_sec, last: now }
    }
    /// Take one token. Err(retry_after_secs) when empty.
    pub fn take(&mut self, now: Instant) -> Result<(), u64> {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * self.rate).min(self.cap);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(())
        } else {
            Err(((1.0 - self.tokens) / self.rate).ceil().max(1.0) as u64)
        }
    }
}

/// Calendar month (UTC) as "YYYY-MM" from unix seconds.
pub fn month_of(unix_secs: u64) -> String {
    // Civil-from-days (Howard Hinnant).
    let z = (unix_secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}")
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct KeyUsage {
    pub month: String,
    /// Quota units used this month (submit_tx costs more, see allowlist).
    pub units: u64,
    pub requests: u64,
    pub tx_submits: u64,
    pub rejected: u64,
}

#[derive(Debug, PartialEq)]
pub enum Deny {
    RateLimited { retry_after: u64 },
    TxRateLimited { retry_after: u64 },
    QuotaExhausted { quota: u64 },
}

struct KeyState {
    rps: Bucket,
    tx: Bucket,
    tier: Tier,
}

pub struct Limiter {
    state: Mutex<HashMap<String, KeyState>>,
    usage: Mutex<HashMap<String, KeyUsage>>,
}

impl Limiter {
    pub fn new(usage: HashMap<String, KeyUsage>) -> Self {
        Self { state: Mutex::new(HashMap::new()), usage: Mutex::new(usage) }
    }

    /// Check rate + quota for `key_id` and, if allowed, meter `cost` units.
    pub fn admit(&self, cfg: &Config, key_id: &str, tier: Tier, cost: u64, is_tx: bool, now: Instant, unix: u64) -> Result<(), Deny> {
        let l = cfg.limits(tier);
        let month = month_of(unix);
        {
            let mut u = self.usage.lock();
            let e = u.entry(key_id.to_string()).or_default();
            if e.month != month {
                *e = KeyUsage { month: month.clone(), ..Default::default() };
            }
            if e.units + cost > l.monthly_quota {
                e.rejected += 1;
                return Err(Deny::QuotaExhausted { quota: l.monthly_quota });
            }
        }
        {
            let mut st = self.state.lock();
            let s = st.entry(key_id.to_string()).or_insert_with(|| KeyState {
                rps: Bucket::new(l.rps, l.burst as f64, now),
                tx: Bucket::new(l.tx_submit_per_min as f64 / 60.0, (l.tx_submit_per_min as f64 / 6.0).max(1.0), now),
                tier,
            });
            if s.tier != tier {
                // Tier changed (key file reload): fresh buckets at the new limits.
                *s = KeyState {
                    rps: Bucket::new(l.rps, l.burst as f64, now),
                    tx: Bucket::new(l.tx_submit_per_min as f64 / 60.0, (l.tx_submit_per_min as f64 / 6.0).max(1.0), now),
                    tier,
                };
            }
            let deny = if let Err(r) = s.rps.take(now) {
                Some(Deny::RateLimited { retry_after: r })
            } else if is_tx {
                s.tx.take(now).err().map(|r| Deny::TxRateLimited { retry_after: r })
            } else {
                None
            };
            if let Some(d) = deny {
                self.usage.lock().entry(key_id.to_string()).or_default().rejected += 1;
                return Err(d);
            }
        }
        let mut u = self.usage.lock();
        let e = u.entry(key_id.to_string()).or_default();
        e.units += cost;
        e.requests += 1;
        if is_tx {
            e.tx_submits += 1;
        }
        Ok(())
    }

    pub fn usage(&self, key_id: &str) -> Option<KeyUsage> {
        self.usage.lock().get(key_id).cloned()
    }

    pub fn snapshot(&self) -> HashMap<String, KeyUsage> {
        self.usage.lock().clone()
    }

    pub fn load_usage(path: &std::path::Path) -> HashMap<String, KeyUsage> {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    /// Atomic write (tmp + rename).
    pub fn flush(&self, path: &std::path::Path) -> std::io::Result<()> {
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&self.snapshot())?)?;
        std::fs::rename(tmp, path)
    }
}

/// Single shared token bucket (gateway-wide limits).
pub struct SharedBucket(Mutex<Bucket>);

impl SharedBucket {
    pub fn new(rate: f64, burst: u32, now: Instant) -> Self {
        Self(Mutex::new(Bucket::new(rate, burst as f64, now)))
    }
    pub fn take(&self, now: Instant) -> Result<(), u64> {
        self.0.lock().take(now)
    }
}

/// Per-client token buckets with a bounded map (cleared when it grows past `max`).
pub struct ClientBuckets {
    map: Mutex<HashMap<String, Bucket>>,
    rate: f64,
    burst: f64,
    max: usize,
}

impl ClientBuckets {
    pub fn new(rate: f64, burst: u32, max: usize) -> Self {
        Self { map: Mutex::new(HashMap::new()), rate, burst: burst as f64, max }
    }
    pub fn take(&self, client: &str, now: Instant) -> Result<(), u64> {
        let mut m = self.map.lock();
        if !m.contains_key(client) && m.len() >= self.max {
            m.clear();
        }
        m.entry(client.to_string()).or_insert_with(|| Bucket::new(self.rate, self.burst, now)).take(now)
    }
    pub fn len(&self) -> usize {
        self.map.lock().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
