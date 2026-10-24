//! Account → shard routing for multi-shard GMP facade.

use serde::{Deserialize, Serialize};

/// How accounts map to physical shards (independent consensus domains).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardAssign {
    /// Contiguous faucet-style ranges: acct1..=20 → shard 0, etc.
    AccountRanges { ranges: Vec<(u32, u32, usize)> },
    /// `fnv1a(account) % n`
    FnvMod { n: usize },
}

impl Default for ShardAssign {
    fn default() -> Self {
        Self::AccountRanges {
            ranges: vec![(1, 20, 0), (21, 40, 1)],
        }
    }
}

/// One physical shard (own UNL / network / GMP).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardEndpoint {
    pub id: usize,
    pub name: String,
    /// falcon-gmpd base URL, e.g. http://127.0.0.1:18500
    pub gmp_url: String,
    /// Any validator RPC for bridge polls / emergency submit, e.g. http://192.0.2.1:18001
    pub rpc_url: String,
    /// Bridge hot wallet on this shard (auto-signed faucet account in lab).
    pub bridge_account: String,
}

#[derive(Debug, Clone)]
pub struct AccountRouter {
    pub assign: ShardAssign,
    pub shards: Vec<ShardEndpoint>,
}

impl AccountRouter {
    pub fn new(assign: ShardAssign, shards: Vec<ShardEndpoint>) -> Self {
        Self { assign, shards }
    }

    /// Parse `acctN` → N; non-matching names fall back to FNV.
    pub fn parse_acct_num(account: &str) -> Option<u32> {
        let s = account.strip_prefix("acct")?;
        s.parse().ok()
    }

    pub fn shard_id_for_account(&self, account: &str) -> usize {
        match &self.assign {
            ShardAssign::AccountRanges { ranges } => {
                if let Some(n) = Self::parse_acct_num(account) {
                    for (lo, hi, sid) in ranges {
                        if n >= *lo && n <= *hi {
                            return *sid;
                        }
                    }
                }
                // fallback: fnv
                let n = self.shards.len().max(1);
                (crate::fnv1a(account) as usize) % n
            }
            ShardAssign::FnvMod { n } => {
                let n = (*n).max(1);
                (crate::fnv1a(account) as usize) % n
            }
        }
    }

    pub fn endpoint(&self, shard_id: usize) -> Option<&ShardEndpoint> {
        self.shards.iter().find(|s| s.id == shard_id)
    }

    pub fn route_account(&self, account: &str) -> Option<&ShardEndpoint> {
        self.endpoint(self.shard_id_for_account(account))
    }

    pub fn is_cross_shard(&self, account: &str, destination: &str) -> bool {
        self.shard_id_for_account(account) != self.shard_id_for_account(destination)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lab_router() -> AccountRouter {
        AccountRouter::new(
            ShardAssign::default(),
            vec![
                ShardEndpoint {
                    id: 0,
                    name: "s0".into(),
                    gmp_url: "http://127.0.0.1:18500".into(),
                    rpc_url: "http://127.0.0.1:18001".into(),
                    bridge_account: "acct20".into(),
                },
                ShardEndpoint {
                    id: 1,
                    name: "s1".into(),
                    gmp_url: "http://127.0.0.1:18510".into(),
                    rpc_url: "http://127.0.0.1:18004".into(),
                    bridge_account: "acct40".into(),
                },
            ],
        )
    }

    #[test]
    fn ranges_route_accounts() {
        let r = lab_router();
        assert_eq!(r.shard_id_for_account("acct1"), 0);
        assert_eq!(r.shard_id_for_account("acct20"), 0);
        assert_eq!(r.shard_id_for_account("acct21"), 1);
        assert_eq!(r.shard_id_for_account("acct40"), 1);
        assert!(r.is_cross_shard("acct5", "acct25"));
        assert!(!r.is_cross_shard("acct5", "acct10"));
    }
}
