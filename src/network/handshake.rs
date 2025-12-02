pub struct Handshake {
    pub protocol_string: String,
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
}

impl Handshake {
    pub fn new(info_hash: [u8; 20], peer_id: [u8; 20]) -> Self {
        Self {
            protocol_string: "BitTorrent protocol".to_string(),
            info_hash,
            peer_id,
        }
    }

    // Convert the Handshake struct as 68 bytes vector
    pub fn as_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(68);
        
        // length of protocol string
        bytes.push(19); 
        // protocol string
        bytes.extend_from_slice(self.protocol_string.as_bytes()); 
        // Reserved Bytes (len = 8)
        bytes.extend_from_slice(&[0u8; 8]); 
        bytes.extend_from_slice(&self.info_hash);
        bytes.extend_from_slice(&self.peer_id);
        
        bytes
    }
}