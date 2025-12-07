mod core;
mod network;
mod utils;

use crate::core::manager::TorrentManager;
use std::env;
use std::process;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::{Duration, sleep};

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

    // 3. Init Manager

    // 3. Init Manager
    // Note: We need a temporary mutable manager to run verification BEFORE wrapping in Arc<Mutex>
    let mut temp_manager = TorrentManager::new(torrent.clone());

    // CHECK EXISTING FILES
    temp_manager.verify_existing_data();

    // Now wrap it
    let manager = Arc::new(Mutex::new(temp_manager));

    // 4. SUPERVISION LOOP (The Fix)
    loop {
        // A. Check if done
        {
            let m = manager.lock().await;
            if m.is_complete() {
                println!("DOWNLOAD COMPLETE! Exiting.");
                break;
            }
            println!(
                "Status: {}/{} pieces. Refreshing peers...",
                m.downloaded_pieces,
                m.piece_status.len()
            );
        }

        // B. Contact Tracker
        println!("Contacting Tracker...");
        // We catch errors here so the loop doesn't crash if the tracker is temporarily down
        match core::tracker::Response::request_peers(&torrent, &peer_id).await {
            Ok(peers) => {
                println!("Found {} peers. Spawning workers...", peers.len());

                // C. Spawn Tasks
                for peer in peers.into_iter().take(20) {
                    // Limit to 20 concurrent connections
                    let manager_clone = manager.clone();
                    let peer_id_clone = peer_id;
                    let info_hash_clone = info_hash;

                    tokio::spawn(async move {
                        // We run the session. If it fails or finishes, the task dies.
                        // The main loop will respawn new ones later if needed.
                        let _ = network::run_peer_session(
                            peer,
                            info_hash_clone,
                            peer_id_clone,
                            manager_clone,
                        )
                        .await;
                    });
                }
            }
            Err(e) => {
                println!("Tracker failed: {}. Retrying in 5s...", e);
            }
        }

        // D. Wait before asking again (End Game Strategy)
        // We wait 10 seconds. In the meantime, the background tasks we just spawned
        // are running and downloading pieces.
        sleep(Duration::from_secs(10)).await;
    }

    Ok(())
}
