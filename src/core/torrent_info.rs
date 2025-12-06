use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use sha1::{Digest, Sha1};
use std::fs;

#[derive(Debug, Deserialize, Clone)]
pub struct Torrent {
    // The URL of the tracker that coordinates the swarm
    pub announce: String,
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
}
