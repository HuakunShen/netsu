const VERIFY_SCRIPT: &str = include_str!("../scripts/verify.sh");

#[test]
fn direct_iroh_smoke_does_not_contact_public_rendezvous() {
    let launch = VERIFY_SCRIPT
        .lines()
        .find(|line| line.contains("server --iroh --direct-only"))
        .expect("verify script launches a direct-only Iroh server");

    assert!(
        launch.contains("--no-rendezkey"),
        "the local smoke must not wait for or consume public RendezKey capacity: {launch}"
    );
}
