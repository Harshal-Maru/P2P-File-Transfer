use tokio::io::AsyncReadExt; 
use anyhow::{Context, Result};


#[derive(Debug, PartialEq)]
pub enum Message {
    KeepAlive,
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have { index: u32 },
    Bitfield(Vec<u8>),
    Request { index: u32, begin: u32, length: u32 },
    Piece { index: u32, begin: u32, block: Vec<u8> },
}

impl Message {
    pub fn serialize(&self) -> Vec<u8> {
        match self {
            Message::KeepAlive => {
                // KeepAlive is just 4 bytes of zeros (Length = 0)
                vec![0, 0, 0, 0]
            },
            Message::Choke => {
                // Length: 1, ID: 0
                vec![0, 0, 0, 1, 0]
            },
            Message::Unchoke => {
                // Length: 1, ID: 1
                vec![0, 0, 0, 1, 1]
            },
            Message::Interested => {
                // Length: 1, ID: 2
                vec![0, 0, 0, 1, 2]
            },
            Message::NotInterested => {
                // Length: 1, ID: 3
                vec![0, 0, 0, 1, 3]
            },
            Message::Have { index } => {
                // Length: 5 (1 byte ID + 4 bytes index), ID: 4
                let mut bytes = vec![0, 0, 0, 5, 4];
                // BitTorrent uses Big Endian for all integers
                bytes.extend_from_slice(&index.to_be_bytes());
                bytes
            },
            Message::Request { index, begin, length } => {
                // Length: 13 (1 ID + 4 index + 4 begin + 4 length), ID: 6
                let mut bytes = vec![0, 0, 0, 13, 6];
                bytes.extend_from_slice(&index.to_be_bytes());
                bytes.extend_from_slice(&begin.to_be_bytes());
                bytes.extend_from_slice(&length.to_be_bytes());
                bytes
            },
            Message::Piece { index, begin, block } => {
                // Length: 1 (ID) + 4 (index) + 4 (begin) + block length
                let len = 1 + 4 + 4 + block.len() as u32;
                
                let mut bytes = Vec::with_capacity(4 + len as usize);
                bytes.extend_from_slice(&len.to_be_bytes()); // Length Prefix
                bytes.push(7); // ID for Piece
                bytes.extend_from_slice(&index.to_be_bytes());
                bytes.extend_from_slice(&begin.to_be_bytes());
                bytes.extend_from_slice(block);
                bytes
            },
            Message::Bitfield(payload) => {
                let mut bytes = vec![0, 0, 0, 0, 5]; // placeholder length
                // Calculate actual length: 1 (ID) + payload length
                let len = 1 + payload.len() as u32;
                // Overwrite the first 4 bytes with actual length
                bytes[0..4].copy_from_slice(&len.to_be_bytes());
                bytes.extend_from_slice(payload);
                bytes
            }
        }
    }

    // Async function to read a message from a generic byte stream (like TcpStream)
    pub async fn read<T: AsyncReadExt + Unpin>(stream: &mut T) -> Result<Self> {
        // 1. Read the Length Prefix (4 bytes)
        let mut length_buf = [0u8; 4];
        // read_exact waits until we have exactly 4 bytes
        stream.read_exact(&mut length_buf).await
            .context("Failed to read message length prefix")?;
        
        let length = u32::from_be_bytes(length_buf);

        // 2. Handle KeepAlive (Length 0)
        if length == 0 {
            return Ok(Message::KeepAlive);
        }

        // 3. Read the Message ID (1 byte)
        let mut id_buf = [0u8; 1];
        stream.read_exact(&mut id_buf).await
            .context("Failed to read message ID")?;
        let id = id_buf[0];

        // 4. Read the Payload (Length - 1 byte for ID)
        let payload_len = (length - 1) as usize;
        let mut payload = vec![0u8; payload_len];
        if payload_len > 0 {
            stream.read_exact(&mut payload).await
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
            },
            5 => {

                Ok(Message::Bitfield(payload)) 
            },
            6 => {
                 // Request: 12 bytes
                 if payload.len() != 12 {
                     anyhow::bail!("Invalid payload length for Request message");
                 }
                 let index = u32::from_be_bytes(payload[0..4].try_into()?);
                 let begin = u32::from_be_bytes(payload[4..8].try_into()?);
                 let length = u32::from_be_bytes(payload[8..12].try_into()?);
                 Ok(Message::Request { index, begin, length })
            },
            7 => {
                // Piece: 4 bytes index + 4 bytes begin + Block
                if payload.len() < 8 {
                    anyhow::bail!("Invalid payload length for Piece message");
                }
                let index = u32::from_be_bytes(payload[0..4].try_into()?);
                let begin = u32::from_be_bytes(payload[4..8].try_into()?);
                // The rest is the actual data
                let block = payload[8..].to_vec();
                Ok(Message::Piece { index, begin, block })
            },
            _ => {
                // Unknown ID (Extension protocols, etc.)
                anyhow::bail!("Unknown message ID: {}", id);
            }
        }
    }
}