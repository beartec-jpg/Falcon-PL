//! Prometheus text metrics. Labels are tier/method/outcome only (no key ids, no IPs).

use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::fmt::Write;

#[derive(Default)]
pub struct Metrics {
    requests: Mutex<BTreeMap<(String, String, String), u64>>,
    upstream_errors: Mutex<BTreeMap<String, u64>>,
    latency_ms_sum: Mutex<BTreeMap<String, (u64, u64)>>,
}

impl Metrics {
    pub fn req(&self, tier: &str, method: &str, outcome: &str) {
        *self.requests.lock().entry((tier.into(), method.into(), outcome.into())).or_default() += 1;
    }
    pub fn upstream_err(&self, kind: &str) {
        *self.upstream_errors.lock().entry(kind.into()).or_default() += 1;
    }
    pub fn latency(&self, method: &str, ms: u64) {
        let mut l = self.latency_ms_sum.lock();
        let e = l.entry(method.into()).or_default();
        e.0 += ms;
        e.1 += 1;
    }
    pub fn render(&self, keys_loaded: usize) -> String {
        let mut s = String::new();
        s.push_str("# HELP fpl_gateway_requests_total Gateway requests by tier, method and outcome.\n# TYPE fpl_gateway_requests_total counter\n");
        for ((t, m, o), n) in self.requests.lock().iter() {
            let _ = writeln!(s, "fpl_gateway_requests_total{{tier=\"{t}\",method=\"{m}\",outcome=\"{o}\"}} {n}");
        }
        s.push_str("# HELP fpl_gateway_upstream_errors_total Node RPC failures.\n# TYPE fpl_gateway_upstream_errors_total counter\n");
        for (k, n) in self.upstream_errors.lock().iter() {
            let _ = writeln!(s, "fpl_gateway_upstream_errors_total{{kind=\"{k}\"}} {n}");
        }
        s.push_str("# TYPE fpl_gateway_upstream_latency_ms_sum counter\n# TYPE fpl_gateway_upstream_latency_ms_count counter\n");
        for (m, (sum, c)) in self.latency_ms_sum.lock().iter() {
            let _ = writeln!(s, "fpl_gateway_upstream_latency_ms_sum{{method=\"{m}\"}} {sum}");
            let _ = writeln!(s, "fpl_gateway_upstream_latency_ms_count{{method=\"{m}\"}} {c}");
        }
        let _ = writeln!(s, "# TYPE fpl_gateway_keys_loaded gauge\nfpl_gateway_keys_loaded {keys_loaded}");
        s
    }
}
