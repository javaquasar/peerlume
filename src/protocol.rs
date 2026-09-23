use std::io::{self, Read, Write};

pub const HANDSHAKE_LENGTH: usize = 68;
const PROTOCOL_NAME: &[u8] = b"BitTorrent protocol";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handshake {
    pub reserved: [u8; 8],
    pub info_hash: [u8; 20],
    pub peer_id: [u8; 20],
}

impl Handshake {
    pub fn encode(&self) -> [u8; HANDSHAKE_LENGTH] {
        let mut output = [0; HANDSHAKE_LENGTH];
        output[0] = PROTOCOL_NAME.len() as u8;
        output[1..20].copy_from_slice(PROTOCOL_NAME);
        output[20..28].copy_from_slice(&self.reserved);
        output[28..48].copy_from_slice(&self.info_hash);
        output[48..68].copy_from_slice(&self.peer_id);
        output
    }

    pub fn decode(input: &[u8]) -> Result<Self, String> {
        if input.len() != HANDSHAKE_LENGTH
            || input[0] as usize != PROTOCOL_NAME.len()
            || &input[1..20] != PROTOCOL_NAME
        {
            return Err("invalid BitTorrent handshake".into());
        }
        Ok(Self {
            reserved: input[20..28].try_into().unwrap(),
            info_hash: input[28..48].try_into().unwrap(),
            peer_id: input[48..68].try_into().unwrap(),
        })
    }

    pub fn exchange(stream: &mut (impl Read + Write), outgoing: &Self) -> io::Result<Self> {
        stream.write_all(&outgoing.encode())?;
        stream.flush()?;
        let mut incoming = [0; HANDSHAKE_LENGTH];
        stream.read_exact(&mut incoming)?;
        Self::decode(&incoming).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PeerMessage {
    KeepAlive,
    Choke,
    Unchoke,
    Interested,
    NotInterested,
    Have(u32),
    Bitfield(Vec<u8>),
    Request {
        index: u32,
        begin: u32,
        length: u32,
    },
    Piece {
        index: u32,
        begin: u32,
        block: Vec<u8>,
    },
    Cancel {
        index: u32,
        begin: u32,
        length: u32,
    },
    Extended {
        extension_id: u8,
        payload: Vec<u8>,
    },
}

impl PeerMessage {
    pub fn encode(&self) -> Vec<u8> {
        let mut payload = Vec::new();
        match self {
            Self::KeepAlive => return vec![0, 0, 0, 0],
            Self::Choke => payload.push(0),
            Self::Unchoke => payload.push(1),
            Self::Interested => payload.push(2),
            Self::NotInterested => payload.push(3),
            Self::Have(index) => {
                payload.push(4);
                payload.extend(index.to_be_bytes());
            }
            Self::Bitfield(bits) => {
                payload.push(5);
                payload.extend(bits);
            }
            Self::Request {
                index,
                begin,
                length,
            } => {
                payload.push(6);
                payload.extend(index.to_be_bytes());
                payload.extend(begin.to_be_bytes());
                payload.extend(length.to_be_bytes());
            }
            Self::Piece {
                index,
                begin,
                block,
            } => {
                payload.push(7);
                payload.extend(index.to_be_bytes());
                payload.extend(begin.to_be_bytes());
                payload.extend(block);
            }
            Self::Cancel {
                index,
                begin,
                length,
            } => {
                payload.push(8);
                payload.extend(index.to_be_bytes());
                payload.extend(begin.to_be_bytes());
                payload.extend(length.to_be_bytes());
            }
            Self::Extended {
                extension_id,
                payload: extension_payload,
            } => {
                payload.push(20);
                payload.push(*extension_id);
                payload.extend(extension_payload);
            }
        }
        let mut output = Vec::with_capacity(payload.len() + 4);
        output.extend((payload.len() as u32).to_be_bytes());
        output.extend(payload);
        output
    }

    pub fn decode(input: &[u8], max_message_bytes: usize) -> Result<Self, String> {
        if input.len() < 4 {
            return Err("peer message is truncated".into());
        }
        let length = u32::from_be_bytes(input[..4].try_into().unwrap()) as usize;
        if length > max_message_bytes || input.len() != length + 4 {
            return Err("peer message length is invalid".into());
        }
        if length == 0 {
            return Ok(Self::KeepAlive);
        }
        let id = input[4];
        let body = &input[5..];
        let triple = || -> Result<(u32, u32, u32), String> {
            if body.len() != 12 {
                return Err("peer request length is invalid".into());
            }
            Ok((
                read_u32(&body[0..4]),
                read_u32(&body[4..8]),
                read_u32(&body[8..12]),
            ))
        };
        match id {
            0 if body.is_empty() => Ok(Self::Choke),
            1 if body.is_empty() => Ok(Self::Unchoke),
            2 if body.is_empty() => Ok(Self::Interested),
            3 if body.is_empty() => Ok(Self::NotInterested),
            4 if body.len() == 4 => Ok(Self::Have(read_u32(body))),
            5 => Ok(Self::Bitfield(body.to_vec())),
            6 => {
                let (index, begin, length) = triple()?;
                Ok(Self::Request {
                    index,
                    begin,
                    length,
                })
            }
            7 if body.len() >= 8 => Ok(Self::Piece {
                index: read_u32(&body[..4]),
                begin: read_u32(&body[4..8]),
                block: body[8..].to_vec(),
            }),
            8 => {
                let (index, begin, length) = triple()?;
                Ok(Self::Cancel {
                    index,
                    begin,
                    length,
                })
            }
            20 if !body.is_empty() => Ok(Self::Extended {
                extension_id: body[0],
                payload: body[1..].to_vec(),
            }),
            _ => Err("unsupported or malformed peer message".into()),
        }
    }
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use std::net::{TcpListener, TcpStream};

    use super::*;

    #[test]
    fn peer_messages_round_trip() {
        let messages = [
            PeerMessage::KeepAlive,
            PeerMessage::Have(7),
            PeerMessage::Request {
                index: 2,
                begin: 16,
                length: 16_384,
            },
            PeerMessage::Piece {
                index: 2,
                begin: 16,
                block: vec![1, 2, 3],
            },
            PeerMessage::Extended {
                extension_id: 3,
                payload: b"d1:md11:ut_metadatai3eee".to_vec(),
            },
        ];
        for message in messages {
            assert_eq!(
                PeerMessage::decode(&message.encode(), 1_000_000).unwrap(),
                message
            );
        }
    }

    #[test]
    fn local_peers_exchange_handshakes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut input = [0; HANDSHAKE_LENGTH];
            stream.read_exact(&mut input).unwrap();
            let received = Handshake::decode(&input).unwrap();
            stream.write_all(&received.encode()).unwrap();
        });
        let expected = Handshake {
            reserved: [0; 8],
            info_hash: [3; 20],
            peer_id: [4; 20],
        };
        let mut stream = TcpStream::connect(address).unwrap();
        let received = Handshake::exchange(&mut stream, &expected).unwrap();
        assert_eq!(received, expected);
        server.join().unwrap();
    }
}
