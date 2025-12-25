//! Transport layer: HTTP client, proxy selection, header rules, mirror resolution.

use crate::model::{RulesSnapshot, SettingsSnapshot};
use anyhow::Context;
use dashmap::DashMap;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::sync::Arc;
use url::Url;

#[derive(Clone)]
pub struct Transport {
  direct: reqwest::Client,
  proxy_clients: Arc<DashMap<String, reqwest::Client>>,
}

impl Transport {
  pub fn new() -> anyhow::Result<Self> {
    Ok(Self {
      direct: build_client(None)?,
      proxy_clients: Arc::new(DashMap::new()),
    })
  }

  pub fn url_hostname(url: &str) -> Option<String> {
    Url::parse(url).ok().and_then(|u| u.host_str().map(|s| s.to_string()))
  }

  pub fn client_for(&self, proxy_url: Option<&str>) -> anyhow::Result<reqwest::Client> {
    if let Some(p) = proxy_url {
      if let Some(existing) = self.proxy_clients.get(p) {
        return Ok(existing.clone());
      }
      let client = build_client(Some(p))?;
      self.proxy_clients.insert(p.to_string(), client.clone());
      return Ok(client);
    }
    Ok(self.direct.clone())
  }

  pub fn effective_proxy_url(
    settings: &SettingsSnapshot,
    rules: &RulesSnapshot,
    url: &Url,
  ) -> Option<String> {
    if !settings.global_proxy_enabled {
      return None;
    }
    let global = settings.global_proxy_url.clone().filter(|s| !s.trim().is_empty())?;
    let host = url.host_str()?.to_string();
    let best = best_pattern_match(&rules.proxy_rules.iter().filter(|r| r.enabled), &host);
    match best {
      Some(rule) if rule.use_proxy => Some(rule.proxy_url_override.clone().unwrap_or(global)),
      Some(_) => None,
      None => None, // allowlist semantics: only proxied domains use proxy
    }
  }

  pub fn apply_header_rules(rules: &RulesSnapshot, headers: &mut HeaderMap, url: &Url) {
    let host = match url.host_str() {
      Some(h) => h,
      None => return,
    };

    let best = best_pattern_match(&rules.header_rules.iter().filter(|r| r.enabled), host);
    let Some(rule) = best else { return };

    // Supported shapes:
    // - {"headers": {"User-Agent": {"value":"X", "mode":"override"}, "Referer": "Y"}}
    // - {"User-Agent": "X", "Authorization": {"value":"...", "mode":"add_if_missing"}}
    let v = &rule.headers_json;
    let obj = if let Some(h) = v.get("headers") { h } else { v };
    let Some(map) = obj.as_object() else { return };

    for (k, v) in map {
      let name = match HeaderName::from_bytes(k.as_bytes()) {
        Ok(n) => n,
        Err(_) => continue,
      };

      let (value, mode) = if let Some(s) = v.as_str() {
        (s.to_string(), "override".to_string())
      } else if let Some(o) = v.as_object() {
        let value = o.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let mode = o.get("mode").and_then(|v| v.as_str()).unwrap_or("override").to_string();
        (value, mode)
      } else {
        continue;
      };

      if value.is_empty() {
        continue;
      }
      let val = match HeaderValue::from_str(&value) {
        Ok(v) => v,
        Err(_) => continue,
      };

      match mode.as_str() {
        "add_if_missing" | "add" => {
          if !headers.contains_key(&name) {
            headers.insert(name, val);
          }
        }
        _ => {
          headers.insert(name, val);
        }
      }
    }
  }

  pub fn mirror_candidates(rules: &RulesSnapshot, url: &Url) -> Vec<Url> {
    let host = match url.host_str() {
      Some(h) => h,
      None => return vec![],
    };
    let best = best_pattern_match(&rules.mirror_rules.iter().filter(|r| r.enabled), host);
    let Some(rule) = best else { return vec![] };
    let Some(list) = rule.candidates_json.as_array() else { return vec![] };

    let mut out = Vec::new();
    for c in list {
      let Some(base) = c.as_str() else { continue };
      if let Ok(mut base_url) = Url::parse(base) {
        base_url.set_path(url.path());
        base_url.set_query(url.query());
        out.push(base_url);
      }
    }
    out
  }
}

fn build_client(proxy_url: Option<&str>) -> anyhow::Result<reqwest::Client> {
  let mut b = reqwest::Client::builder()
    .user_agent("Z-DMR/0.1")
    .redirect(reqwest::redirect::Policy::limited(10))
    .connect_timeout(std::time::Duration::from_secs(15))
    .timeout(std::time::Duration::from_secs(60));
  if let Some(p) = proxy_url {
    let proxy = reqwest::Proxy::all(p).context("invalid proxy url")?;
    b = b.proxy(proxy);
  }
  b.build().context("failed to build reqwest client")
}

fn pattern_specificity(pattern: &str) -> (u8, usize) {
  // higher is more specific
  if !pattern.contains('*') {
    (2, pattern.len())
  } else {
    // "*.example.com" => suffix "example.com"
    let suffix = pattern.trim_start_matches("*.").trim_start_matches('*');
    (1, suffix.len())
  }
}

fn pattern_matches(pattern: &str, host: &str) -> bool {
  let p = pattern.trim().to_ascii_lowercase();
  let h = host.trim().to_ascii_lowercase();
  if p.is_empty() {
    return false;
  }
  if !p.contains('*') {
    return p == h;
  }
  if let Some(suffix) = p.strip_prefix("*.") {
    return h == suffix || h.ends_with(&format!(".{suffix}"));
  }
  // Very small wildcard support: "*" means match all (not recommended, but deterministic).
  p == "*"
}

fn best_pattern_match<'a, I, T>(rules: &I, host: &str) -> Option<&'a T>
where
  I: Iterator<Item = &'a T> + Clone,
  T: PatternRule,
{
  let mut best: Option<(&T, (u8, usize))> = None;
  for r in rules.clone() {
    if !pattern_matches(r.pattern(), host) {
      continue;
    }
    let spec = pattern_specificity(r.pattern());
    if best.map(|(_, s)| spec > s).unwrap_or(true) {
      best = Some((r, spec));
    }
  }
  best.map(|(r, _)| r)
}

trait PatternRule {
  fn pattern(&self) -> &str;
}

impl PatternRule for crate::model::ProxyRule {
  fn pattern(&self) -> &str {
    &self.pattern
  }
}
impl PatternRule for crate::model::HeaderRule {
  fn pattern(&self) -> &str {
    &self.pattern
  }
}
impl PatternRule for crate::model::MirrorRule {
  fn pattern(&self) -> &str {
    &self.pattern
  }
}


