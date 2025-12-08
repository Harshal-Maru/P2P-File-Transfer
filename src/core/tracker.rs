use crate::core::torrent_info::Torrent;
use crate::utils::url_encode;
use anyhow::Context;
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use serde::Deserialize;
use serde_bytes::ByteBuf;
use std::collections::HashSet;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::timeout;

#[derive(Debug, Deserialize)]
pub struct Response {
    pub _interval: Option<i64>, 
    pub peers: Peers,
}

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
        let tracker_urls = torrent.get_tracker_urls();
        let info_hash = torrent.calculate_info_hash()?;
        let total_length = torrent.total_length();
        let peer_id_fixed = *peer_id; 

        println!(
            "Found {} trackers. Contacting all concurrently...",
            tracker_urls.len()
        );

        let mut handles = Vec::new();

        // 1. SCATTER: Spawn a task for every tracker
        for url in tracker_urls {
            let url = url.clone();
            let info_hash = info_hash; // Copy
            let peer_id = peer_id_fixed; // Copy

            handles.push(tokio::spawn(async move {
                // Try to connect (HTTP or UDP)
                let res = if url.starts_with("udp://") {
                    Self::udp_announce(&url, &info_hash, &peer_id).await
                } else if url.starts_with("http://") || url.starts_with("https://") {
                    // Refactored to take lightweight args instead of &Torrent
                    Self::http_announce(&url, &info_hash, total_length, &peer_id).await
                } else {
                    Err(anyhow::anyhow!("Unsupported protocol"))
                };

                (url, res) // Return the URL so we know who failed
            }));
        }

        // 2. GATHER: Collect results
        let mut unique_peers = HashSet::new();

        for handle in handles {
            // Await the thread result
            if let Ok((url, result)) = handle.await {
                match result {
                    Ok(peers) => {
                        if !peers.is_empty() {
                            println!("{} returned {} peers.", url, peers.len());
                            for p in peers {
                                unique_peers.insert(p);
                            }
                        }
                    }
                    Err(e) => {
                        // Silently fail or print minimal error to keep logs clean
                        println!(" {} failed: {}", url, e);
                    }
                }
            }
        }

        if unique_peers.is_empty() {
            anyhow::bail!("All trackers failed. Could not find any peers.");
        }

        println!("Merged list: {} unique peers found.", unique_peers.len());
        Ok(unique_peers.into_iter().collect())
    }

    // --- HTTP HELPER ---
    async fn http_announce(
        url: &str,
        info_hash: &[u8; 20],
        total_length: i64,
        peer_id: &[u8; 20],
    ) -> anyhow::Result<Vec<String>> {
        let encoded_info_hash = url_encode(info_hash);
        let encoded_peer_id = url_encode(peer_id);

        let final_url = format!(
            "{}?info_hash={}&peer_id={}&port=8888&uploaded=0&downloaded=0&compact=1&left={}",
            url, encoded_info_hash, encoded_peer_id, total_length
        );

        // Shorter timeout for HTTP
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;

        let response = client
            .get(&final_url)
            .send()
            .await
            .context("Failed to connect")?;
        let response_bytes = response.bytes().await.context("Failed to read bytes")?;

        let tracker_response: Response = serde_bencode::from_bytes(&response_bytes)
            .context("Failed to decode tracker response")?;

        Self::extract_peers(tracker_response.peers)
    }

    // --- UDP LOGIC  ---
    async fn udp_announce(
        announce_url: &str,
        info_hash: &[u8; 20],
        peer_id: &[u8; 20],
    ) -> anyhow::Result<Vec<String>> {
        let url_part = announce_url.strip_prefix("udp://").unwrap_or(announce_url);
        let host_port = url_part.split('/').next().unwrap();

        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket.connect(host_port).await.context("Connect failed")?;

        // 3. CONNECTION REQUEST
        let mut connect_req = Vec::new();
        connect_req.write_u64::<BigEndian>(0x41727101980)?;
        connect_req.write_u32::<BigEndian>(0)?; // Action: Connect
        connect_req.write_u32::<BigEndian>(12345)?; // Trans ID
        socket.send(&connect_req).await?;

        // 4. CONNECTION RESPONSE
        let mut buf = [0u8; 16];
        let (len, _) = timeout(Duration::from_secs(3), socket.recv_from(&mut buf))
            .await
            .context("Connect timeout")??;

        if len < 16 {
            anyhow::bail!("Invalid response");
        }
        let mut rdr = std::io::Cursor::new(&buf[..len]);
        let _action = rdr.read_u32::<BigEndian>()?;
        let _trans_id = rdr.read_u32::<BigEndian>()?;
        let connection_id = rdr.read_u64::<BigEndian>()?;

        // 5. ANNOUNCE REQUEST
        let mut announce_req = Vec::new();
        announce_req.write_u64::<BigEndian>(connection_id)?;
        announce_req.write_u32::<BigEndian>(1)?; // Action: Announce
        announce_req.write_u32::<BigEndian>(12345)?;
        announce_req.extend_from_slice(info_hash);
        announce_req.extend_from_slice(peer_id);
        announce_req.write_u64::<BigEndian>(0)?; // Downloaded
        announce_req.write_u64::<BigEndian>(0)?; // Left
        announce_req.write_u64::<BigEndian>(0)?; // Uploaded
        announce_req.write_u32::<BigEndian>(0)?; // Event: None
        announce_req.write_u32::<BigEndian>(0)?; // IP
        announce_req.write_u32::<BigEndian>(0)?; // Key
        announce_req.write_i32::<BigEndian>(-1)?; // Num Want
        announce_req.write_u16::<BigEndian>(8888)?; // Port
        socket.send(&announce_req).await?;

        // 6. ANNOUNCE RESPONSE
        let mut response_buf = [0u8; 4096];
        let (len, _) = timeout(Duration::from_secs(3), socket.recv_from(&mut response_buf))
            .await
            .context("Announce timeout")??;

        let mut rdr = std::io::Cursor::new(&response_buf[..len]);
        let _action = rdr.read_u32::<BigEndian>()?;
        let _trans_id = rdr.read_u32::<BigEndian>()?;
        let _interval = rdr.read_u32::<BigEndian>()?;
        let _leechers = rdr.read_u32::<BigEndian>()?;
        let _seeders = rdr.read_u32::<BigEndian>()?;

        let mut peers = Vec::new();
        while rdr.position() < len as u64 {
            if let Ok(ip_num) = rdr.read_u32::<BigEndian>() {
                if let Ok(port) = rdr.read_u16::<BigEndian>() {
                    let ip = std::net::Ipv4Addr::from(ip_num);
                    peers.push(format!("{}:{}", ip, port));
                }
            } else {
                break;
            }
        }
        Ok(peers)
    }

    fn extract_peers(peers: Peers) -> anyhow::Result<Vec<String>> {
        let mut peer_addresses = Vec::new();
        match peers {
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
