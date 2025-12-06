pub mod handshake;
pub mod message;

use crate::core::manager::TorrentManager;
use anyhow::{Context, Result};
use handshake::Handshake;
use message::Message;
use sha1::{Digest, Sha1};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::{Duration, timeout};

const BLOCK_MAX: u32 = 16384;

// State to track what this specific peer is currently working on
struct PeerSessionState {
    piece_index: usize,
    piece_buffer: Vec<u8>,
    downloaded: u32,
    requested: u32,
    piece_length: u32,
}

pub async fn run_peer_session(
    peer_addr: String,
    info_hash: [u8; 20],
    peer_id: [u8; 20],
    manager: Arc<Mutex<TorrentManager>>, // Shared Manager
) -> Result<()> {
    println!("Connecting to {}...", peer_addr);

    let mut stream = timeout(Duration::from_secs(3), TcpStream::connect(&peer_addr))
        .await
        .context("Connection timed out")?
        .context(format!("Failed to connect to peer: {}", peer_addr))?;

    // --- 1. Handshake ---
    let handshake = Handshake::new(info_hash, peer_id);
    stream.write_all(&handshake.as_bytes()).await?;

    let mut response_buf = [0u8; 68];
    stream.read_exact(&mut response_buf).await?;

    if &response_buf[28..48] != info_hash {
        anyhow::bail!("Invalid Info Hash");
    }
    println!("{}: Handshake Successful", peer_addr);

    // --- 2. Interested ---
    let msg = Message::Interested;
    stream.write_all(&msg.serialize()).await?;

    // --- STATE TRACKING ---
    let mut am_unchoked = false;
    let mut peer_has_pieces = vec![false; manager.lock().await.piece_status.len()];

    // The current piece this peer is working on (if any)
    let mut current_work: Option<PeerSessionState> = None;

    // --- 3. Message Loop ---
    loop {
        let frame = Message::read(&mut stream).await?;

        match frame {
            Message::Choke => {
                println!("{}: Choked", peer_addr);
                am_unchoked = false;
                // Ideally we would release the piece back to the manager here,
                // but for simplicity we hold onto it hoping they unchoke us.
            }
            Message::Unchoke => {
                println!("{}: Unchoked", peer_addr);
                am_unchoked = true;
            }
            Message::Interested => {}
            Message::NotInterested => {}

            Message::Have { index } => {
                if (index as usize) < peer_has_pieces.len() {
                    peer_has_pieces[index as usize] = true;
                }
            }
            Message::Bitfield(bitfield) => {
                // Populate peer_has_pieces from the bitfield bytes
                // (Simplification: assuming dense bitfield for this step)
                for (i, byte) in bitfield.iter().enumerate() {
                    for bit in 0..8 {
                        let piece_idx = i * 8 + bit;
                        if piece_idx < peer_has_pieces.len() && (byte & (1 << (7 - bit))) != 0 {
                            peer_has_pieces[piece_idx] = true;
                        }
                    }
                }
            }

            Message::Piece {
                index,
                begin,
                block,
            } => {
                // Handle incoming data
                if let Some(state) = &mut current_work {
                    if state.piece_index == index as usize {
                        let begin_usize = begin as usize;
                        if begin_usize + block.len() <= state.piece_buffer.len() {
                            state.piece_buffer[begin_usize..begin_usize + block.len()]
                                .copy_from_slice(&block);
                            state.downloaded += block.len() as u32;

                            // Check Completion
                            if state.downloaded == state.piece_length {
                                // Verify Hash
                                let mut hasher = Sha1::new();
                                hasher.update(&state.piece_buffer);
                                let actual_hash: [u8; 20] = hasher.finalize().into();

                                // Lock manager to get expected hash & update status
                                let mut m = manager.lock().await;
                                let expected_hash = m.torrent.get_piece_hash(state.piece_index)?;

                                if actual_hash == expected_hash {
                                    println!(
                                        "{}: Piece {} Verified!",
                                        peer_addr, state.piece_index
                                    );
                                    m.mark_piece_complete(state.piece_index);

                                    // 1. Prepare the Downloads folder
                                    let output_dir = "downloads";
                                    tokio::fs::create_dir_all(output_dir).await.ok();

                                    // 2. Construct the full path (downloads/ubuntu.iso)
                                    let filename = &m.torrent.info.name;
                                    let filepath = format!("{}/{}", output_dir, filename);

                                    // 3. Open the file in Read/Write mode
                                    // 'create(true)' makes it if missing.
                                    // 'write(true)' allows us to modify it.
                                    // We open a fresh handle per piece. This relies on the OS's file locking
                                    // to handle concurrency, which is safe for simple writes.
                                    let mut file = tokio::fs::OpenOptions::new()
                                        .write(true)
                                        .create(true)
                                        .open(&filepath)
                                        .await?;

                                    // 4. Calculate the Byte Offset
                                    // Example: Piece 0 starts at 0. Piece 1 starts at 262144.
                                    let offset =
                                        (state.piece_index as u64) * (state.piece_length as u64);

                                    // 5. Seek to the correct position
                                    file.seek(std::io::SeekFrom::Start(offset)).await?;

                                    // 6. Write the data
                                    file.write_all(&state.piece_buffer).await?;
                                    println!(
                                        "Wrote Piece {} to {}",
                                        state.piece_index, filepath
                                    );

                                    // Reset work
                                    current_work = None;
                                } else {
                                    println!(
                                        "{}: Piece {} Hash Mismatch",
                                        peer_addr, state.piece_index
                                    );
                                    m.reset_piece(state.piece_index);
                                    current_work = None;
                                }
                            }
                        }
                    }
                }
            }

            Message::Request { .. } => {}
            Message::KeepAlive => {}
        }

        // --- WORK ASSIGNMENT LOGIC ---
        // If we are unchoked and don't have work, ask the manager for a piece!
        if am_unchoked && current_work.is_none() {
            let mut m = manager.lock().await;

            // 👇 CHANGE: Pass the bitfield to the manager
            if let Some(index) = m.pick_next_piece(&peer_has_pieces) {
                // We are guaranteed that this peer has the piece now.
                let piece_len = m.torrent.info.piece_length as u32;
                drop(m); // Unlock manager immediately

                println!("{}: Starting Piece {}", peer_addr, index);
                current_work = Some(PeerSessionState {
                    piece_index: index,
                    piece_buffer: vec![0u8; piece_len as usize],
                    downloaded: 0,
                    requested: 0,
                    piece_length: piece_len,
                });
            } else {
                // No pieces left that this peer has.
                drop(m); // Release lock
                // We wait loop to continue, maybe they announce more pieces later.
            }
        }
        // --- PIPELINE LOGIC ---
        // If we have work, keep pipelines full (Queue 5 blocks / 80KB at a time)
        if let Some(state) = &mut current_work {
            // PIPELINE SIZE: 5 blocks (Adjustable)
            // While we are unchoked AND we haven't asked for the whole file
            // AND the amount of "In Flight" data is less than 5 blocks...
            while am_unchoked
                && state.requested < state.piece_length
                && (state.requested - state.downloaded) < (BLOCK_MAX * 5)
            {
                // Request the next block
                let remaining = state.piece_length - state.requested;
                let block_size = std::cmp::min(BLOCK_MAX, remaining);

                let request = Message::Request {
                    index: state.piece_index as u32,
                    begin: state.requested,
                    length: block_size,
                };
                stream.write_all(&request.serialize()).await?;

                // Advance the "Requested" cursor, but "Downloaded" stays the same
                // untill data arrives.
                state.requested += block_size;
            }
        }
    }
}
