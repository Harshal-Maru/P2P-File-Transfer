pub mod handshake;
pub mod message;

use tokio::net::TcpStream;
use tokio::io::{AsyncWriteExt, AsyncReadExt};
use anyhow::{Context, Result};
use handshake::Handshake;
use message::Message;

pub async fn run_peer_session(peer_addr: &str, info_hash: [u8; 20], peer_id: [u8; 20]) -> Result<()> {
    println!("Connecting to {}...", peer_addr);
    
    let mut stream = TcpStream::connect(peer_addr).await
        .context(format!("Failed to connect to peer: {}", peer_addr))?;

    // --- Step 1: Handshake ---
    let handshake = Handshake::new(info_hash, peer_id);
    let handshake_bytes = handshake.as_bytes();
    
    stream.write_all(&handshake_bytes).await?;

    let mut response_buf = [0u8; 68];
    stream.read_exact(&mut response_buf).await?;

    if &response_buf[28..48] != info_hash {
        anyhow::bail!("Invalid Info Hash from peer");
    }
    println!("✅ Handshake Successful.");

    // --- Step 2: Send "Interested" ---
    // We must tell the peer we want to download, or they will never Unchoke us.
    let msg = Message::Interested;
    stream.write_all(&msg.serialize()).await?;
    println!("Sent: Interested");

    // --- Step 3: Message Loop ---
    loop {
        // Read the next message from the stream
        let frame = Message::read(&mut stream).await?;

        match frame {
            Message::Choke => println!("Peer Choked us (Wait...)"),
            Message::Unchoke => {
                println!("✅ Peer Unchoked us! We can start requesting blocks.");
                // TODO: Here is where we would start the download logic
            },
            Message::Interested => println!("Peer is Interested."),
            Message::NotInterested => println!("Peer is Not Interested."),
            Message::Have { index } => println!("Peer has piece: {}", index),
            Message::Bitfield(bitfield) => println!("Peer sent Bitfield ({} bytes)", bitfield.len()),
            Message::Request { .. } => println!("Peer requested data (We are leeching, so ignore)"),
            Message::Piece { index, begin, block } => {
                println!("Received data! Index: {}, Offset: {}, Length: {}", index, begin, block.len());
            },
            Message::KeepAlive => println!("Peer sent KeepAlive"),
        }
    }
}