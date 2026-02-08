use std::path::PathBuf;

use clap::Parser;

use nzbg_nntp_stub::{StubConfig, StubServer, load_fixtures};

#[derive(Parser, Debug)]
#[command(
    name = "nzbg-nntp-stub",
    about = "Minimal NNTP stub server for integration tests"
)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:3119")]
    bind: std::net::SocketAddr,

    #[arg(long, default_value = "fixtures/nntp/fixtures-basic.json")]
    fixtures: PathBuf,

    #[arg(long, default_value_t = false)]
    require_auth: bool,

    #[arg(long, default_value = "test")]
    username: String,

    #[arg(long, default_value = "secret")]
    password: String,

    #[arg(long, default_value_t = 0)]
    disconnect_after: usize,

    #[arg(long, default_value_t = 0)]
    delay_ms: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();
    let fixtures = load_fixtures(&args.fixtures)?;
    let config = StubConfig {
        bind: args.bind,
        require_auth: args.require_auth,
        username: args.username,
        password: args.password,
        disconnect_after: args.disconnect_after,
        delay_ms: args.delay_ms,
    };
    let server = StubServer::new(config, fixtures);
    println!("NNTP stub listening on {}", args.bind);
    server.serve().await?;
    Ok(())
}
