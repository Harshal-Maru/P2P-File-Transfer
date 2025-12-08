/// Represents the initial Handshake message exchanged between peers.
///
/// The handshake is the first message sent immediately after establishing a TCP connection.
/// It ensures that both peers are communicating via the BitTorrent protocol and are
/// interested in the same torrent (verified via the Info Hash).
///
/// Structure (Total 68 bytes):
/// - 1 byte:  Length of the protocol identifier (19).
/// - 19 bytes: Protocol identifier string ("BitTorrent protocol").
/// - 8 bytes: Reserved bytes (set to 0, reserved for extensions like DHT/Fast).
/// - 20 bytes: Info Hash (SHA-1 hash of the metainfo file).
/// - 20 bytes: Peer ID (Unique identifier for this client).
pub struct Handshake {
    pub protocol_string: String,
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
}

impl Handshake {
    /// Creates a new Handshake instance for the specific torrent.
    pub fn new(info_hash: [u8; 20], peer_id: [u8; 20]) -> Self {
        Self {
            protocol_string: "BitTorrent protocol".to_string(),
            info_hash,
            peer_id,
        }
    }

    /// Serializes the Handshake struct into a raw byte vector.
    ///
    /// Returns exactly 68 bytes formatted according to the BitTorrent specification.
    pub fn as_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(68);

        // 1. Length of the protocol identifier
        bytes.push(19);

        // 2. Protocol identifier string
        bytes.extend_from_slice(self.protocol_string.as_bytes());

        // 3. Reserved Bytes (8 bytes set to 0)
        bytes.extend_from_slice(&[0u8; 8]);

        // 4. Info Hash
        bytes.extend_from_slice(&self.info_hash);

        // 5. Peer ID
        bytes.extend_from_slice(&self.peer_id);

        bytes
    }
}