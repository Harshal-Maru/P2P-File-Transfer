use crate::core::torrent_info::{FileNode, Info, Torrent};
use sha1::{Digest, Sha1};
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use walkdir::WalkDir;

/// Standard piece size for most torrents (256 KB).
/// This strikes a balance between metadata size (smaller pieces = larger .torrent file)
/// and efficiency (larger pieces = more wasted data on corruption).
const PIECE_LENGTH: usize = 262144;

/// Generates a valid .torrent metainfo file from a given file or directory.
///
/// This function performs the following steps:
/// 1. Scans the input path (recursively if a directory).
/// 2. Sorts files to ensure deterministic hashing (producing the same Info Hash every time).
/// 3. Reads all files as a single continuous stream of bytes.
/// 4. Chunks the stream into 256KB pieces and calculates SHA-1 hashes.
/// 5. Serializes the metadata into Bencode format.
pub fn create_torrent_file(
    path_str: &str,
    announce_url: &str,
    output_path: &str,
) -> anyhow::Result<()> {
    let path = Path::new(path_str);
    if !path.exists() {
        anyhow::bail!("Path does not exist: {}", path_str);
    }

    println!("Hashing files from: {:?}", path);

    // --- 1. Identify Files ---
    let mut files = Vec::new();
    let is_single_file = path.is_file();

    // The 'name' field in the Info dictionary is either the filename
    // or the name of the root directory.
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Invalid path name"))?
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid UTF-8 in path"))?
        .to_string();

    if is_single_file {
        files.push(path.to_path_buf());
    } else {
        // Recursively find all files in the folder
        for entry in WalkDir::new(path) {
            let entry = entry?;
            if entry.file_type().is_file() {
                files.push(entry.path().to_path_buf());
            }
        }
    }

    // Critical: Sort files to ensure the Info Hash is deterministic.
    // If we process files in random order, the resulting hash will change,
    // creating a different torrent swarm for the same data.
    files.sort();

    // --- 2. Hash Pieces ---
    let mut hasher = Sha1::new();
    let mut pieces = Vec::new();
    let mut buffer = vec![0u8; PIECE_LENGTH];
    let mut buf_idx = 0;
    let mut total_length = 0i64;

    // Simulate a continuous stream across multiple files.
    // BitTorrent treats a multi-file torrent as one long string of bytes.
    for file_path in &files {
        let mut f = File::open(file_path)?;
        let file_len = f.metadata()?.len() as i64;
        total_length += file_len;

        let mut bytes_left = file_len;
        while bytes_left > 0 {
            // Fill the buffer until it hits 256KB or the file ends
            let space_in_buf = PIECE_LENGTH - buf_idx;
            let read_len = std::cmp::min(space_in_buf as i64, bytes_left) as usize;

            f.read_exact(&mut buffer[buf_idx..buf_idx + read_len])?;

            buf_idx += read_len;
            bytes_left -= read_len as i64;

            // If buffer is full, hash it and reset
            if buf_idx == PIECE_LENGTH {
                hasher.update(&buffer);
                pieces.extend_from_slice(&hasher.finalize_reset());
                buf_idx = 0;
            }
        }
    }

    // Hash remaining bytes (the final partial piece)
    if buf_idx > 0 {
        hasher.update(&buffer[..buf_idx]);
        pieces.extend_from_slice(&hasher.finalize_reset());
    }

    // --- 3. Build Info Structure ---
    let info = if is_single_file {
        Info {
            name,
            piece_length: PIECE_LENGTH,
            pieces: serde_bytes::ByteBuf::from(pieces),
            length: Some(total_length),
            files: None,
        }
    } else {
        // For multi-file torrents, we calculate paths relative to the root folder
        let file_nodes: Vec<FileNode> = files
            .iter()
            .map(|f| {
                let relative = f.strip_prefix(path).unwrap();
                let path_parts = relative
                    .iter()
                    .map(|s| s.to_str().unwrap().to_string())
                    .collect();

                FileNode {
                    length: f.metadata().unwrap().len() as i64,
                    path: path_parts,
                }
            })
            .collect();

        Info {
            name,
            piece_length: PIECE_LENGTH,
            pieces: serde_bytes::ByteBuf::from(pieces),
            length: None,
            files: Some(file_nodes),
        }
    };

    // --- 4. Build & Save Torrent ---
    let torrent = Torrent {
        announce: announce_url.to_string(),
        announce_list: None,
        info,
    };

    let bencoded = serde_bencode::to_bytes(&torrent)?;
    let mut out = File::create(output_path)?;
    out.write_all(&bencoded)?;

    println!("Torrent created successfully: {}", output_path);
    Ok(())
}
