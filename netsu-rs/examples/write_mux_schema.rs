//! Emits the mux result JSON schema to stdout. Regenerate the checked-in copy:
//!   cargo run --example write_mux_schema --features iroh > schema/mux-result-v1.json
//!
//! Requires the `iroh` feature (the mux module is feature-gated).

#[cfg(feature = "iroh")]
fn main() {
    let schema = schemars::schema_for!(netsu::mux::result::MuxResult);
    println!("{}", serde_json::to_string_pretty(&schema).unwrap());
}

#[cfg(not(feature = "iroh"))]
fn main() {
    eprintln!("build with --features iroh to emit the mux schema");
    std::process::exit(1);
}
