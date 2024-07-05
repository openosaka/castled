#[cfg(feature = "otel")]
pub fn setup_logging(default_console_port: u16) {
    use std::net::Ipv4Addr;
    use tracing_subscriber::prelude::*;

    // spawn the console server in the background
    let console_layer = console_subscriber::ConsoleLayer::builder()
        .server_addr((Ipv4Addr::LOCALHOST, default_console_port))
        .with_default_env()
        .spawn();

    // build a `Subscriber` by combining layers with a
    // `tracing_subscriber::Registry`:
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(console_layer)
        .init();
}

#[cfg(not(feature = "otel"))]
pub fn setup_logging(_: u16) {
    tracing_subscriber::fmt::init();
}
