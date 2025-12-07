use crate::core::torrent_info::Torrent;
use crate::utils::url_encode;
use anyhow::Context;
use serde::Deserialize;
use serde_bytes::ByteBuf; 
#[derive(Debug, Deserialize)]
pub struct Response {
    pub interval: i64,
    pub peers: Peers,
}

// This enum tries to match the input data to one of the variants.
// If the tracker sends a String/Bytes, it matches 'Binary'.
// If the tracker sends a List, it matches 'List'.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Peers {
    Binary(ByteBuf),
    List(Vec<Peer>),
}

#[derive(Debug, Deserialize)]
pub struct Peer {
    pub ip: String,
    pub port: u16,
}

impl Response {
    pub async fn request_peers(
        torrent: &Torrent,
        peer_id: &[u8; 20],
    ) -> anyhow::Result<Vec<String>> {
        let info_hash = torrent.calculate_info_hash()?;
        let encoded_info_hash = url_encode(&info_hash);
        let encoded_peer_id = url_encode(peer_id);

        let url = format!(
            "{}?info_hash={}&peer_id={}&port=8888&uploaded=0&downloaded=0&compact=1&left={}",
            torrent.announce,
            encoded_info_hash,
            encoded_peer_id,
            torrent.total_length()
        );

        let response = reqwest::get(&url)
            .await
            .context("Failed to connect to tracker")?;

        let response_bytes = response
            .bytes()
            .await
            .context("Failed to read response bytes")?;

        let tracker_response: Response = serde_bencode::from_bytes(&response_bytes)
            .context("Failed to decode tracker response")?;

        let mut peer_addresses = Vec::new();

        match tracker_response.peers {
            Peers::Binary(data) => {
                for chunk in data.chunks(6) {
                    if chunk.len() == 6 {
                        let ip = format!("{}.{}.{}.{}", chunk[0], chunk[1], chunk[2], chunk[3]);
                        let port = u16::from_be_bytes([chunk[4], chunk[5]]);
                        peer_addresses.push(format!("{}:{}", ip, port));
                    }
                }
            }
            Peers::List(list) => {
                for peer in list {
                    peer_addresses.push(format!("{}:{}", peer.ip, peer.port));
                }
            }
        }

        Ok(peer_addresses)
    }
}
