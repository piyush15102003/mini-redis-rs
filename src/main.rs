mod command;
mod handler;
mod resp;
mod store;

use std::time::Duration;

use bytes::BytesMut;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use command::Command;
use resp::Frame;
use store::Store;

/// Value following `--flag`, if present.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let port: u16 = flag(&args, "--port").and_then(|p| p.parse().ok()).unwrap_or(6379);
    let max_keys: Option<usize> = flag(&args, "--max-keys").and_then(|n| n.parse().ok());
    // Use `--host 0.0.0.0` inside Docker so other containers / the host can reach it.
    let host = flag(&args, "--host").unwrap_or_else(|| "127.0.0.1".to_string());

    let listener = TcpListener::bind((host.as_str(), port)).await.expect("bind");
    println!("mini-redis: listening on {host}:{port}");

    let store = Store::new(max_keys);

    // Active expiry: keys nobody reads again still get removed.
    let sweeper = store.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            interval.tick().await;
            sweeper.sweep();
        }
    });

    loop {
        let (socket, peer) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                eprintln!("accept failed: {e}");
                continue;
            }
        };
        println!("client connected: {peer}");
        let store = store.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(socket, store).await {
                eprintln!("client {peer}: {e}");
            }
        });
    }
}

async fn handle_connection(mut socket: TcpStream, store: Store) -> std::io::Result<()> {
    // Bytes read but not yet parsed. A frame may arrive split across reads, and one read
    // may hold many pipelined frames.
    let mut buf = BytesMut::with_capacity(4096);
    let mut out = Vec::new();

    loop {
        // Drain every complete frame, then write all replies in one go (keeps pipelining fast).
        loop {
            match resp::parse(&mut buf) {
                Ok(Some(frame)) => {
                    let reply = match Command::from_frame(frame) {
                        Ok(cmd) => handler::execute(cmd, &store),
                        Err(e) => Frame::Error(e),
                    };
                    resp::encode(&reply, &mut out);
                }
                Ok(None) => break,
                Err(e) => {
                    // Protocol garbage: report it and drop the connection. Only this client
                    // is affected.
                    resp::encode(&Frame::Error(format!("ERR protocol error: {e}")), &mut out);
                    socket.write_all(&out).await?;
                    return Ok(());
                }
            }
        }
        if !out.is_empty() {
            socket.write_all(&out).await?;
            out.clear();
        }

        if socket.read_buf(&mut buf).await? == 0 {
            return Ok(());
        }
    }
}
