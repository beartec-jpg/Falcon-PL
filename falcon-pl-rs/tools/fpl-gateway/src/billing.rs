//! Stripe billing hooks: STUB, off by default (`[billing] enabled = false`).
//!
//! When enabled, `POST /v1/billing/stripe/webhook` verifies the
//! `Stripe-Signature` header (HMAC-SHA256 over `"{t}.{body}"`, v1 scheme,
//! replay window) and appends the subscription event to `events_path` for the
//! operator. It does NOT call the Stripe API and does NOT change any key's
//! tier automatically; that is a later step once Scott signs off the GBP
//! prices. No live Stripe objects are created by this code.

use hmac::{Hmac, Mac};
use sha2::Sha256;

#[derive(Debug, PartialEq)]
pub enum SigErr {
    Missing,
    Malformed,
    Stale,
    Mismatch,
}

pub fn verify_signature(header: &str, body: &[u8], secret: &str, now: u64, tolerance: u64) -> Result<(), SigErr> {
    let mut t: Option<u64> = None;
    let mut sigs = Vec::new();
    for part in header.split(',') {
        let (k, v) = part.split_once('=').ok_or(SigErr::Malformed)?;
        match k.trim() {
            "t" => t = Some(v.parse().map_err(|_| SigErr::Malformed)?),
            "v1" => sigs.push(hex::decode(v).map_err(|_| SigErr::Malformed)?),
            _ => {}
        }
    }
    let t = t.ok_or(SigErr::Missing)?;
    if sigs.is_empty() {
        return Err(SigErr::Missing);
    }
    if now.abs_diff(t) > tolerance {
        return Err(SigErr::Stale);
    }
    for s in sigs {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| SigErr::Malformed)?;
        mac.update(t.to_string().as_bytes());
        mac.update(b".");
        mac.update(body);
        if mac.verify_slice(&s).is_ok() {
            return Ok(());
        }
    }
    Err(SigErr::Mismatch)
}

/// Event types the stub records. Everything else is acknowledged and ignored.
pub const HANDLED_EVENTS: &[&str] = &[
    "checkout.session.completed",
    "customer.subscription.created",
    "customer.subscription.updated",
    "customer.subscription.deleted",
    "invoice.payment_failed",
];
