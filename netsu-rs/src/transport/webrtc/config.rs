use url::Url;

use crate::error::{NetsuError, Result};

pub const MAX_STUN_URLS: usize = 4;

/// Shared client/server configuration for the direct-only WebRTC transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebRtcOptions {
    /// HTTP service base, for example `http://127.0.0.1:8787/v1/signal`.
    pub signal_url: Url,
    /// Optional STUN discovery servers. TURN is deliberately not representable.
    pub stun_urls: Vec<String>,
    /// Whether diagnostics may include selected candidate addresses.
    pub include_addresses: bool,
}

impl WebRtcOptions {
    pub fn new<I, S>(signal_url: &str, stun_urls: I, include_addresses: bool) -> Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let signal_url = Url::parse(signal_url).map_err(|_| {
            NetsuError::Protocol("WebRTC signal URL must be an absolute HTTP(S) URL".into())
        })?;
        if !matches!(signal_url.scheme(), "http" | "https") || signal_url.host().is_none() {
            return Err(NetsuError::Protocol(
                "WebRTC signal URL must be an absolute HTTP(S) URL".into(),
            ));
        }

        let stun_urls = stun_urls
            .into_iter()
            .map(|value| value.as_ref().to_owned())
            .collect::<Vec<_>>();
        if stun_urls.len() > MAX_STUN_URLS {
            return Err(NetsuError::Protocol(
                "WebRTC accepts at most 4 STUN URLs".into(),
            ));
        }
        for value in &stun_urls {
            let parsed = Url::parse(value).map_err(|_| {
                NetsuError::Protocol("WebRTC ICE servers must be non-empty STUN URLs".into())
            })?;
            if parsed.scheme() != "stun" || value.len() <= "stun:".len() {
                return Err(NetsuError::Protocol(
                    "WebRTC direct-only mode accepts STUN URLs only; TURN is unsupported".into(),
                ));
            }
        }

        Ok(Self {
            signal_url,
            stun_urls,
            include_addresses,
        })
    }

    pub fn rooms_url(&self) -> Result<Url> {
        self.append_signal_path("rooms")
    }

    pub fn room_websocket_url(&self, code: &str) -> Result<Url> {
        let mut url = self.append_signal_path(&format!("rooms/{code}/ws"))?;
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme).map_err(|_| {
            NetsuError::Protocol("could not derive signaling room WebSocket URL".into())
        })?;
        Ok(url)
    }

    fn append_signal_path(&self, suffix: &str) -> Result<Url> {
        let mut base = self.signal_url.clone();
        if !base.path().ends_with('/') {
            let mut path = base.path().to_owned();
            path.push('/');
            base.set_path(&path);
        }
        base.join(suffix)
            .map_err(|_| NetsuError::Protocol("could not derive signaling service URL".into()))
    }
}
