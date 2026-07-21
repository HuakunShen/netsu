//! RendezKey: exchange an iroh ticket for a short, hand-typable code and back.
//! A listener stores its ticket (needs the API token) and prints the returned
//! code; a peer claims the code (no token) to recover the ticket. See the
//! `rendez-key` service docs. Never store secrets — tickets are short-lived
//! connection addresses, the intended use.

use std::time::Duration;

use anyhow::{Context, bail};

pub const DEFAULT_BASE_URL: &str = "https://rendez-key.huakun.workers.dev";
const TOKEN_ENV: &str = "NETSU_RENDEZKEY_TOKEN";
const TOKEN_ENV_FALLBACK: &str = "RENDEZKEY_TOKEN";
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// The API token for storing (from `NETSU_RENDEZKEY_TOKEN`, else
/// `RENDEZKEY_TOKEN`). Claiming needs no token, so this is `None`-tolerant.
pub fn token_from_env() -> Option<String> {
    std::env::var(TOKEN_ENV)
        .ok()
        .or_else(|| std::env::var(TOKEN_ENV_FALLBACK).ok())
        .filter(|s| !s.trim().is_empty())
}

/// Store `value` (a ticket) with a `ttl` (seconds) and `reads` (max claims),
/// returning the short code. Requires `token`.
pub async fn store(
    base_url: &str,
    token: &str,
    value: &str,
    ttl_secs: u64,
    reads: u32,
) -> anyhow::Result<String> {
    let url = format!(
        "{}/v1/entries?ttl={}&reads={}",
        base_url.trim_end_matches('/'),
        ttl_secs,
        reads
    );
    let resp = reqwest::Client::new()
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "text/plain; charset=utf-8")
        .header("Accept", "text/plain")
        .body(value.to_string())
        .timeout(HTTP_TIMEOUT)
        .send()
        .await
        .context("rendez-key store request")?;
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
    let resp = reqwest::Client::new()
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
}
