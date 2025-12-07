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
    // Lock manager briefly to initialize bitfield size
    let piece_count = manager.lock().await.piece_status.len();
    let mut peer_has_pieces = vec![false; piece_count];

    // The current piece this peer is working on (if any)
    let mut current_work: Option<PeerSessionState> = None;

    // --- 3. Message Loop ---
    // We wrap the loop in an async block to easily catch errors/timeouts and perform cleanup
    let result: Result<()> = async {
        loop {
            // FIX: Add a 30s timeout. If peer is silent, kill connection.
            let frame = match timeout(Duration::from_secs(30), Message::read(&mut stream)).await {
                Ok(res) => res?, // Propagate Read Errors
                Err(_) => {
                    return Err(anyhow::anyhow!("Connection timed out (Stalled)"));
                }
            };

            match frame {
                Message::Choke => {
                    println!("{}: Choked", peer_addr);
                    am_unchoked = false;
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

                            // Safety check
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

                                    let mut m = manager.lock().await;
                                    let expected_hash =
                                        m.torrent.get_piece_hash(state.piece_index)?;

                                    if actual_hash == expected_hash {
                                        println!(
                                            "{}: Piece {} Verified!",
                                            peer_addr, state.piece_index
                                        );
                                        m.mark_piece_complete(state.piece_index);

                                        // --- MULTI-FILE WRITE LOGIC ---
                                        let output_dir = "downloads";
                                        let piece_len = state.piece_length as u64;
                                        let piece_global_start =
                                            (state.piece_index as u64) * piece_len;
                                        let piece_global_end = piece_global_start + piece_len;

                                        // 1. Get the list of files (handle single vs multi file torrents uniformly)
                                        let files_list = if let Some(files) = &m.torrent.info.files
                                        {
                                            // Multi-file
                                            files
                                                .iter()
                                                .map(|f| {
                                                    let mut path =
                                                        std::path::PathBuf::from(output_dir);
                                                    path.push(&m.torrent.info.name);
                                                    for part in &f.path {
                                                        path.push(part);
                                                    }
                                                    (path, f.length)
                                                })
                                                .collect::<Vec<_>>()
                                        } else {
                                            // Single-file
                                            let mut path = std::path::PathBuf::from(output_dir);
                                            path.push(&m.torrent.info.name);
                                            vec![(path, m.torrent.total_length())]
                                        };

                                        // 2. Iterate through files and write overlapping chunks
                                        let mut file_global_start = 0u64;
                                        for (path, file_len) in files_list {
                                            let file_global_end =
                                                file_global_start + (file_len as u64);

                                            // Check overlap
                                            if file_global_end > piece_global_start
                                                && file_global_start < piece_global_end
                                            {
                                                // Calculate slice inside the piece buffer
                                                let write_start_in_piece =
                                                    if file_global_start > piece_global_start {
                                                        file_global_start - piece_global_start
                                                    } else {
                                                        0
                                                    };
                                                let write_end_in_piece =
                                                    if file_global_end < piece_global_end {
                                                        file_global_end - piece_global_start
                                                    } else {
                                                        piece_len
                                                    };

                                                // Calculate offset inside the target file
                                                let seek_pos_in_file =
                                                    if piece_global_start > file_global_start {
                                                        piece_global_start - file_global_start
                                                    } else {
                                                        0
                                                    };

                                                // Ensure dirs exist
                                                if let Some(parent) = path.parent() {
                                                    tokio::fs::create_dir_all(parent).await.ok();
                                                }

                                                // Write
                                                let mut file = tokio::fs::OpenOptions::new()
                                                    .write(true)
                                                    .create(true)
                                                    .open(&path)
                                                    .await?;
                                                file.seek(std::io::SeekFrom::Start(
                                                    seek_pos_in_file,
                                                ))
                                                .await?;

                                                let buffer_slice = &state.piece_buffer
                                                    [write_start_in_piece as usize
                                                        ..write_end_in_piece as usize];
                                                file.write_all(buffer_slice).await?;
                                                file.sync_all().await?;

                                                let slice_len =
                                                    write_end_in_piece - write_start_in_piece;
                                                println!(
                                                    "Wrote {} bytes to {:?}",
                                                    slice_len, path
                                                );
                                            }
                                            file_global_start += file_len as u64;
                                        }

                                        // Done with this piece
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

            // --- WORK ASSIGNMENT ---
            // If unchoked and idle, ask manager for work
            if am_unchoked && current_work.is_none() {
                let mut m = manager.lock().await;
                if let Some(index) = m.pick_next_piece(&peer_has_pieces) {
                    let piece_len = m.torrent.calculate_piece_size(index);
                    drop(m);

                    println!("{}: Starting Piece {}", peer_addr, index);
                    current_work = Some(PeerSessionState {
                        piece_index: index,
                        piece_buffer: vec![0u8; piece_len as usize],
                        downloaded: 0,
                        requested: 0,
                        piece_length: piece_len,
                    });
                } else {
                    drop(m);
                    // No work available for this peer right now
                }
            }

            // --- PIPELINE ---
            // Keep 5 blocks (approx 80KB) in flight
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

    // --- CLEANUP ---
    // If the loop exited (error or timeout), release the piece back to the manager
    if let Some(state) = current_work {
        println!(
            "{}: Connection died. Releasing Piece {}",
            peer_addr, state.piece_index
        );
        let mut m = manager.lock().await;
        m.reset_piece(state.piece_index);
    }

    result
}
