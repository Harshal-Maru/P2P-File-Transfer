pub mod handshake;
pub mod message;

use crate::core::torrent_info::Torrent;
use anyhow::{Context, Result};
use handshake::Handshake;
use message::Message;
use sha1::{Digest, Sha1}; // <--- Needed for verification!
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{Duration, timeout}; // <--- Needed for metadata

const BLOCK_MAX: u32 = 16384;

pub async fn run_peer_session(
    peer_addr: &str,
    info_hash: [u8; 20],
    peer_id: [u8; 20],
    torrent: &Torrent, // <--- NEW ARGUMENT
    piece_index: u32,  // <--- NEW ARGUMENT (Which piece to download)
) -> Result<Vec<u8>> {
    // <--- Returns the full piece data on success
    println!("Connecting to {}...", peer_addr);

    let mut stream = timeout(Duration::from_secs(3), TcpStream::connect(peer_addr))
        .await
        .context("Connection timed out")?
        .context(format!("Failed to connect to peer: {}", peer_addr))?;

    // --- 1. Handshake ---
    let handshake = Handshake::new(info_hash, peer_id);
    stream.write_all(&handshake.as_bytes()).await?;

    let mut response_buf = [0u8; 68];
    stream.read_exact(&mut response_buf).await?;

    if &response_buf[28..48] != info_hash {
        anyhow::bail!("Invalid Info Hash from peer");
    }
    println!("Handshake Successful.");

    // --- 2. Interested ---
    let msg = Message::Interested;
    stream.write_all(&msg.serialize()).await?;
    println!("Sent: Interested");

    // --- STATE TRACKING ---
    let mut am_unchoked = false;
    let mut peer_has_piece = false;

    // DOWNLOAD STATE
    let piece_length = torrent.info.piece_length as u32;
    let mut piece_buffer = vec![0u8; piece_length as usize]; // The Assembly Buffer
    let mut downloaded = 0u32; // How many bytes we have received
    let mut requested = 0u32; // How many bytes we have asked for

    // --- 3. Message Loop ---
    loop {
        let frame = Message::read(&mut stream).await?;

        match frame {
            Message::Choke => {
                println!("Peer Choked us");
                am_unchoked = false;
            }
            Message::Unchoke => {
                println!("Peer Unchoked us!");
                am_unchoked = true;

                // If we haven't started requesting yet, start now!
                if peer_has_piece && requested < piece_length {
                    println!("Requesting Piece {}, Block 0...", piece_index);
                    let request = Message::Request {
                        index: piece_index,
                        begin: 0,
                        length: BLOCK_MAX,
                    };
                    stream.write_all(&request.serialize()).await?;
                    requested += BLOCK_MAX;
                }
            }
            Message::Interested => {}
            Message::NotInterested => {}

            Message::Have { index } => {
                if index == piece_index {
                    peer_has_piece = true;
                    // Trigger download if unchoked and waiting
                    if am_unchoked && requested == 0 {
                        let request = Message::Request {
                            index: piece_index,
                            begin: 0,
                            length: BLOCK_MAX,
                        };
                        stream.write_all(&request.serialize()).await?;
                        requested += BLOCK_MAX;
                    }
                }
            }

            Message::Bitfield(_) => {
                // In a real client, we check the bitfield for piece_index.
                // For now, assume they have it.
                peer_has_piece = true;
                if am_unchoked && requested == 0 {
                    let request = Message::Request {
                        index: piece_index,
                        begin: 0,
                        length: BLOCK_MAX,
                    };
                    stream.write_all(&request.serialize()).await?;
                    requested += BLOCK_MAX;
                }
            }

            Message::Request { .. } => {}

            Message::Piece {
                index,
                begin,
                block,
            } => {
                // Ignore pieces we didn't ask for
                if index != piece_index {
                    continue;
                }

                let begin_usize = begin as usize;

                // Safety check to prevent buffer overflow
                if begin_usize + block.len() > piece_buffer.len() {
                    anyhow::bail!("Received block goes out of bounds!");
                }

                // COPY DATA TO BUFFER
                piece_buffer[begin_usize..begin_usize + block.len()].copy_from_slice(&block);
                downloaded += block.len() as u32;
                println!("Downloaded {}/{} bytes", downloaded, piece_length);

                // CHECK IF COMPLETE
                if downloaded == piece_length {
                    println!("Piece {} download complete. Verifying Hash...", piece_index);

                    // SHA-1 VERIFICATION
                    let mut hasher = Sha1::new();
                    hasher.update(&piece_buffer);
                    let actual_hash: [u8; 20] = hasher.finalize().into();

                    // You need to implement get_piece_hash in torrent_info.rs!
                    let expected_hash = torrent.get_piece_hash(piece_index as usize)?;

                    if actual_hash == expected_hash {
                        println!(" Hash Matched! Piece is valid.");
                        return Ok(piece_buffer);
                    } else {
                        println!("Hash Mismatch! Data corrupted.");
                        anyhow::bail!("Piece verification failed");
                    }
                }

                // PIPELINE: If not complete, Request the NEXT block immediately
                if am_unchoked && requested < piece_length {
                    // Calculate remaining size (handle last block being smaller than 16KB)
                    let remaining = piece_length - requested;
                    let block_size = std::cmp::min(BLOCK_MAX, remaining);

                    let request = Message::Request {
                        index: piece_index,
                        begin: requested,
                        length: block_size,
                    };
                    stream.write_all(&request.serialize()).await?;
                    requested += block_size;
                }
            }

            Message::KeepAlive => println!("Peer sent KeepAlive"),
        }
    }
}
