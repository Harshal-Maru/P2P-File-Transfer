use crate::core::torrent_info::Torrent;
use sha1::{Digest, Sha1};
use std::io::{Read, Seek, SeekFrom, Write};

#[derive(Debug, Clone, PartialEq)]
pub enum PieceStatus {
    Pending,
    InProgress,
    Complete,
}

/// Manages the state of the torrent download, including piece tracking,
/// file I/O, and data verification.
///
/// This struct acts as the "Single Source of Truth" for the download progress.
/// It coordinates multiple concurrent workers to ensure pieces are downloaded
/// only once and written to the correct location on disk.
pub struct TorrentManager {
    pub torrent: Torrent,
    pub piece_status: Vec<PieceStatus>,
    pub downloaded_pieces: usize,
}

impl TorrentManager {
    pub fn new(torrent: Torrent) -> Self {
        // Calculate total pieces based on the piece length (usually 20 bytes per hash)
        let piece_count = torrent.info.pieces.len() / 20;
        Self {
            torrent,
            piece_status: vec![PieceStatus::Pending; piece_count],
            downloaded_pieces: 0,
        }
    }

    /// Selects the next available piece to download based on the connected peer's availability.
    ///
    /// Implements a simple "Rarest First" or sequential strategy (currently sequential).
    /// Returns `Some(index)` if a pending piece is found that the peer possesses.
    pub fn pick_next_piece(&mut self, peer_bitfield: &[bool]) -> Option<usize> {
        for (index, status) in self.piece_status.iter_mut().enumerate() {
            if *status == PieceStatus::Pending {
                // Only assign if the peer actually has this piece
                if index < peer_bitfield.len() && peer_bitfield[index] {
                    *status = PieceStatus::InProgress;
                    return Some(index);
                }
            }
        }
        None
    }

    /// Marks a piece as fully downloaded and verified.
    /// Updates the global progress counter.
    pub fn mark_piece_complete(&mut self, index: usize) {
        if self.piece_status[index] != PieceStatus::Complete {
            self.piece_status[index] = PieceStatus::Complete;
            self.downloaded_pieces += 1;
            println!(
                "Piece {} finished. Progress: {}/{}",
                index,
                self.downloaded_pieces,
                self.piece_status.len()
            );
        }
    }

    /// Resets a piece status to Pending.
    ///
    /// This is typically called when a worker disconnects or when a downloaded piece
    /// fails the SHA-1 hash verification.
    pub fn reset_piece(&mut self, index: usize) {
        if self.piece_status[index] != PieceStatus::Complete {
            self.piece_status[index] = PieceStatus::Pending;
        }
    }

    pub fn is_complete(&self) -> bool {
        self.downloaded_pieces == self.piece_status.len()
    }

    /// Scans the disk on startup to identify existing files and verify their integrity.
    ///
    /// This function performs two critical tasks:
    /// 1. **Pre-allocation:** Creates empty files of the correct size to prevent
    ///    sparse file read errors and reduce disk fragmentation.
    /// 2. **Resume:** Reads existing data, hashes it, and updates the `piece_status`
    ///    to skip re-downloading valid pieces.
    pub fn verify_existing_data(&mut self) {
        println!("Checking existing files for resume...");
        let output_dir = "downloads";

        // --- PHASE 0: PRE-ALLOCATE FILES ---
        let files_list = if let Some(files) = &self.torrent.info.files {
            files
                .iter()
                .map(|f| {
                    let mut path = std::path::PathBuf::from(output_dir);
                    path.push(&self.torrent.info.name);
                    for part in &f.path {
                        path.push(part);
                    }
                    (path, f.length)
                })
                .collect::<Vec<_>>()
        } else {
            let mut path = std::path::PathBuf::from(output_dir);
            path.push(&self.torrent.info.name);
            vec![(path, self.torrent.total_length())]
        };

        for (path, length) in &files_list {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).ok();
            }

            match std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .read(true)
                .open(path)
            {
                Ok(file) => {
                    let current_len = file.metadata().map(|m| m.len()).unwrap_or(0);

                    // If file is missing or truncated, extend it.
                    // Important: We assume the OS fills the gap with zeros.
                    if current_len < *length as u64 {
                        println!("Pre-allocating file: {:?} ({} bytes)", path, length);
                        if let Err(e) = file.set_len(*length as u64) {
                            println!("Failed to pre-allocate file: {}", e);
                        }
                        // CRITICAL: Force OS to flush metadata changes to disk immediately.
                        // This prevents race conditions where the reader sees a 0-byte file.
                        let _ = file.sync_all();
                    }
                }
                Err(e) => println!("Failed to open file for pre-allocation: {}", e),
            }
        }

        // --- PHASE 1: VERIFY PIECES ---
        println!("Verifying piece hashes...");
        for index in 0..self.piece_status.len() {
            let piece_index = index;
            let expected_hash = match self.torrent.get_piece_hash(piece_index) {
                Ok(h) => h,
                Err(_) => continue,
            };

            let expected_size = self.torrent.calculate_piece_size(piece_index) as u64;

            // Reuse the robust read logic to check the disk
            match self.read_piece_from_disk(piece_index, expected_size, output_dir) {
                Ok(buffer) => {
                    let mut hasher = Sha1::new();
                    hasher.update(&buffer);
                    let actual_hash: [u8; 20] = hasher.finalize().into();

                    if actual_hash == expected_hash {
                        self.piece_status[piece_index] = PieceStatus::Complete;
                        self.downloaded_pieces += 1;
                    }
                }
                Err(_) => {
                    // Fail silently; piece remains 'Pending' and will be downloaded.
                }
            }
        }

        println!(
            "Resume: Found {}/{} complete pieces.",
            self.downloaded_pieces,
            self.piece_status.len()
        );
    }

    /// Reads a specific piece from the disk, handling logic for pieces that span
    /// across multiple files.
    ///
    /// This function is public to support the seeding functionality (uploading to peers).
    pub fn read_piece_from_disk(
        &self,
        index: usize,
        piece_size: u64,
        output_dir: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let mut buffer = vec![0u8; piece_size as usize];
        let standard_len = self.torrent.info.piece_length as u64;

        // Calculate global byte offsets for this piece
        let piece_global_start = (index as u64) * standard_len;
        let piece_global_end = piece_global_start + piece_size;

        // Flatten the multi-file structure into a linear list of (Path, Length)
        let files_list = if let Some(files) = &self.torrent.info.files {
            files
                .iter()
                .map(|f| {
                    let mut path = std::path::PathBuf::from(output_dir);
                    path.push(&self.torrent.info.name);
                    for part in &f.path {
                        path.push(part);
                    }
                    (path, f.length)
                })
                .collect::<Vec<_>>()
        } else {
            let mut path = std::path::PathBuf::from(output_dir);
            path.push(&self.torrent.info.name);
            vec![(path, self.torrent.total_length())]
        };

        let mut file_global_start = 0u64;
        let mut bytes_read = 0;

        for (path, file_len) in files_list {
            let file_global_end = file_global_start + (file_len as u64);

            // Check if this file contains any part of the requested piece
            if file_global_end > piece_global_start && file_global_start < piece_global_end {
                // Calculate the byte range relative to the PIECE
                let read_start_in_piece = if file_global_start > piece_global_start {
                    file_global_start - piece_global_start
                } else {
                    0
                };

                let read_end_in_piece = if file_global_end < piece_global_end {
                    file_global_end - piece_global_start
                } else {
                    piece_size
                };

                // Calculate the byte offset relative to the FILE
                let seek_pos_in_file = if piece_global_start > file_global_start {
                    piece_global_start - file_global_start
                } else {
                    0
                };

                if path.exists() {
                    let mut file = std::fs::File::open(&path)?;
                    file.seek(SeekFrom::Start(seek_pos_in_file))?;

                    let slice_len = (read_end_in_piece - read_start_in_piece) as usize;
                    let mut chunk_buf = vec![0u8; slice_len];
                    file.read_exact(&mut chunk_buf)?;

                    // Copy read data into the main buffer
                    let start = read_start_in_piece as usize;
                    buffer[start..start + slice_len].copy_from_slice(&chunk_buf);
                    bytes_read += slice_len;
                } else {
                    anyhow::bail!("File missing during read operation");
                }
            }
            file_global_start += file_len as u64;
        }

        if bytes_read == piece_size as usize {
            Ok(buffer)
        } else {
            anyhow::bail!(
                "Incomplete read: expected {} bytes, got {}",
                piece_size,
                bytes_read
            )
        }
    }

    /// Writes a downloaded piece to disk.
    ///
    /// This mirrors `read_piece_from_disk` but performs writes. It ensures data is
    /// correctly distributed across file boundaries if a piece spans multiple files.
    /// Includes `sync_all()` calls to enforce data durability.
    pub fn write_piece_to_disk(&self, index: usize, data: &[u8]) -> anyhow::Result<()> {
        let output_dir = "downloads";
        let piece_len = self.torrent.calculate_piece_size(index) as u64;

        // Safety check to ensure network logic delivered the correct amount of data
        if data.len() as u64 != piece_len {
            anyhow::bail!(
                "Data length mismatch. Expected {}, got {}",
                piece_len,
                data.len()
            );
        }

        let piece_global_start = (index as u64) * (self.torrent.info.piece_length as u64);
        let piece_global_end = piece_global_start + piece_len;

        let files_list = if let Some(files) = &self.torrent.info.files {
            files
                .iter()
                .map(|f| {
                    let mut path = std::path::PathBuf::from(output_dir);
                    path.push(&self.torrent.info.name);
                    for part in &f.path {
                        path.push(part);
                    }
                    (path, f.length)
                })
                .collect::<Vec<_>>()
        } else {
            let mut path = std::path::PathBuf::from(output_dir);
            path.push(&self.torrent.info.name);
            vec![(path, self.torrent.total_length())]
        };

        let mut file_global_start = 0u64;

        for (path, file_len) in files_list {
            let file_global_end = file_global_start + (file_len as u64);

            // Check overlap
            if file_global_end > piece_global_start && file_global_start < piece_global_end {
                let write_start_in_piece = if file_global_start > piece_global_start {
                    file_global_start - piece_global_start
                } else {
                    0
                };
                let write_end_in_piece = if file_global_end < piece_global_end {
                    file_global_end - piece_global_start
                } else {
                    piece_len
                };
                let seek_pos_in_file = if piece_global_start > file_global_start {
                    piece_global_start - file_global_start
                } else {
                    0
                };

                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).ok();
                }

                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .open(&path)?;
                file.seek(std::io::SeekFrom::Start(seek_pos_in_file))?;

                let buffer_slice =
                    &data[write_start_in_piece as usize..write_end_in_piece as usize];

                file.write_all(buffer_slice)?;
                // Critical for data integrity on crash/restart
                file.sync_all()?;
            }
            file_global_start += file_len as u64;
        }
        Ok(())
    }
}
