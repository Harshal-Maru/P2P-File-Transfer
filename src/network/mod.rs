pub mod handshake;

use tokio::net::TcpStream;
use tokio::io::{AsyncWriteExt, AsyncReadExt};
use anyhow::{Context, Result};
use handshake::Handshake;

pub async fn send_handshake(peer_addr: &str, info_hash: [u8; 20], peer_id: [u8; 20]) -> Result<()> {
    println!("Connecting to {}...", peer_addr);
    
    let mut stream = TcpStream::connect(peer_addr).await
        .context(format!("Failed to connect to peer: {}", peer_addr))?;

    println!("Connected! Sending Handshake...");

    let handshake = Handshake::new(info_hash, peer_id);
    let handshake_bytes = handshake.as_bytes();
    
    stream.write_all(&handshake_bytes).await
        .context("Failed to write handshake to socket")?;

    let mut response_buf = [0u8; 68];
    stream.read_exact(&mut response_buf).await
        .context("Failed to read handshake response")?;

    println!("Peer Handshaked Back!");
    
    let received_info_hash = &response_buf[28..48];
    if received_info_hash != info_hash {
        println!("Warning: Peer sent wrong Info Hash!");
    } else {
        println!("Info Hash Verified.");
    }

    Ok(())
}