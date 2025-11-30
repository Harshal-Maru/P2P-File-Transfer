use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use sha1::{Digest, Sha1};
use std::fs;

#[derive(Debug, Deserialize)]
pub struct Torrent {
    pub announce: String,
    pub info: Info,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Info {
    pub name: String,

    // FIX IS HERE: Map "piece length" from file to "piece_length" in Rust
    #[serde(rename = "piece length")] 
    pub piece_length: usize,

    pub pieces: ByteBuf,

    pub length: Option<i64>,
}

impl Torrent {
    pub fn read(file_path: &str) -> anyhow::Result<Self> {
        let file_content = fs::read(file_path).context("Failed to read torrent file")?;

        let torrent: Torrent =
            serde_bencode::from_bytes(&file_content).context("Failed to decode bencode data")?;

        Ok(torrent) // Use standard Ok
    }

    pub fn calculate_info_hash(&self) -> anyhow::Result<[u8; 20]> {
        let info_bytes = serde_bencode::to_bytes(&self.info)?;
        
        let mut hasher = Sha1::new();
        hasher.update(&info_bytes);
        let result = hasher.finalize();

        Ok(result.into())
    }
}