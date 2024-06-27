use clap::Parser;

#[derive(Parser, Debug, Default)]
pub struct Config {
    #[arg(long, default_value = "6610")]
    pub control_port: u16,

    #[arg(long, default_value = "6611")]
    pub http_port: u16,

    /// Domain name for the server, it could be empty,
    /// the client can't register with domain if it's empty.
    #[arg(long, default_value = "", required = false)]
    pub domain: String,
}

mod manager;
mod server;
pub use server::*;