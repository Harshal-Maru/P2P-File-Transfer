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
    println!("---------------------------------");
    
    // 3. Discovery (Get Peers from Tracker)
    println!("Contacting Tracker to find peers...");
    let peers = core::tracker::Response::request_peers(&torrent, &peer_id).await?;
    println!("Found {} peers.", peers.len());

    // 4. Connection Loop
    // We attempt to connect to the first 10 peers.
    // If run_peer_session returns Ok(()), it means we successfully downloaded a block!
    for (i, peer) in peers.iter().enumerate().take(10) {
        println!("\n--- Attempt {}/10: Connecting to {} ---", i + 1, peer);
        
        match network::run_peer_session(peer, info_hash, peer_id).await {
            Ok(_) => {
                println!("✨ Success! Download test completed.");
                break; // Exit after successful download of one block
            }
            Err(e) => {
                eprintln!("❌ Connection failed: {}", e);
            }
        }
    }

    Ok(())
}