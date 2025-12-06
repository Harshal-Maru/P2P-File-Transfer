use rand::Rng;
use url::form_urlencoded;

pub fn generate_peer_id() -> [u8; 20] {
    const PREFIX: &[u8; 8] = b"-RT0100-";
    const CHARSET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

    let mut rng = rand::thread_rng();
    let mut peer_id = [0u8; 20];

    peer_id[..8].copy_from_slice(PREFIX);

    for i in 8..20 {
        let idx = rng.gen_range(0..CHARSET.len());
        peer_id[i] = CHARSET[idx];
    }

    peer_id
}


pub fn url_encode (data: &[u8]) -> String {
    form_urlencoded::byte_serialize(data).collect()
}


