/// Install netsu's rustls provider before a provider-neutral TLS client is
/// built. Preserve a provider already selected by an embedding application.
pub(crate) fn ensure_rustls_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
