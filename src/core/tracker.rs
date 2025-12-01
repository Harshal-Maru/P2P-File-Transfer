use anyhow::Context;
use serde::Deserialize;
use crate::core::torrent_info::Torrent;
use crate::utils::url_encode;

#[derive(Debug, Deserialize)]
pub struct Response {
    pub interval: i64,
    pub peers: Vec<Peer>, 
}

#[derive(Debug, Deserialize)]
pub struct Peer {
    pub ip: String,
    pub port: u16,
}

impl Response {
    pub fn request_peers(torrent: &Torrent, peer_id: &[u8; 20]) -> anyhow::Result<Vec<String>> {
        
        let info_hash = torrent.calculate_info_hash()?;
        let encoded_info_hash = url_encode(&info_hash);
        let encoded_peer_id = url_encode(peer_id);

        let url = format!(
            "{}?info_hash={}&peer_id={}&port=6881&uploaded=0&downloaded=0&compact=1&left={}",
            torrent.announce,
            encoded_info_hash,
            encoded_peer_id,
            torrent.info.length.unwrap_or(0)
        );

        let response = reqwest::blocking::get(&url)
            .context("Failed to connect to tracker")?;

        let response_bytes = response.bytes()
            .context("Failed to read response bytes")?;

        // Deserialize using the new Vec<Peer> structure
        let tracker_response: Response = serde_bencode::from_bytes(&response_bytes)
            .context("Failed to decode tracker response")?;

        // Transform the structured data into our simple string list
        let mut peer_addresses = Vec::new();
        for peer in tracker_response.peers {
            peer_addresses.push(format!("{}:{}", peer.ip, peer.port));
        }

        Ok(peer_addresses)
    }
}