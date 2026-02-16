mod binary_wrappers;
pub use binary_wrappers::*;  // Silences any dead code warnings

#[test]
fn can_start_and_connect_to_simulated_telescope() {
    let simulated_telescope = SimSalsaBin::spawn();
    println!("Port: {}", simulated_telescope.port);
    let _test_server = SalsaTestServer::spawn();
}
