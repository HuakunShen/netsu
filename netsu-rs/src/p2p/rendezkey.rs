//! RendezKey: exchange an iroh ticket for a short, hand-typable code and back.
//! A listener stores its ticket and prints the returned code; a peer claims the
//! code to recover the ticket. The default deployment runs in **open mode**, so
//! storing works anonymously (no token); a token (if set) unlocks the
//! privileged tier (higher caps, no rate limit). Never store secrets — tickets
//! are short-lived connection addresses, the intended use.

use std::time::Duration;

use anyhow::{Context, bail};

/// Open-mode RendezKey instance: anonymous (tokenless) creates are accepted.
pub const DEFAULT_BASE_URL: &str = "https://rendez-key.xc.huakun.tech";
const TOKEN_ENV: &str = "NETSU_RENDEZKEY_TOKEN";
const TOKEN_ENV_FALLBACK: &str = "RENDEZKEY_TOKEN";
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// The anonymous (open-mode) create tier caps `reads` at 5 and `ttl` at 1 hour.
pub const ANON_MAX_READS: u32 = 5;
pub const ANON_MAX_TTL_SECS: u64 = 3600;

/// An optional API token (from `NETSU_RENDEZKEY_TOKEN`, else `RENDEZKEY_TOKEN`).
/// Storing works without one in open mode; a token unlocks the privileged tier.
pub fn token_from_env() -> Option<String> {
    std::env::var(TOKEN_ENV)
        .ok()
        .or_else(|| std::env::var(TOKEN_ENV_FALLBACK).ok())
        .filter(|s| !s.trim().is_empty())
}

fn http_client() -> reqwest::Client {
    crate::crypto::ensure_rustls_provider();
    reqwest::Client::new()
}

/// Store `value` (a ticket) with a `ttl` (seconds) and `reads` (max claims),
/// returning the short code. `token` is optional: `None` uses the anonymous
/// open-mode tier (its caps are enforced by the server); `Some` uses the
/// privileged tier. Anonymous `ttl`/`reads` are clamped to the anon ceilings so
/// a tokenless caller doesn't trip a 400.
pub async fn store(
    base_url: &str,
    token: Option<&str>,
    value: &str,
    ttl_secs: u64,
    reads: u32,
) -> anyhow::Result<String> {
    let (ttl_secs, reads) = match token {
        Some(_) => (ttl_secs, reads),
        None => (ttl_secs.min(ANON_MAX_TTL_SECS), reads.min(ANON_MAX_READS)),
    };
    let url = format!(
        "{}/v1/entries?ttl={}&reads={}",
        base_url.trim_end_matches('/'),
        ttl_secs,
        reads
    );
    let mut request = http_client()
        .post(&url)
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("Accept", "text/plain")
        .body(value.to_string())
        .timeout(HTTP_TIMEOUT);
    if let Some(token) = token {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let resp = request.send().await.context("rendez-key store request")?;
    let status = resp.status();
    if !status.is_success() {
        bail!("rendez-key store failed: HTTP {status}");
    }
    let code = resp.text().await.context("rendez-key store response")?;
    let code = code.trim().to_string();
    if code.is_empty() {
        bail!("rendez-key store returned an empty code");
    }
    Ok(code)
}

/// Claim `code`, returning the stored ticket string. No token required.
pub async fn claim(base_url: &str, code: &str) -> anyhow::Result<String> {
    let url = format!(
        "{}/v1/entries/{}/claim",
        base_url.trim_end_matches('/'),
        code.trim()
    );
    let resp = http_client()
        .post(&url)
        .timeout(HTTP_TIMEOUT)
        .send()
        .await
        .context("rendez-key claim request")?;
    let status = resp.status();
    if status.as_u16() == 404 {
        bail!("rendez-key code not available (invalid, expired, or already claimed)");
    }
    if !status.is_success() {
        bail!("rendez-key claim failed: HTTP {status}");
    }
    let ticket = resp.text().await.context("rendez-key claim response")?;
    Ok(ticket.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Network test: exercises the real TLS/HTTP path (no token needed — a
    // claim of an unknown code returns 404 → "not available"). Run explicitly:
    //   cargo test --features iroh --lib rendezkey -- --ignored
    #[tokio::test]
    #[ignore = "network: hits the real rendez-key server"]
    async fn claim_unknown_code_is_not_available() {
        let err = claim(DEFAULT_BASE_URL, "zzzznope99")
            .await
            .expect_err("an unknown code must not resolve");
        assert!(
            err.to_string().contains("not available"),
            "expected a 404 not-available error (TLS/HTTP worked), got: {err:#}"
        );
    }

    // Full open-mode round-trip: store anonymously (no token) → claim → recover.
    #[tokio::test]
    #[ignore = "network: hits the real rendez-key server (open mode)"]
    async fn anonymous_store_then_claim_round_trips() {
        let value = "netsu-open-mode-roundtrip-ticket";
        let code = store(DEFAULT_BASE_URL, None, value, 300, 1)
            .await
            .expect("anonymous store should succeed in open mode");
        assert!(!code.is_empty());
        let got = claim(DEFAULT_BASE_URL, &code)
            .await
            .expect("claim should recover the value");
        assert_eq!(got, value);
    }
}
