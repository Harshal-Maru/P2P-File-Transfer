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

/// Represents the response structure from a BitTorrent tracker.
///
/// Trackers return a list of peers (IP:Port) that are currently part of the swarm.
/// This structure handles both standard dictionary-based responses and compact binary responses.
#[derive(Debug, Deserialize)]
pub struct Response {
    /// Interval in seconds that the client should wait before sending the next announce.
    /// Optional because not all trackers provide it immediately or on errors.
    pub _interval: Option<i64>,
    /// The list of peers provided by the tracker.
    pub peers: Peers,
}

/// Enum handling the two possible formats for the peer list:
/// 1. Binary: Compact format (6 bytes per peer: 4 for IP, 2 for Port).
/// 2. List: Dictionary format (List of maps containing "ip" and "port").
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
    /// Contacts all trackers listed in the Torrent file concurrently to retrieve a list of peers.
    ///
    /// Implements a "Scatter-Gather" pattern:
    /// 1. Scatter: Spawns an async task for every tracker URL found in the torrent metadata.
    /// 2. Gather: Collects results as they finish, disregarding slow or failed trackers.
    /// 3. Deduplicate: Uses a HashSet to ensure unique peer addresses.
    ///
    /// This approach significantly reduces startup time compared to sequential announcements.
    pub async fn request_peers(
        torrent: &Torrent,
        peer_id: &[u8; 20],
    ) -> anyhow::Result<Vec<String>> {
        let tracker_urls = torrent.get_tracker_urls();
        let info_hash = torrent.calculate_info_hash()?;
        let total_length = torrent.total_length();
        let peer_id_fixed = *peer_id; // Copy to move into async closure

        println!(
            "Found {} trackers. Contacting all concurrently...",
            tracker_urls.len()
        );

        let mut handles = Vec::new();

        // SCATTER: Spawn a task for every tracker
        for url in tracker_urls {
            let url = url.clone();
            let info_hash = info_hash;
            let peer_id = peer_id_fixed;

            handles.push(tokio::spawn(async move {
                // Determine protocol and dispatch to appropriate handler
                let res = if url.starts_with("udp://") {
                    Self::udp_announce(&url, &info_hash, &peer_id).await
                } else if url.starts_with("http://") || url.starts_with("https://") {
                    Self::http_announce(&url, &info_hash, total_length, &peer_id).await
                } else {
                    Err(anyhow::anyhow!("Unsupported protocol"))
                };

                (url, res)
            }));
        }

        // GATHER: Collect successful results
        let mut unique_peers = HashSet::new();

        for handle in handles {
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
                    Err(_) => {
                        // Fail silently for individual trackers to keep CLI output clean.
                        // We only care about the trackers that actually work.
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

    /// performs an announce request to an HTTP/HTTPS tracker.
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

        // Enforce a short timeout to prevent slow HTTP trackers from blocking the gather phase
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?;

        let response = client
            .get(&final_url)
            .send()
            .await
            .context("Failed to connect to HTTP tracker")?;

        let response_bytes = response
            .bytes()
            .await
            .context("Failed to read HTTP response bytes")?;

        let tracker_response: Response = serde_bencode::from_bytes(&response_bytes)
            .context("Failed to decode HTTP tracker response")?;

        Self::extract_peers(tracker_response.peers)
    }

    /// Performs an announce request to a UDP tracker implementing BEP 15.
    ///
    /// The UDP protocol involves a two-step handshake:
    /// 1. Connect Request -> Connect Response (Get Connection ID)
    /// 2. Announce Request -> Announce Response (Get Peers)
    async fn udp_announce(
        announce_url: &str,
        info_hash: &[u8; 20],
        peer_id: &[u8; 20],
    ) -> anyhow::Result<Vec<String>> {
        // Parse host:port from URL
        let url_part = announce_url.strip_prefix("udp://").unwrap_or(announce_url);
        let host_port = url_part.split('/').next().unwrap();

        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket
            .connect(host_port)
            .await
            .context("UDP Connect failed")?;

        // --- Step 1: Connection Request ---
        let mut connect_req = Vec::new();
        connect_req.write_u64::<BigEndian>(0x41727101980)?; // Magic Constant
        connect_req.write_u32::<BigEndian>(0)?; // Action: Connect
        connect_req.write_u32::<BigEndian>(12345)?; // Transaction ID
        socket.send(&connect_req).await?;

        // Read Connection Response
        let mut buf = [0u8; 16];
        let (len, _) = timeout(Duration::from_secs(3), socket.recv_from(&mut buf))
            .await
            .context("UDP Connect Timeout")??;

        if len < 16 {
            anyhow::bail!("Invalid UDP Connect Response length");
        }
        let mut rdr = std::io::Cursor::new(&buf[..len]);
        let _action = rdr.read_u32::<BigEndian>()?;
        let _trans_id = rdr.read_u32::<BigEndian>()?;
        let connection_id = rdr.read_u64::<BigEndian>()?;

        // --- Step 2: Announce Request ---
        let mut announce_req = Vec::new();
        announce_req.write_u64::<BigEndian>(connection_id)?;
        announce_req.write_u32::<BigEndian>(1)?; // Action: Announce
        announce_req.write_u32::<BigEndian>(12345)?; // Transaction ID
        announce_req.extend_from_slice(info_hash);
        announce_req.extend_from_slice(peer_id);
        announce_req.write_u64::<BigEndian>(0)?; // Downloaded
        announce_req.write_u64::<BigEndian>(0)?; // Left
        announce_req.write_u64::<BigEndian>(0)?; // Uploaded
        announce_req.write_u32::<BigEndian>(0)?; // Event: None
        announce_req.write_u32::<BigEndian>(0)?; // IP (0 = default)
        announce_req.write_u32::<BigEndian>(0)?; // Key
        announce_req.write_i32::<BigEndian>(-1)?; // Num Want (-1 = default)
        announce_req.write_u16::<BigEndian>(8888)?; // Port
        socket.send(&announce_req).await?;

        // Read Announce Response
        let mut response_buf = [0u8; 4096];
        let (len, _) = timeout(Duration::from_secs(3), socket.recv_from(&mut response_buf))
            .await
            .context("UDP Announce Timeout")??;

        let mut rdr = std::io::Cursor::new(&response_buf[..len]);
        let _action = rdr.read_u32::<BigEndian>()?;
        let _trans_id = rdr.read_u32::<BigEndian>()?;
        let _interval = rdr.read_u32::<BigEndian>()?;
        let _leechers = rdr.read_u32::<BigEndian>()?;
        let _seeders = rdr.read_u32::<BigEndian>()?;

        // Extract Peers (Compact IP/Port pairs)
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

    /// Helper to convert raw peer data (Binary or List) into a standardized string format.
    fn extract_peers(peers: Peers) -> anyhow::Result<Vec<String>> {
        let mut peer_addresses = Vec::new();
        match peers {
            Peers::Binary(data) => {
                // Parse chunks of 6 bytes (4 byte IP + 2 byte Port)
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
