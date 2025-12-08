pub mod handshake;
pub mod message;

use crate::core::manager::TorrentManager;
use anyhow::{Context, Result};
use handshake::Handshake;
use message::Message;
use sha1::{Digest, Sha1};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::{Duration, timeout};

/// Maximum block size requested from peers (16KB is the standard).
const BLOCK_MAX: u32 = 16384;

/// Tracks the progress of a specific piece being downloaded by this peer.
struct PeerSessionState {
    piece_index: usize,
    piece_buffer: Vec<u8>,
    downloaded: u32,
    requested: u32,
    piece_length: u32,
}

/// Manages a single TCP connection to a peer.
///
/// This function handles the entire lifecycle:
/// 1. TCP Connect & Handshake
/// 2. Exchange of Bitfield/Have messages
/// 3. Download loop (requesting blocks and assembling pieces)
/// 4. Upload loop (responding to peer requests)
/// 5. Cleanup on disconnection
pub async fn run_peer_session(
    peer_addr: String,
    info_hash: [u8; 20],
    peer_id: [u8; 20],
    manager: Arc<Mutex<TorrentManager>>,
) -> Result<()> {
    // println!("Connecting to {}...", peer_addr);

    // Enforce a strict connection timeout to avoid hanging on dead peers
    let mut stream = timeout(Duration::from_secs(3), TcpStream::connect(&peer_addr))
        .await
        .context("Connection timed out")?
        .context(format!("Failed to connect to peer: {}", peer_addr))?;

    // --- 1. Handshake ---
    let handshake = Handshake::new(info_hash, peer_id);
    stream.write_all(&handshake.as_bytes()).await?;

    let mut response_buf = [0u8; 68];
    stream.read_exact(&mut response_buf).await?;

    // Verify the peer is serving the correct torrent
    if &response_buf[28..48] != info_hash {
        anyhow::bail!("Invalid Info Hash");
    }
    // println!("{}: Handshake Successful", peer_addr);

    // --- 2. BitTorrent Protocol Setup ---
    // Signal that we are interested in downloading
    let msg = Message::Interested;
    stream.write_all(&msg.serialize()).await?;

    // --- Session State ---
    let mut am_unchoked = false;

    // Initialize local bitfield to track what the peer has
    let piece_count = manager.lock().await.piece_status.len();
    let mut peer_has_pieces = vec![false; piece_count];

    // The current piece assignment for this worker
    let mut current_work: Option<PeerSessionState> = None;

    // --- 3. Event Loop ---
    // Wrapped in an async block to ensure cleanup runs even on error/return
    let result: Result<()> = async {
        loop {
            // Keep-Alive / Stalled Check:
            // If the peer sends nothing for 30 seconds, we assume the connection is dead.
            let frame = match timeout(Duration::from_secs(30), Message::read(&mut stream)).await {
                Ok(res) => res?, // Propagate protocol errors (e.g. malformed message)
                Err(_) => {
                    return Err(anyhow::anyhow!("Connection timed out (Stalled)"));
                }
            };

            match frame {
                Message::Choke => {
                    // println!("{}: Choked", peer_addr);
                    am_unchoked = false;
                }
                Message::Unchoke => {
                    // println!("{}: Unchoked", peer_addr);
                    am_unchoked = true;
                }
                Message::Interested => {}
                Message::NotInterested => {}

                // Update Peer Bitfield
                Message::Have { index } => {
                    if (index as usize) < peer_has_pieces.len() {
                        peer_has_pieces[index as usize] = true;
                    }
                }
                Message::Bitfield(bitfield) => {
                    for (i, byte) in bitfield.iter().enumerate() {
                        for bit in 0..8 {
                            let piece_idx = i * 8 + bit;
                            if piece_idx < peer_has_pieces.len() && (byte & (1 << (7 - bit))) != 0 {
                                peer_has_pieces[piece_idx] = true;
                            }
                        }
                    }
                }

                // DOWNLOAD LOGIC: Receive a block of data
                Message::Piece {
                    index,
                    begin,
                    block,
                } => {
                    if let Some(state) = &mut current_work {
                        // Ensure this block belongs to the piece we are currently downloading
                        if state.piece_index == index as usize {
                            let begin_usize = begin as usize;

                            // Bounds check to prevent buffer overflow attacks
                            if begin_usize + block.len() <= state.piece_buffer.len() {
                                state.piece_buffer[begin_usize..begin_usize + block.len()]
                                    .copy_from_slice(&block);
                                state.downloaded += block.len() as u32;

                                // Check if the piece is fully assembled
                                if state.downloaded == state.piece_length {
                                    // Verify Integrity (SHA-1)
                                    let mut hasher = Sha1::new();
                                    hasher.update(&state.piece_buffer);
                                    let actual_hash: [u8; 20] = hasher.finalize().into();

                                    let mut m = manager.lock().await;
                                    let expected_hash =
                                        m.torrent.get_piece_hash(state.piece_index)?;

                                    if actual_hash == expected_hash {
                                        // println!("{}: Piece {} Verified!", peer_addr, state.piece_index);
                                        m.mark_piece_complete(state.piece_index);

                                        // Delegate writing to Manager (Single Source of Truth for file I/O)
                                        if let Err(e) = m.write_piece_to_disk(
                                            state.piece_index,
                                            &state.piece_buffer,
                                        ) {
                                            println!("Disk Write Failed: {}", e);
                                        }

                                        current_work = None;
                                    } else {
                                        println!(
                                            "{}: Piece {} Hash Mismatch",
                                            peer_addr, state.piece_index
                                        );
                                        // Failed hash check -> Release piece for re-download
                                        m.reset_piece(state.piece_index);
                                        current_work = None;
                                    }
                                }
                            }
                        }
                    }
                }

                // SEEDING LOGIC: Respond to requests from the peer
                Message::Request {
                    index,
                    begin,
                    length,
                } => {
                    let m = manager.lock().await;

                    // Only serve pieces we have fully validated
                    if m.piece_status.get(index as usize)
                        == Some(&crate::core::manager::PieceStatus::Complete)
                    {
                        let piece_len = m.torrent.calculate_piece_size(index as usize) as u64;

                        // Read directly from disk
                        if let Ok(buffer) =
                            m.read_piece_from_disk(index as usize, piece_len, "downloads")
                        {
                            let start = begin as usize;
                            let end = start + length as usize;

                            if end <= buffer.len() {
                                let block_data = buffer[start..end].to_vec();
                                let response = Message::Piece {
                                    index,
                                    begin,
                                    block: block_data,
                                };

                                // Release lock before network I/O
                                drop(m);
                                stream.write_all(&response.serialize()).await?;
                                // println!("Uploaded {} bytes to {}", length, peer_addr);
                            }
                        }
                    }
                }
                Message::KeepAlive => {}
            }

            // --- WORK ASSIGNMENT STRATEGY ---
            // If we are ready to download (unchoked + idle), ask the Manager for a new piece.
            if am_unchoked && current_work.is_none() {
                let mut m = manager.lock().await;
                // Only pick a piece that this specific peer actually has
                if let Some(index) = m.pick_next_piece(&peer_has_pieces) {
                    let piece_len = m.torrent.calculate_piece_size(index);
                    drop(m); // Unlock ASAP

                    // println!("{}: Starting Piece {}", peer_addr, index);

                    // Initialize state for the new piece
                    current_work = Some(PeerSessionState {
                        piece_index: index,
                        piece_buffer: vec![0u8; piece_len as usize],
                        downloaded: 0,
                        requested: 0,
                        piece_length: piece_len,
                    });
                } else {
                    drop(m);
                    // No pieces available that this peer has (or we are done)
                }
            }

            // --- PIPELINING REQUESTS ---
            // To maximize throughput, we keep up to 5 blocks (approx 80KB) "in flight" at once.
            if let Some(state) = &mut current_work {
                while am_unchoked
                    && state.requested < state.piece_length
                    && (state.requested - state.downloaded) < (BLOCK_MAX * 5)
                {
                    let remaining = state.piece_length - state.requested;
                    let block_size = std::cmp::min(BLOCK_MAX, remaining);

                    let request = Message::Request {
                        index: state.piece_index as u32,
                        begin: state.requested,
                        length: block_size,
                    };
                    stream.write_all(&request.serialize()).await?;
                    state.requested += block_size;
                }
            }
        }
    }
    .await;

    // --- FAILURE CLEANUP ---
    // If the connection drops while we were working on a piece, we MUST release it
    // so another peer can pick it up.
    if let Some(state) = current_work {
        // println!("{}: Connection died. Releasing Piece {}", peer_addr, state.piece_index);
        let mut m = manager.lock().await;
        m.reset_piece(state.piece_index);
    }

    result
}
