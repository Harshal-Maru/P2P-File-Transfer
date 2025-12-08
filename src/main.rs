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
    // 1. Argument Parsing
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage:");
        eprintln!("  Create:   cargo run -- create <input_path> <output_torrent_path>");
        eprintln!("  Download: cargo run -- download <file.torrent>");
        eprintln!("  Seed:     cargo run -- seed <file.torrent>");
        process::exit(1);
    }

    let command = &args[1];

    // --- MODE 1: CREATE TORRENT ---
    if command == "create" {
        if args.len() < 4 {
            eprintln!("Usage: cargo run -- create <input_path> <output_torrent_path>");
            process::exit(1);
        }
        let input_path = &args[2];
        let output_path = &args[3];

        // Use a reliable public UDP tracker by default
        let tracker = "udp://tracker.opentrackr.org:1337";

        // Generate the .torrent file
        core::creator::create_torrent_file(input_path, tracker, output_path)?;
        return Ok(());
    }

    // --- MODE 2 & 3: DOWNLOAD / SEED ---
    if command == "download" || command == "seed" {
        if args.len() < 3 {
            eprintln!("Usage: cargo run -- {} <file.torrent>", command);
            process::exit(1);
        }

        let torrent_path = &args[2];
        let is_seeding_mode = command == "seed";

        // 2. Load Metadata
        println!("Loading torrent file: {}", torrent_path);
        let torrent = core::torrent_info::Torrent::read(torrent_path)?;
        let info_hash = torrent.calculate_info_hash()?;
        let peer_id = utils::generate_peer_id();

        println!("---------------------------------");
        println!("File:       {}", torrent.info.name);
        println!("Info Hash:  {}", hex::encode(&info_hash));
        if is_seeding_mode {
            println!("Mode:       SEEDING (Upload Only)");
        }
        println!("---------------------------------");

        // 3. Initialize Manager
        // Note: Verification runs immediately to pre-allocate files and check resume state.
        let mut temp_manager = TorrentManager::new(torrent.clone());
        temp_manager.verify_existing_data();
        let manager = Arc::new(Mutex::new(temp_manager));

        // 4. Supervision Loop
        // This loop manages the high-level state: contacting trackers and checking completion.
        loop {
            // A. Check Download Status
            {
                let m = manager.lock().await;
                if m.is_complete() {
                    if !is_seeding_mode {
                        println!("DOWNLOAD COMPLETE!");

                        // Safety: Wait for background threads to finish `file.sync_all()`
                        drop(m);
                        sleep(Duration::from_secs(2)).await;

                        println!("Exiting.");
                        break;
                    } else {
                        // In Seed mode, we continue running to serve requests
                        println!("Seeding... (Status: 100% complete)");
                    }
                } else {
                    println!(
                        "Status: {}/{} pieces. Refreshing peers...",
                        m.downloaded_pieces,
                        m.piece_status.len()
                    );
                }
            }

            // B. Contact Tracker (Scatter-Gather)
            println!("Contacting Tracker...");
            match core::tracker::Response::request_peers(&torrent, &peer_id).await {
                Ok(peers) => {
                    println!("Found {} peers. Spawning workers...", peers.len());

                    // C. Spawn Peer Workers
                    // Limit concurrency to avoid file handle exhaustion
                    for peer in peers.into_iter().take(20) {
                        let m_clone = manager.clone();
                        let p_clone = peer_id;
                        let h_clone = info_hash;

                        tokio::spawn(async move {
                            // Each session handles the handshake, download, and upload logic independently
                            let _ =
                                network::run_peer_session(peer, h_clone, p_clone, m_clone).await;
                        });
                    }
                }
                Err(e) => println!("Tracker failed: {}. Retrying in 5s...", e),
            }

            // D. Wait Interval
            // Standard re-announce interval (or shorter for aggressive discovery)
            sleep(Duration::from_secs(10)).await;
        }
    } else {
        eprintln!("Unknown command: {}", command);
    }

    Ok(())
}
