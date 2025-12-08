use rand::Rng;
use url::form_urlencoded;

/// Generates a unique 20-byte Peer ID for this client instance.
///
/// Following the Azureus-style convention:
/// - First 8 bytes: `-RT0100-` (Client ID 'RT' and Version '0100').
/// - Last 12 bytes: Random alphanumeric characters to ensure uniqueness in the swarm.
pub fn generate_peer_id() -> [u8; 20] {
    const PREFIX: &[u8; 8] = b"-RT0100-";
    const CHARSET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

    let mut rng = rand::thread_rng();
    let mut peer_id = [0u8; 20];

    // Apply the client version prefix
    peer_id[..8].copy_from_slice(PREFIX);

    // Fill the remaining 12 bytes with random characters
    for i in 8..20 {
        let idx = rng.gen_range(0..CHARSET.len());
        peer_id[i] = CHARSET[idx];
    }

    peer_id
}

/// URL-encodes a byte slice into a string suitable for HTTP query parameters.
///
/// This is primarily used for encoding the `info_hash` and `peer_id` when
/// communicating with HTTP trackers, as they often contain non-printable binary data.
pub fn url_encode(data: &[u8]) -> String {
    form_urlencoded::byte_serialize(data).collect()
}
