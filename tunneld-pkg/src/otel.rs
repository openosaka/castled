use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

pub fn setup_logging() {
    // spawn the console server in the background
    let console_layer = console_subscriber::ConsoleLayer::builder()
        .with_default_env()
        .spawn();

    let filter = EnvFilter::from_default_env();

    // build a `Subscriber` by combining layers with a
    // `tracing_subscriber::Registry`:
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(filter)
        .with(console_layer)
        .init();
}
