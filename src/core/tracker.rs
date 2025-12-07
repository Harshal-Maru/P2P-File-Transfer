use crate::core::torrent_info::Torrent;
use crate::utils::url_encode;
use anyhow::Context;
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use serde::Deserialize;
use serde_bytes::ByteBuf;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::time::timeout;

#[derive(Debug, Deserialize)]
pub struct Response {
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
        // Get the full list of trackers (Main + Backups)
        let tracker_urls = torrent.get_tracker_urls();
        println!(
            "Found {} trackers. Trying them in order...",
            tracker_urls.len()
        );

        // Loop through them until one works
        for url in tracker_urls {
            println!("Trying tracker: {}", url);

            // Try to connect (HTTP or UDP)
            let result = if url.starts_with("udp://") {
                Self::udp_announce(&url, &torrent.calculate_info_hash()?, peer_id).await
            } else if url.starts_with("http://") || url.starts_with("https://") {
                Self::http_announce(&url, torrent, peer_id).await
            } else {
                println!("Skipping unsupported protocol: {}", url);
                continue;
            };

            match result {
                Ok(peers) => {
                    if !peers.is_empty() {
                        println!(" Success! Found {} peers on {}.", peers.len(), url);
                        return Ok(peers);
                    } else {
                        println!("Tracker worked but returned 0 peers. Trying next...");
                    }
                }
                Err(e) => {
                    println!(" Tracker failed: {}. Trying next...", e);
                }
            }
        }

        anyhow::bail!("All trackers failed. Could not find any peers.")
    }

    // --- HTTP LOGIC ---
    async fn http_announce(
        url: &str,
        torrent: &Torrent,
        peer_id: &[u8; 20],
    ) -> anyhow::Result<Vec<String>> {
        let info_hash = torrent.calculate_info_hash()?;
        let encoded_info_hash = url_encode(&info_hash);
        let encoded_peer_id = url_encode(peer_id);

        let final_url = format!(
            "{}?info_hash={}&peer_id={}&port=8888&uploaded=0&downloaded=0&compact=1&left={}",
            url, // Use the specific URL passed in, not torrent.announce
            encoded_info_hash,
            encoded_peer_id,
            torrent.total_length()
        );

        let response = reqwest::get(&final_url)
            .await
            .context("Failed to connect to tracker")?;
        let response_bytes = response.bytes().await.context("Failed to read bytes")?;
        let tracker_response: Response = serde_bencode::from_bytes(&response_bytes)
            .context("Failed to decode tracker response")?;

        Self::extract_peers(tracker_response.peers)
    }

    // --- UDP LOGIC ---
    async fn udp_announce(
        announce_url: &str,
        info_hash: &[u8; 20],
        peer_id: &[u8; 20],
    ) -> anyhow::Result<Vec<String>> {
        // 1. Parse URL to get host:port
        // Remove "udp://" and find the port
        let url_part = announce_url.strip_prefix("udp://").unwrap_or(announce_url);
        // Some URLs have paths like "/announce", strip that too
        let host_port = url_part.split('/').next().unwrap();

        println!("Connecting to UDP Tracker: {}", host_port);

        // 2. Bind Socket
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        socket
            .connect(host_port)
            .await
            .context("Failed to connect UDP socket")?;

        // 3. CONNECTION REQUEST
        // Protocol ID (magic constant): 0x41727101980
        // Action (Connect): 0
        // Transaction ID: Random (we use 12345)
        let mut connect_req = Vec::new();
        connect_req.write_u64::<BigEndian>(0x41727101980)?;
        connect_req.write_u32::<BigEndian>(0)?; // Action: Connect
        connect_req.write_u32::<BigEndian>(12345)?; // Transaction ID

        socket.send(&connect_req).await?;

        // 4. CONNECTION RESPONSE
        let mut buf = [0u8; 16];
        let (len, _) = timeout(Duration::from_secs(5), socket.recv_from(&mut buf))
            .await
            .context("UDP Connect Timeout")??;

        if len < 16 {
            anyhow::bail!("Invalid UDP connect response");
        }

        let mut rdr = std::io::Cursor::new(&buf[..len]);
        let action = rdr.read_u32::<BigEndian>()?;
        let _trans_id = rdr.read_u32::<BigEndian>()?;
        let connection_id = rdr.read_u64::<BigEndian>()?;

        if action != 0 {
            anyhow::bail!("Tracker rejected connection");
        }

        // 5. ANNOUNCE REQUEST
        // Offsets:
        //  0-8: Connection ID
        //  8-12: Action (1 = Announce)
        // 12-16: Trans ID
        // 16-36: Info Hash (20 bytes)
        // 36-56: Peer ID (20 bytes)
        // 56-64: Downloaded (0)
        // 64-72: Left (0 - simplified)
        // 72-80: Uploaded (0)
        // 80-84: Event (0 = None)
        // 84-88: IP (0)
        // 88-92: Key (0)
        // 92-96: Num Want (-1)
        // 96-98: Port (8888)
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
        // We need a bigger buffer for peers
        let mut response_buf = [0u8; 4096];
        let (len, _) = timeout(Duration::from_secs(5), socket.recv_from(&mut response_buf))
            .await
            .context("UDP Announce Timeout")??;

        let mut rdr = std::io::Cursor::new(&response_buf[..len]);
        let action = rdr.read_u32::<BigEndian>()?;
        let _trans_id = rdr.read_u32::<BigEndian>()?;
        let _interval = rdr.read_u32::<BigEndian>()?;
        let _leechers = rdr.read_u32::<BigEndian>()?;
        let _seeders = rdr.read_u32::<BigEndian>()?;

        if action != 1 {
            anyhow::bail!("Tracker announce failed");
        }

        // REMAINING BYTES ARE PEERS (IP + Port)
        // 4 bytes IP + 2 bytes Port = 6 bytes per peer
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
