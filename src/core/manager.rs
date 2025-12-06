use std::sync::Mutex;
use crate::core::torrent_info::Torrent;

#[derive(Debug, Clone, PartialEq)]
pub enum PieceStatus {
    Pending,
    InProgress,
    Complete,
}

pub struct TorrentManager {
    pub torrent: Torrent,
    pub piece_status: Vec<PieceStatus>,
    pub downloaded_pieces: usize,
}

impl TorrentManager {
    pub fn new(torrent: Torrent) -> Self {
        let piece_count = torrent.info.pieces.len() / 20;
        Self {
            torrent,
            piece_status: vec![PieceStatus::Pending; piece_count],
            downloaded_pieces: 0,
        }
    }
    // We iterate through pieces. We pick one ONLY if:
    // 1. It is Pending
    // 2. The connected peer actually HAS it
    pub fn pick_next_piece(&mut self, peer_bitfield: &[bool]) -> Option<usize> {
        for (index, status) in self.piece_status.iter_mut().enumerate() {
            if *status == PieceStatus::Pending {
                // Check if the peer has this piece
                if index < peer_bitfield.len() && peer_bitfield[index] {
                    *status = PieceStatus::InProgress;
                    println!("Manager: Assigned Piece {} to a worker", index);
                    return Some(index);
                }
            }
        }
        None
    }

    pub fn mark_piece_complete(&mut self, index: usize) {
        if self.piece_status[index] != PieceStatus::Complete {
            self.piece_status[index] = PieceStatus::Complete;
            self.downloaded_pieces += 1;
            println!("Manager: Piece {} finished. Progress: {}/{}", 
                index, 
                self.downloaded_pieces, 
                self.piece_status.len()
            );
        }
    }

    pub fn reset_piece(&mut self, index: usize) {
        if self.piece_status[index] != PieceStatus::Complete {
            println!("Manager: Resetting Piece {} to Pending", index);
            self.piece_status[index] = PieceStatus::Pending;
        }
    }

    pub fn is_complete(&self) -> bool {
        self.downloaded_pieces == self.piece_status.len()
    }
}