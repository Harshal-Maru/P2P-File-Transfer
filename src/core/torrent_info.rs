use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use sha1::{Digest, Sha1};
use std::fs;

/// Represents the top-level dictionary of a Metainfo (.torrent) file.
///
/// This structure holds the necessary metadata to connect to trackers
/// and validate the data content of the torrent.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Torrent {
    /// The URL of the primary tracker.
    pub announce: String,

    /// Optional list of backup trackers (BEP 12 Multitracker Metadata Extension).
    /// Structure: A list of tiers, where each tier is a list of tracker URLs.
    #[serde(rename = "announce-list")]
    pub announce_list: Option<Vec<Vec<String>>>,

    /// The dictionary containing specific metadata about the file(s) and pieces.
    pub info: Info,
}

/// The 'info' dictionary containing file structure and integrity data.
///
/// The SHA-1 hash of the Bencoded form of this struct is the "Info Hash",
/// which uniquely identifies the torrent in the global swarm.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Info {
    /// Suggested name for the file (single-file) or root directory (multi-file).
    pub name: String,

    /// Number of bytes in each piece (usually a power of 2, e.g., 256KB).
    #[serde(rename = "piece length")]
    pub piece_length: usize,

    /// A concatenated string of 20-byte SHA-1 hashes, one per piece.
    /// Using ByteBuf ensures Serde treats this as raw binary data rather than a generic array.
    pub pieces: ByteBuf,

    /// Length of the file in bytes. Present only in single-file mode.
    pub length: Option<i64>,

    /// List of files. Present only in multi-file mode.
    pub files: Option<Vec<FileNode>>,
}

/// Represents a single file within a multi-file torrent structure.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FileNode {
    pub length: i64,
    /// The path components of the file (e.g., ["folder", "subfolder", "file.txt"]).
    pub path: Vec<String>,
}

impl Torrent {
    /// Reads and deserializes a .torrent file from the specified path.
    pub fn read(file_path: &str) -> anyhow::Result<Self> {
        let file_content = fs::read(file_path).context("Failed to read torrent file")?;

        let torrent: Torrent =
            serde_bencode::from_bytes(&file_content).context("Failed to decode bencode data")?;

        Ok(torrent)
    }

    /// Calculates the Info Hash (SHA-1) of the 'info' dictionary.
    ///
    /// This requires re-serializing the parsed `Info` struct back into Bencode
    /// to ensure the hash matches the original file exactly.
    pub fn calculate_info_hash(&self) -> anyhow::Result<[u8; 20]> {
        let info_bytes = serde_bencode::to_bytes(&self.info)?;

        let mut hasher = Sha1::new();
        hasher.update(&info_bytes);
        let result = hasher.finalize();

        Ok(result.into())
    }

    /// Extracts the expected SHA-1 hash for a specific piece index.
    ///
    /// The `pieces` field is a flat byte array where every 20 bytes corresponds
    /// to one piece.
    pub fn get_piece_hash(&self, piece_index: usize) -> anyhow::Result<[u8; 20]> {
        const HASH_LEN: usize = 20;
        let start = piece_index * HASH_LEN;
        let end = start + HASH_LEN;

        if end > self.info.pieces.len() {
            anyhow::bail!("Piece index out of bounds");
        }

        let mut hash = [0u8; 20];
        hash.copy_from_slice(&self.info.pieces[start..end]);
        Ok(hash)
    }

    /// Calculates the total size of the torrent in bytes.
    /// Handles both single-file and multi-file structures.
    pub fn total_length(&self) -> i64 {
        if let Some(len) = self.info.length {
            return len;
        }
        if let Some(files) = &self.info.files {
            return files.iter().map(|f| f.length).sum();
        }
        0
    }

    /// Aggregates all tracker URLs into a single flat list.
    ///
    /// Combines the primary `announce` URL with the `announce-list` tiers,
    /// ensuring no duplicates are returned.
    pub fn get_tracker_urls(&self) -> Vec<String> {
        let mut trackers = Vec::new();

        // 1. Add primary tracker
        trackers.push(self.announce.clone());

        // 2. Add backup trackers
        if let Some(tiers) = &self.announce_list {
            for tier in tiers {
                for url in tier {
                    if !trackers.contains(url) {
                        trackers.push(url.clone());
                    }
                }
            }
        }
        trackers
    }

    /// Calculates the exact byte size of a specific piece.
    ///
    /// While most pieces are exactly `piece_length`, the final piece is usually smaller
    /// (the remainder of the total size). Requesting the wrong size for the last piece
    /// will cause peers to drop the connection.
    pub fn calculate_piece_size(&self, piece_index: usize) -> u32 {
        let piece_len = self.info.piece_length as u64;
        let total_len = self.total_length();
        // Calculate total number of pieces (ceiling division)
        let num_pieces = self.info.pieces.len() / 20;

        // Check if this is the last piece
        if piece_index == num_pieces - 1 {
            let remainder = total_len % piece_len as i64;
            if remainder == 0 {
                piece_len as u32
            } else {
                remainder as u32
            }
        } else {
            // Standard piece
            piece_len as u32
        }
    }
}
