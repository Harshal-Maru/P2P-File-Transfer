use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use sha1::{Digest, Sha1};
use std::fs;

#[derive(Debug, Deserialize, Clone)]
pub struct Torrent {
    // The URL of the tracker that coordinates the swarm
    pub announce: String,

    #[serde(rename = "announce-list")]
    pub announce_list: Option<Vec<Vec<String>>>, // It's a list of lists of strings

    // The dictionary containing metadata about the file(s)
    pub info: Info,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Info {
    pub name: String,

    // Map 'piece length' from Bencode (space) to 'piece_length' in Rust (snake_case)
    #[serde(rename = "piece length")]
    pub piece_length: usize,

    // The concatenated SHA-1 hashes of all pieces.
    // Using ByteBuf forces Serde to treat this as a raw binary blob
    // instead of a list of integers.
    pub pieces: ByteBuf,

    // File size (Optional to support multi-file torrents structure in future)
    pub length: Option<i64>,

    pub files: Option<Vec<FileNode>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FileNode {
    pub length: i64,
    pub path: Vec<String>, // e.g., ["bin", "data.pak"]
}

impl Torrent {
    pub fn read(file_path: &str) -> anyhow::Result<Self> {
        let file_content = fs::read(file_path).context("Failed to read torrent file")?;

        let torrent: Torrent =
            serde_bencode::from_bytes(&file_content).context("Failed to decode bencode data")?;

        Ok(torrent)
    }

    // Calculates the SHA-1 hash of the 'info' dictionary.
    // This hash uniquely identifies the torrent in the swarm.
    pub fn calculate_info_hash(&self) -> anyhow::Result<[u8; 20]> {
        // We must re-serialize the parsed info struct back to Bencode bytes
        // to calculate the hash correctly.
        let info_bytes = serde_bencode::to_bytes(&self.info)?;

        let mut hasher = Sha1::new();
        hasher.update(&info_bytes);
        let result = hasher.finalize();

        Ok(result.into())
    }

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

    pub fn total_length(&self) -> i64 {
        if let Some(len) = self.info.length {
            return len;
        }
        if let Some(files) = &self.info.files {
            return files.iter().map(|f| f.length).sum();
        }
        0
    }

    pub fn get_tracker_urls(&self) -> Vec<String> {
        let mut trackers = Vec::new();

        // 1. Always add the main announce URL first
        trackers.push(self.announce.clone());

        // 2. Add all backups from the announce-list
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

    pub fn calculate_piece_size(&self, piece_index: usize) -> u32 {
        let piece_len = self.info.piece_length as u64;
        let total_len = self.total_length();
        let num_pieces = self.info.pieces.len() / 20;

        // If it's the last piece, the size is the remainder
        if piece_index == num_pieces - 1 {
            let remainder = total_len % piece_len as i64;
            if remainder == 0 {
                piece_len as u32
            } else {
                remainder as u32
            }
        } else {
            // Otherwise, it's the standard size
            piece_len as u32
        }
    }
}
