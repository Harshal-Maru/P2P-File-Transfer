mod core;
mod network;
mod utils;

use std::env;
use std::process;
use std::sync::Arc;
use tokio::sync::Mutex;
use crate::core::manager::TorrentManager;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: cargo run <torrent_file>");
        process::exit(1);
    }

    println!("Loading torrent file: {}", args[1]);
    let torrent = core::torrent_info::Torrent::read(&args[1])?;
    let info_hash = torrent.calculate_info_hash()?;
    let peer_id = utils::generate_peer_id();

    println!("File: {}", torrent.info.name);

    // 1. Init Manager
    let manager = Arc::new(Mutex::new(TorrentManager::new(torrent.clone())));

    // 2. Get Peers
    println!("Contacting Tracker...");
    let peers = core::tracker::Response::request_peers(&torrent, &peer_id).await?;
    println!("Found {} peers.", peers.len());

    // 3. Spawn Tasks
    let mut handles = vec![];
    
    // Connect to first 20 peers in parallel
    for peer in peers.into_iter().take(20) {
        let manager_clone = manager.clone();
        let peer_id_clone = peer_id;
        let info_hash_clone = info_hash;

        let handle = tokio::spawn(async move {
            // We ignore errors from individual peers; other peers will pick up the slack
            let _ = network::run_peer_session(peer, info_hash_clone, peer_id_clone, manager_clone).await;
        });
        handles.push(handle);
    }

    // 4. Wait for completion
    // In a real app, we'd wait for a signal. Here we just wait for all tasks (or Ctrl+C)
    for handle in handles {
        let _ = handle.await;
    }

    Ok(())
}