use fpl_gateway::auth::{self, hash_key, KeyRecord};
use fpl_gateway::config::{Config, Tier};
use fpl_gateway::portal::{self, Sources};
use serde_json::json;

fn portal_body(keys: serde_json::Value) -> String {
    json!({ "keys": keys }).to_string()
}

const PK: &str = "fplk_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const PK2: &str = "fplk_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn tmp_keys(recs: &[KeyRecord]) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("fplgw-keys-{}-{}.json", std::process::id(), rand::random::<u64>()));
    std::fs::write(&p, serde_json::to_string(recs).unwrap()).unwrap();
    p
}

#[test]
fn portal_hashed_key_authenticates_with_tier_and_customer() {
    let body = portal_body(json!([{ "key_id": "fck_1", "sha256": hash_key(PK).to_uppercase(), "tier": "builder", "stripe_customer": "cus_X", "disabled": false }]));
    let recs = portal::parse(&body).unwrap();
    let ks = auth::KeyStore::merged(vec![], &recs).unwrap();
    let r = ks.authenticate(PK).expect("portal key auths");
    assert_eq!((r.id.as_str(), r.tier, r.stripe_customer.as_deref()), ("fck_1", Tier::Builder, Some("cus_X")));
    assert!(ks.authenticate(PK2).is_none());
}

#[test]
fn portal_disabled_and_bad_rows_rejected() {
    let body = portal_body(json!([
        { "key_id": "off", "sha256": hash_key(PK), "tier": "scale", "disabled": true },
        { "key_id": "notier", "sha256": hash_key(PK2), "tier": null },
        { "key_id": "badhash", "sha256": "zz", "tier": "free" },
        { "key_id": "weird", "sha256": hash_key("fplk_c"), "tier": "platinum" }
    ]));
    let recs = portal::parse(&body).unwrap();
    let ks = auth::KeyStore::merged(vec![], &recs).unwrap();
    assert!(ks.authenticate(PK).is_none(), "disabled");
    assert!(ks.authenticate(PK2).is_none(), "null tier skipped");
    assert!(ks.authenticate("fplk_c").is_none(), "unknown tier skipped");
}

#[test]
fn local_keys_win_over_portal_on_id_or_hash() {
    let (lk, mut local) = auth::generate("e2e-free-scott", Tier::Free, "ops");
    local.label = "ops".into();
    let body = portal_body(json!([
        { "key_id": "e2e-free-scott", "sha256": hash_key(PK), "tier": "scale" },
        { "key_id": "fck_dup", "sha256": local.sha256, "tier": "scale" },
        { "key_id": "fck_ok", "sha256": hash_key(PK2), "tier": "builder" }
    ]));
    let ks = auth::KeyStore::merged(vec![local], &portal::parse(&body).unwrap()).unwrap();
    let r = ks.authenticate(&lk).unwrap();
    assert_eq!((r.id.as_str(), r.tier), ("e2e-free-scott", Tier::Free));
    assert!(ks.authenticate(PK).is_none(), "portal id clash with local is dropped");
    assert_eq!(ks.authenticate(PK2).unwrap().id, "fck_ok");
    assert_eq!(ks.len(), 2);
}

#[test]
fn sources_reload_keeps_portal_snapshot_and_rereads_keys_json() {
    let (lk, local) = auth::generate("ops1", Tier::Scale, "");
    let path = tmp_keys(&[local]);
    let src = Sources::new(path.clone());
    assert_eq!(src.build().unwrap().len(), 1);
    let ks = src.set_portal(portal::parse(&portal_body(json!([{ "key_id": "fck_1", "sha256": hash_key(PK), "tier": "free" }]))).unwrap()).unwrap();
    assert!(ks.authenticate(PK).is_some() && ks.authenticate(&lk).is_some());
    // SIGHUP path: keys.json edited, portal snapshot retained.
    let (lk2, local2) = auth::generate("ops2", Tier::Free, "");
    std::fs::write(&path, serde_json::to_string(&[local2]).unwrap()).unwrap();
    let ks = src.build().unwrap();
    assert!(ks.authenticate(PK).is_some() && ks.authenticate(&lk2).is_some() && ks.authenticate(&lk).is_none());
    // Broken keys.json => error (caller keeps old set), portal snapshot untouched.
    std::fs::write(&path, "not json").unwrap();
    assert!(src.build().is_err());
    assert_eq!(src.portal_len(), 1);
    let _ = std::fs::remove_file(path);
}

#[test]
fn portal_config_defaults_off_and_validates() {
    let mut c = Config::proposed_default();
    assert!(!c.portal.enabled && !c.billing.enabled);
    c.validate().unwrap();
    c.portal.enabled = true;
    c.portal.url = "http://evil.example/keys".into();
    assert!(c.validate().is_err());
    c.portal.url = "https://www.beartec.uk/api/falcon-cloud/internal/keys".into();
    c.validate().unwrap();
    // Old gateway.toml without [portal] still parses.
    let mut v: toml::Value = toml::Value::try_from(Config::proposed_default()).unwrap();
    v.as_table_mut().unwrap().remove("portal");
    let c2: Config = toml::from_str(&toml::to_string(&v).unwrap()).unwrap();
    assert!(!c2.portal.enabled);
}
