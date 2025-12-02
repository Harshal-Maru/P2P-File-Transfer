use anyhow::Context;
use serde::Deserialize;
use crate::core::torrent_info::Torrent;
use crate::utils::url_encode;

#[derive(Debug, Deserialize)]
pub struct Response {
    // Interval in seconds that the client should wait before sending requests to the tracker
    pub interval: i64,
    // List of peers provided by the tracker. 
    // Note: We are using the "Dictionary Model" here to support trackers that return 
    // structured lists (common with IPv6), rather than the compact binary model.
    pub peers: Vec<Peer>, 
}

#[derive(Debug, Deserialize)]
pub struct Peer {
    pub ip: String,
    pub port: u16,
}

impl Response {
    pub async fn request_peers(torrent: &Torrent, peer_id: &[u8; 20]) -> anyhow::Result<Vec<String>> {
        
        let info_hash = torrent.calculate_info_hash()?;
        let encoded_info_hash = url_encode(&info_hash);
        let encoded_peer_id = url_encode(peer_id);

        // Build the tracker URL with required query parameters
        // info_hash: Identifies the file we want
        // peer_id: Identifies us to the swarm
        // compact: 1 (Requests binary format, though some trackers ignore this for IPv6)
        let url = format!(
            "{}?info_hash={}&peer_id={}&port=6881&uploaded=0&downloaded=0&compact=1&left={}",
            torrent.announce,
            encoded_info_hash,
            encoded_peer_id,
            torrent.info.length.unwrap_or(0)
        );

        // Perform Async HTTP GET request
        let response = reqwest::get(&url).await
            .context("Failed to connect to tracker")?;

        let response_bytes = response.bytes().await
            .context("Failed to read response bytes")?;

        // Deserialize Bencoded response into struct
        let tracker_response: Response = serde_bencode::from_bytes(&response_bytes)
            .context("Failed to decode tracker response")?;

        // Extract just the address strings (IP:Port) for the network layer
        let mut peer_addresses = Vec::new();
        for peer in tracker_response.peers {
            peer_addresses.push(format!("{}:{}", peer.ip, peer.port));
        }

        Ok(peer_addresses)
    }
}