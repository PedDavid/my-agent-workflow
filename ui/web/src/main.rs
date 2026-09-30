use std::net::SocketAddr;
use std::path::PathBuf;

const USAGE: &str = "usage: drove-web [--listen 127.0.0.1:7878] [--socket PATH] [--unsafe-listen]";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut listen: SocketAddr = "127.0.0.1:7878".parse()?;
    let mut socket: Option<PathBuf> = None;
    let mut allow_unsafe = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--listen" => listen = args.next().ok_or_else(|| anyhow::anyhow!(USAGE))?.parse()?,
            "--socket" => socket = args.next().map(PathBuf::from),
            "--unsafe-listen" => allow_unsafe = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other:?}\n{USAGE}"),
        }
    }
    drove_web::check_listen(&listen, allow_unsafe).map_err(|e| anyhow::anyhow!(e))?;
    let socket = socket.unwrap_or_else(drove_client::socket_path);
    let mut app = drove_web::App::new(socket.clone());
    if allow_unsafe {
        app = app.allow_any_host();
    }
    let listener = tokio::net::TcpListener::bind(listen).await?;
    eprintln!(
        "drove-web: http://{} (daemon socket {})",
        listener.local_addr()?,
        socket.display()
    );
    axum::serve(listener, drove_web::router(app)).await?;
    Ok(())
}
