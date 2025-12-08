use crate::core::torrent_info::Torrent;
use sha1::{Digest, Sha1};
use std::io::{Read, Seek, SeekFrom, Write};

#[derive(Debug, Clone, PartialEq)]
pub enum PieceStatus {
    Pending,
    InProgress,
    Complete,
}

pub struct TorrentManager {
    pub torrent: Torrent,
    pub piece_status: Vec<PieceStatus>,
    pub downloaded_pieces: usize,
}

impl TorrentManager {
    pub fn new(torrent: Torrent) -> Self {
        let piece_count = torrent.info.pieces.len() / 20;
        Self {
            torrent,
            piece_status: vec![PieceStatus::Pending; piece_count],
            downloaded_pieces: 0,
        }
    }

    // We iterate through pieces. We pick one ONLY if:
    // 1. It is Pending
    // 2. The connected peer actually HAS it
    pub fn pick_next_piece(&mut self, peer_bitfield: &[bool]) -> Option<usize> {
        for (index, status) in self.piece_status.iter_mut().enumerate() {
            if *status == PieceStatus::Pending {
                // Check if the peer has this piece
                if index < peer_bitfield.len() && peer_bitfield[index] {
                    *status = PieceStatus::InProgress;
                    println!("Manager: Assigned Piece {} to a worker", index);
                    return Some(index);
                }
            }
        }
        None
    }

    pub fn mark_piece_complete(&mut self, index: usize) {
        if self.piece_status[index] != PieceStatus::Complete {
            self.piece_status[index] = PieceStatus::Complete;
            self.downloaded_pieces += 1;
            println!(
                "Manager: Piece {} finished. Progress: {}/{}",
                index,
                self.downloaded_pieces,
                self.piece_status.len()
            );
        }
    }

    pub fn reset_piece(&mut self, index: usize) {
        if self.piece_status[index] != PieceStatus::Complete {
            println!("Manager: Resetting Piece {} to Pending", index);
            self.piece_status[index] = PieceStatus::Pending;
        }
    }

    pub fn is_complete(&self) -> bool {
        self.downloaded_pieces == self.piece_status.len()
    }

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

            // Open for Read/Write/Create
            match std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .read(true)
                .open(path)
            {
                Ok(file) => {
                    let current_len = file.metadata().map(|m| m.len()).unwrap_or(0);

                    if current_len < *length as u64 {
                        println!("Pre-allocating file: {:?} ({} bytes)", path, length);
                        if let Err(e) = file.set_len(*length as u64) {
                            println!("Failed to pre-allocate file: {}", e);
                        }
                        // FIX: Force OS to write the size change to disk IMMEDIATELY
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
                    // Fail silently, Manager will mark as Pending
                }
            }
        }

        println!(
            "Resume: Found {}/{} complete pieces.",
            self.downloaded_pieces,
            self.piece_status.len()
        );
    }

    // Helper to read a full piece from the multi-file structure
    pub fn read_piece_from_disk(
        &self,
        index: usize,
        piece_size: u64,
        output_dir: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let mut buffer = vec![0u8; piece_size as usize]; // Buffer is exact size requested
        let standard_len = self.torrent.info.piece_length as u64; // Standard size for offsets

        // GLOBAL OFFSET is always based on STANDARD length
        let piece_global_start = (index as u64) * standard_len;
        let piece_global_end = piece_global_start + piece_size;

        // Prepare file list (Same logic as writer)
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

            if file_global_end > piece_global_start && file_global_start < piece_global_end {
                // Calc offsets
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

                let seek_pos_in_file = if piece_global_start > file_global_start {
                    piece_global_start - file_global_start
                } else {
                    0
                };

                // Open file
                if path.exists() {
                    let mut file = std::fs::File::open(&path)?;
                    file.seek(SeekFrom::Start(seek_pos_in_file))?;

                    let slice_len = (read_end_in_piece - read_start_in_piece) as usize;
                    let mut chunk_buf = vec![0u8; slice_len];
                    file.read_exact(&mut chunk_buf)?;

                    // Copy into main buffer
                    let start = read_start_in_piece as usize;
                    buffer[start..start + slice_len].copy_from_slice(&chunk_buf);
                    bytes_read += slice_len;
                } else {
                    // Do not bail immediately, other files might exist.
                    // But for strict checking, bail makes sense.
                    anyhow::bail!("File missing");
                }
            }
            file_global_start += file_len as u64;
        }

        if bytes_read == piece_size as usize {
            Ok(buffer)
        } else {
            anyhow::bail!("Incomplete read")
        }
    }


    pub fn write_piece_to_disk(&self, index: usize, data: &[u8]) -> anyhow::Result<()> {
        let output_dir = "downloads";
        let piece_len = self.torrent.calculate_piece_size(index) as u64; // Uses the fix we made earlier

        // Safety check
        if data.len() as u64 != piece_len {
            anyhow::bail!(
                "Data length mismatch. Expected {}, got {}",
                piece_len,
                data.len()
            );
        }

        let piece_global_start = (index as u64) * (self.torrent.info.piece_length as u64);
        let piece_global_end = piece_global_start + piece_len;

        // Prepare file list (Reusing the logic that we know works)
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

            // Check for Overlap
            if file_global_end > piece_global_start && file_global_start < piece_global_end {
                // Calculate offsets
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

                // Perform Write
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
                file.sync_all()?; // Force save to disk

                println!("Wrote {} bytes to {:?}", buffer_slice.len(), path);
            }
            file_global_start += file_len as u64;
        }
        Ok(())
    }
}
