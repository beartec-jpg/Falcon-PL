//! Protocol account classes (v2.2) — Vault single-destination hard lock.

use serde::{Deserialize, Serialize};

/// Protocol account class. Set at open; Vault is hard-constrained after lock.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AccountType {
    #[default]
    Hot,
    Timed,
    /// Cold: outflows only to allowlist; permanent after `vault_locked`.
    Vault,
    Pool,
    LendingMarket,
}

/// Extended account fields for v2.2 account types.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AccountMeta {
    #[serde(default)]
    pub account_type: AccountType,
    /// Allowed payment destinations (Vault: max 1 for v2.2 single-send).
    #[serde(default)]
    pub allowlist: Vec<String>,
    /// Irreversible hard lock for Vault.
    #[serde(default)]
    pub vault_locked: bool,
    /// Falcon public key recorded on the first accepted payment. Later
    /// Merkle batches omit it and receivers verify against this field.
    /// Empty stays off the wire so older account bytes do not change.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub public_key: String,
    /// v2.9.65 (C-1): sha256 (hex) of the raw Falcon public key a funder
    /// committed to with `PayToKey`. Only that key may bind first. Empty stays
    /// off the wire so older account bytes do not change.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key_commit: String,
}

impl AccountMeta {
    pub const VAULT_MAX_DEST: usize = 1;

    pub fn enforce_payment(&self, destination: &str) -> Result<(), String> {
        match self.account_type {
            AccountType::Hot => Ok(()),
            AccountType::Timed => {
                if self.allowlist.is_empty() || self.allowlist.iter().any(|d| d == destination) {
                    Ok(())
                } else {
                    Err("timed account: destination not on allowlist".into())
                }
            }
            AccountType::Vault => {
                // Vaults may only pay the single nominated address. Activate
                // requires that nomination; lock makes it immutable.
                if self.allowlist.is_empty() {
                    return Err("vault: nominate a destination to activate before sending".into());
                }
                if self.allowlist.iter().any(|d| d == destination) {
                    Ok(())
                } else {
                    Err("vault: destination not on allowlist (protocol enforced)".into())
                }
            }
            AccountType::Pool | AccountType::LendingMarket => {
                Err("pool/lending: free payment forbidden — use protocol ops".into())
            }
        }
    }

    pub fn set_allowlist(&mut self, dests: Vec<String>) -> Result<(), String> {
        if self.account_type == AccountType::Vault && self.vault_locked {
            return Err("vault locked: allowlist immutable".into());
        }
        let dests: Vec<String> = dests
            .into_iter()
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty())
            .collect();
        if self.account_type == AccountType::Vault && dests.len() > Self::VAULT_MAX_DEST {
            return Err(format!(
                "vault allowlist max {} destination(s)",
                Self::VAULT_MAX_DEST
            ));
        }
        if dests.is_empty() && self.account_type == AccountType::Vault {
            return Err("vault: nominate a destination to activate".into());
        }
        self.allowlist = dests;
        Ok(())
    }

    pub fn vault_lock(&mut self) -> Result<(), String> {
        if self.account_type != AccountType::Vault {
            return Err("not a vault".into());
        }
        if self.allowlist.is_empty() {
            return Err("vault lock requires allowlist".into());
        }
        self.vault_locked = true;
        Ok(())
    }

    pub fn open_as(&mut self, t: AccountType) -> Result<(), String> {
        if self.account_type != AccountType::Hot {
            return Err("only Hot can convert account type once".into());
        }
        self.account_type = t;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vault_single_dest_enforced() {
        let mut m = AccountMeta::default();
        m.open_as(AccountType::Vault).unwrap();
        assert!(m.set_allowlist(vec![]).is_err());
        assert!(m.set_allowlist(vec!["  ".into()]).is_err());
        m.set_allowlist(vec!["cold".into()]).unwrap();
        m.vault_lock().unwrap();
        assert!(m.enforce_payment("cold").is_ok());
        assert!(m.enforce_payment("hot").is_err());
        assert!(m.set_allowlist(vec!["x".into()]).is_err());
    }

    #[test]
    fn vault_cannot_send_until_nominated() {
        let mut m = AccountMeta::default();
        m.open_as(AccountType::Vault).unwrap();
        assert!(m.enforce_payment("anyone").unwrap_err().contains("nominate"));
        assert!(m.vault_lock().is_err());
    }
}
