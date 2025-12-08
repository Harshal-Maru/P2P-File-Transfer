use anyhow::{Context, Result};
use tokio::io::AsyncReadExt;

/// Represents the standard messages of the BitTorrent Peer Wire Protocol.
///
/// Messages generally follow the format: `<Length Prefix><Message ID><Payload>`.
/// - **Length Prefix**: 4-byte big-endian integer.
/// - **Message ID**: 1-byte identifier.
/// - **Payload**: Variable length data depending on the message type.
#[derive(Debug, PartialEq)]
pub enum Message {
    /// Keep-alive message (0-byte length prefix, no ID). Used to prevent timeouts.
    KeepAlive,
    /// Signals that the peer is choking the receiver (stopping upload).
    Choke,
    /// Signals that the peer is unchoking the receiver (allowing upload).
    Unchoke,
    /// Signals that the sender is interested in the receiver's data.
    Interested,
    /// Signals that the sender is not interested in the receiver's data.
    NotInterested,
    /// Notifies that the sender has successfully downloaded a specific piece.
    Have { index: u32 },
    /// Sent immediately after handshake, representing the pieces the peer currently has.
    Bitfield(Vec<u8>),
    /// Requests a specific block of data from a piece.
    Request { index: u32, begin: u32, length: u32 },
    /// Contains the actual block of data requested.
    Piece {
        index: u32,
        begin: u32,
        block: Vec<u8>,
    },
}

impl Message {
    /// Serializes the message into raw bytes suitable for sending over a TCP stream.
    ///
    /// Follows the Big-Endian convention specified by the BitTorrent protocol.
    pub fn serialize(&self) -> Vec<u8> {
        match self {
            Message::KeepAlive => {
                // KeepAlive is simply 4 bytes of zeros (Length = 0)
                vec![0, 0, 0, 0]
            }
            Message::Choke => {
                // Length: 1, ID: 0
                vec![0, 0, 0, 1, 0]
            }
            Message::Unchoke => {
                // Length: 1, ID: 1
                vec![0, 0, 0, 1, 1]
            }
            Message::Interested => {
                // Length: 1, ID: 2
                vec![0, 0, 0, 1, 2]
            }
            Message::NotInterested => {
                // Length: 1, ID: 3
                vec![0, 0, 0, 1, 3]
            }
            Message::Have { index } => {
                // Length: 5 (1 byte ID + 4 bytes index), ID: 4
                let mut bytes = vec![0, 0, 0, 5, 4];
                bytes.extend_from_slice(&index.to_be_bytes());
                bytes
            }
            Message::Bitfield(payload) => {
                let len = 1 + payload.len() as u32;
                let mut bytes = Vec::with_capacity(4 + len as usize);

                bytes.extend_from_slice(&len.to_be_bytes());
                bytes.push(5); // ID: 5
                bytes.extend_from_slice(payload);
                bytes
            }
            Message::Request {
                index,
                begin,
                length,
            } => {
                // Length: 13 (1 ID + 4 index + 4 begin + 4 length), ID: 6
                let mut bytes = vec![0, 0, 0, 13, 6];
                bytes.extend_from_slice(&index.to_be_bytes());
                bytes.extend_from_slice(&begin.to_be_bytes());
                bytes.extend_from_slice(&length.to_be_bytes());
                bytes
            }
            Message::Piece {
                index,
                begin,
                block,
            } => {
                // Length: 1 (ID) + 4 (index) + 4 (begin) + block length
                let len = 1 + 4 + 4 + block.len() as u32;

                let mut bytes = Vec::with_capacity(4 + len as usize);
                bytes.extend_from_slice(&len.to_be_bytes()); // Length Prefix
                bytes.push(7); // ID: 7
                bytes.extend_from_slice(&index.to_be_bytes());
                bytes.extend_from_slice(&begin.to_be_bytes());
                bytes.extend_from_slice(block);
                bytes
            }
        }
    }

    /// Reads a single message from an async byte stream (e.g., TcpStream).
    ///
    /// This method handles framing by first reading the length prefix, buffering
    /// the exact amount of payload data, and then parsing the message type.
    pub async fn read<T: AsyncReadExt + Unpin>(stream: &mut T) -> Result<Self> {
        // 1. Read the Length Prefix (4 bytes)
        let mut length_buf = [0u8; 4];
        stream
            .read_exact(&mut length_buf)
            .await
            .context("Failed to read message length prefix")?;

        let length = u32::from_be_bytes(length_buf);

        // 2. Handle KeepAlive (Length 0)
        if length == 0 {
            return Ok(Message::KeepAlive);
        }

        // 3. Read the Message ID (1 byte)
        let mut id_buf = [0u8; 1];
        stream
            .read_exact(&mut id_buf)
            .await
            .context("Failed to read message ID")?;
        let id = id_buf[0];

        // 4. Read the Payload (Length - 1 byte for ID)
        let payload_len = (length - 1) as usize;
        let mut payload = vec![0u8; payload_len];
        if payload_len > 0 {
            stream
                .read_exact(&mut payload)
                .await
                .context("Failed to read message payload")?;
        }

        // 5. Parse the Payload based on ID
        match id {
            0 => Ok(Message::Choke),
            1 => Ok(Message::Unchoke),
            2 => Ok(Message::Interested),
            3 => Ok(Message::NotInterested),
            4 => {
                // Have: payload is 4 bytes (index)
                if payload.len() != 4 {
                    anyhow::bail!("Invalid payload length for Have message");
                }
                let index = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
                Ok(Message::Have { index })
            }
            5 => Ok(Message::Bitfield(payload)),
            6 => {
                // Request: 12 bytes (index, begin, length)
                if payload.len() != 12 {
                    anyhow::bail!("Invalid payload length for Request message");
                }
                let index = u32::from_be_bytes(payload[0..4].try_into()?);
                let begin = u32::from_be_bytes(payload[4..8].try_into()?);
                let length = u32::from_be_bytes(payload[8..12].try_into()?);
                Ok(Message::Request {
                    index,
                    begin,
                    length,
                })
            }
            7 => {
                // Piece: 4 bytes index + 4 bytes begin + Block data
                if payload.len() < 8 {
                    anyhow::bail!("Invalid payload length for Piece message");
                }
                let index = u32::from_be_bytes(payload[0..4].try_into()?);
                let begin = u32::from_be_bytes(payload[4..8].try_into()?);
                let block = payload[8..].to_vec();
                Ok(Message::Piece {
                    index,
                    begin,
                    block,
                })
            }
            _ => {
                // Unknown ID (possibly Extension Protocol handshake, which we don't support yet)
                anyhow::bail!("Unknown message ID: {}", id);
            }
        }
    }
}
