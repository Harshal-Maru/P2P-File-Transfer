# P2P File Transfer Engine (Rust)

A multi-threaded BitTorrent client implemented from scratch in Rust.

I built this project to able to understand how Peer to Peer System Works. It implements the BitTorrent Wire Protocol (TCP) and communicates with trackers (UDP/HTTP) to discover peers and transfer files. It supports creating, downloading, and seeding `.torrent` files.

## Features

- **Custom Protocol Implementation:** Implements the BitTorrent handshake, bitfield exchange, and piece pipelining logic manually.
- **High-Performance Discovery:** Uses a “scatter–gather” approach to query multiple trackers concurrently, significantly reducing peer discovery time.
- **Resilience:** Handles End Game scenarios, stalls, and disconnects. If a peer drops connection, the pending work is reassigned.
- **Data Integrity:** Validates every downloaded piece against SHA-1 hashes.
- **Zero-Corruption Resume:** Pre-allocates files and syncs metadata so downloads can be stopped and resumed safely.
- **CLI Interface:** Supports creating torrents, downloading, and seeding.

## Installation

Ensure you have Rust and Cargo installed: https://rustup.rs/

```bash
git clone https://github.com/your-username/p2p-file-transfer.git
cd p2p-file-transfer
cargo build --release
```

## Usage

The application runs in three modes: **create**, **download**, and **seed**.

### 1. Create a Torrent

Converts a file or folder into a `.torrent` file. Uses opentrackr.org as the default tracker.

```bash
cargo run --release -- create <input_path> <output_name.torrent>
```

### 2. Download a Torrent

Downloads content to the `downloads/` directory. Automatically resumes if partial data already exists.

```bash
cargo run --release -- download <file.torrent>
```

### 3. Seed a Torrent

Acts as a dedicated seeder.

```bash
cargo run --release -- seed <file.torrent>
```

## Architecture

- **main.rs:** CLI parsing and runtime setup.
- **core/manager.rs:** Central coordinator and disk-writer.
- **network/mod.rs:** Peer TCP session lifecycle + pipelining.
- **network/message.rs:** BitTorrent wire message serializers.
- **core/tracker.rs:** UDP/HTTP tracker communication.

## Technical Details

- **Concurrency:** tokio async runtime.
- **Disk I/O:** `set_len` sparse allocation + `sync_all`.
- **Serialization:** `serde` + `serde_bencode`.

## Future Improvements

- Magnet link & DHT support.
- Upload throttling.
- Selective file download.