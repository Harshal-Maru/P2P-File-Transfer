pub mod handshake;
pub mod message;

use anyhow::{Context, Result};
use handshake::Handshake;
use message::Message;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{Duration, timeout};

const BLOCK_MAX: u32 = 16384;

pub async fn run_peer_session(
    peer_addr: &str,
    info_hash: [u8; 20],
    peer_id: [u8; 20],
) -> Result<()> {
    println!("Connecting to {}...", peer_addr);

    let mut stream = timeout(Duration::from_secs(3), TcpStream::connect(peer_addr))
        .await
        .context("Connection timed out")? 
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
    let msg = Message::Interested;
    stream.write_all(&msg.serialize()).await?;
    println!("Sent: Interested");

    // --- STATE TRACKING ---
    let mut peer_has_piece = false;
    let mut am_unchoked = false; 
    // --- Step 3: Message Loop ---
    loop {
        let frame = Message::read(&mut stream).await?;

        match frame {
            Message::Choke => {
                println!("Peer Choked us");
                am_unchoked = false;
            }
            Message::Unchoke => {
                println!("✅ Peer Unchoked us!");
                am_unchoked = true;

                // If we ALREADY know they have pieces, request now.
                if peer_has_piece {
                    println!("Requesting Piece 0, Block 0...");
                    let request = Message::Request {
                        index: 0,
                        begin: 0,
                        length: BLOCK_MAX,
                    };
                    stream.write_all(&request.serialize()).await?;
                } else {
                    println!("⚠️ Peer has not announced pieces yet. Waiting for 'Have'...");
                }
            }
            Message::Interested => println!("Peer is Interested."),
            Message::NotInterested => println!("Peer is Not Interested."),
            
            Message::Have { index } => {
                println!("Peer has piece: {}", index);
                peer_has_piece = true;

                
                if am_unchoked {
                    println!("Requesting Piece {}, Block 0...", index);
                    // Just request the piece they announced (or Piece 0 for simplicity)
                    let request = Message::Request {
                        index, // Request the piece they just said they have!
                        begin: 0,
                        length: BLOCK_MAX,
                    };
                    stream.write_all(&request.serialize()).await?;
                }
            }
            
            Message::Bitfield(bitfield) => {
                println!("Peer sent Bitfield ({} bytes)", bitfield.len());
                peer_has_piece = true;
                
                
                if am_unchoked {
                    let request = Message::Request { index: 0, begin: 0, length: BLOCK_MAX };
                    stream.write_all(&request.serialize()).await?;
                }
            }
            
            Message::Request { .. } => println!("Peer requested data (ignored)"),
            
            Message::Piece { index, begin, block } => {
                println!("--------------------------------");
                println!("🎁 RECEIVED DATA!");
                println!("Piece Index: {}", index);
                println!("Block Offset: {}", begin);
                println!("Data Length: {} bytes", block.len());
                println!("--------------------------------");
                return Ok(());
            }
            Message::KeepAlive => println!("Peer sent KeepAlive"),
        }
    }
}