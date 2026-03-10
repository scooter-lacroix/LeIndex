//! leserve binary entry point

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("LeServe - LeIndex HTTP/WebSocket Server");
    println!("Initializing...");

    let config = leserve::config::ServerConfig::from_env();

    println!("Configuration:");
    println!("  Host: {}", config.host);
    println!("  Port: {}", config.port);
    println!("  DB Path: {}", config.db_path);

    // Create server
    let server = leserve::LeServeServer::new(config)?;

    println!();
    println!("Server starting on: {}", server.server_url());
    println!("Press Ctrl+C to stop");

    // Start server
    server.start().await?;

    Ok(())
}
