mod core;
mod network;
mod utils;

use std::env;
use std::process;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Parse Arguments
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: cargo run <torrent_file>");
        process::exit(1);
    }

    // 2. Load Torrent & Meta Info
    println!("Loading torrent file: {}", args[1]);
    let torrent = core::torrent_info::Torrent::read(&args[1])?;
    let info_hash = torrent.calculate_info_hash()?;
    let peer_id = utils::generate_peer_id();

    println!("---------------------------------");
    println!("File:       {}", torrent.info.name);
    println!("Info Hash:  {}", hex::encode(&info_hash));
    println!("Peer ID:    {}", String::from_utf8_lossy(&peer_id));
    println!("Piece Length: {} bytes", torrent.info.piece_length);
    println!("---------------------------------");
    
    // 3. Discovery (Get Peers from Tracker)
    println!("Contacting Tracker to find peers...");
    let peers = core::tracker::Response::request_peers(&torrent, &peer_id).await?;
    println!("Found {} peers.", peers.len());

    // 4. Connection Loop
    for (i, peer) in peers.iter().enumerate().take(10) {
        println!("\n--- Attempt {}/10: Connecting to {} ---", i + 1, peer);
        
        // 👇 UPDATED CALL: We pass '&torrent' and '0' (Piece Index)
        match network::run_peer_session(peer, info_hash, peer_id, &torrent, 0).await {
            Ok(piece_data) => {
                println!("Success! Piece 0 downloaded and verified.");
                println!("Size: {} bytes", piece_data.len());
                
                // Optional: Save it to disk to prove it works
                let filename = format!("piece_0_{}.bin", torrent.info.name);
                std::fs::write(&filename, piece_data).unwrap();
                println!("   Saved to file: {}", filename);
                
                break; // Exit after success
            }
            Err(e) => {
                eprintln!("Connection failed: {}", e);
            }
        }
    }

    Ok(())
}