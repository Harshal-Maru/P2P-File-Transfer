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
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage:");
        eprintln!("  Create:   cargo run -- create <input_path> <output_torrent_path>");
        eprintln!("  Download: cargo run -- download <file.torrent>");
        eprintln!("  Seed:     cargo run -- seed <file.torrent>");
        process::exit(1);
    }

    let command = &args[1];

    // --- CREATE MODE ---
    if command == "create" {
        if args.len() < 4 {
            eprintln!("Usage: cargo run -- create <input_path> <output_torrent_path>");
            process::exit(1);
        }
        let input_path = &args[2];
        let output_path = &args[3];
        // Use a generic public tracker that works well
        let tracker = "udp://tracker.opentrackr.org:1337";

        core::creator::create_torrent_file(input_path, tracker, output_path)?;
        return Ok(());
    }

    // --- DOWNLOAD & SEED MODES ---
    if command == "download" || command == "seed" {
        if args.len() < 3 {
            eprintln!("Usage: cargo run -- {} <file.torrent>", command);
            process::exit(1);
        }
        let torrent_path = &args[2];
        let is_seeding_mode = command == "seed";

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

        let mut temp_manager = TorrentManager::new(torrent.clone());
        temp_manager.verify_existing_data();
        let manager = Arc::new(Mutex::new(temp_manager));

        // SUPERVISION LOOP
        loop {
            // A. Check Status
            {
                let m = manager.lock().await;
                if m.is_complete() {
                    if !is_seeding_mode {
                        println!("DOWNLOAD COMPLETE!");
                        drop(m);
                        sleep(Duration::from_secs(2)).await; // Wait for disk flush
                        println!("Exiting.");
                        break;
                    } else {
                        // In Seed mode, we just keep going even if complete
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

            // B. Contact Tracker
            // (In a real client, we would update the tracker with "left=0" if seeding)
            println!("Contacting Tracker...");
            match core::tracker::Response::request_peers(&torrent, &peer_id).await {
                Ok(peers) => {
                    println!("Found {} peers. Spawning workers...", peers.len());
                    for peer in peers.into_iter().take(20) {
                        let m_clone = manager.clone();
                        let p_clone = peer_id;
                        let h_clone = info_hash;
                        tokio::spawn(async move {
                            // Run session (Download & Upload)
                            let _ =
                                network::run_peer_session(peer, h_clone, p_clone, m_clone).await;
                        });
                    }
                }
                Err(e) => println!("Tracker failed: {}. Retrying in 5s...", e),
            }

            // Wait interval
            sleep(Duration::from_secs(10)).await;
        }
    } else {
        eprintln!("Unknown command: {}", command);
    }

    Ok(())
}
