use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};

use crate::{
    protocol::{Handshake, PeerMessage},
    storage::PositionedFile,
    verify::verify_v1_piece,
};

pub fn download_single_v1_piece(
    peer: SocketAddr,
    handshake: &Handshake,
    piece_index: u32,
    expected_hash: &[u8; 20],
    piece_length: u32,
    destination: &PositionedFile,
    destination_offset: u64,
) -> Result<(), String> {
    if piece_length == 0 || piece_length > 16 * 1024 {
        return Err("single-block piece length must be between 1 and 16384".into());
    }
    let mut stream = TcpStream::connect_timeout(&peer, Duration::from_secs(10))
        .map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| error.to_string())?;
    let remote = Handshake::exchange(&mut stream, handshake).map_err(|error| error.to_string())?;
    if remote.info_hash != handshake.info_hash {
        return Err("peer returned a different info hash".into());
    }
    write_message(&mut stream, &PeerMessage::Interested)?;
    if read_message(&mut stream)? != PeerMessage::Unchoke {
        return Err("peer did not unchoke the client".into());
    }
    write_message(
        &mut stream,
        &PeerMessage::Request {
            index: piece_index,
            begin: 0,
            length: piece_length,
        },
    )?;
    let PeerMessage::Piece {
        index,
        begin,
        block,
    } = read_message(&mut stream)?
    else {
        return Err("peer did not return a piece".into());
    };
    if index != piece_index || begin != 0 || block.len() != piece_length as usize {
        return Err("peer returned an unexpected piece block".into());
    }
    if !verify_v1_piece(&block, expected_hash) {
        return Err("piece hash verification failed".into());
    }
    destination
        .write_all_at(destination_offset, &block)
        .map_err(|error| error.to_string())
}

fn write_message(stream: &mut TcpStream, message: &PeerMessage) -> Result<(), String> {
    stream
        .write_all(&message.encode())
        .map_err(|error| error.to_string())
}

fn read_message(stream: &mut TcpStream) -> Result<PeerMessage, String> {
    let mut length = [0; 4];
    stream
        .read_exact(&mut length)
        .map_err(|error| error.to_string())?;
    let payload_length = u32::from_be_bytes(length) as usize;
    if payload_length > 2 * 1024 * 1024 {
        return Err("peer message exceeds configured limit".into());
    }
    let mut encoded = Vec::with_capacity(payload_length + 4);
    encoded.extend(length);
    encoded.resize(payload_length + 4, 0);
    stream
        .read_exact(&mut encoded[4..])
        .map_err(|error| error.to_string())?;
    PeerMessage::decode(&encoded, 2 * 1024 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::HANDSHAKE_LENGTH;
    use sha1::{Digest, Sha1};
    use std::{
        fs,
        io::{Read, Write},
        net::TcpListener,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn downloads_verifies_and_commits_one_piece_from_local_peer() {
        let payload = b"verified piece".to_vec();
        let expected_hash: [u8; 20] = Sha1::digest(&payload).into();
        let handshake = Handshake {
            reserved: [0; 8],
            info_hash: [9; 20],
            peer_id: *b"-PL0001-client000001",
        };
        let server_handshake = handshake.clone();
        let server_payload = payload.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = [0; HANDSHAKE_LENGTH];
            stream.read_exact(&mut received).unwrap();
            assert_eq!(
                Handshake::decode(&received).unwrap().info_hash,
                server_handshake.info_hash
            );
            stream.write_all(&server_handshake.encode()).unwrap();
            assert_eq!(read_message(&mut stream).unwrap(), PeerMessage::Interested);
            write_message(&mut stream, &PeerMessage::Unchoke).unwrap();
            assert!(matches!(
                read_message(&mut stream).unwrap(),
                PeerMessage::Request {
                    index: 0,
                    begin: 0,
                    ..
                }
            ));
            write_message(
                &mut stream,
                &PeerMessage::Piece {
                    index: 0,
                    begin: 0,
                    block: server_payload,
                },
            )
            .unwrap();
        });
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("peerlume-transfer-{nonce}"));
        let destination = PositionedFile::create(&root, "piece.bin", payload.len() as u64).unwrap();
        download_single_v1_piece(
            address,
            &handshake,
            0,
            &expected_hash,
            payload.len() as u32,
            &destination,
            0,
        )
        .unwrap();
        let mut actual = vec![0; payload.len()];
        destination.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(actual, payload);
        server.join().unwrap();
        drop(destination);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_piece_is_never_committed() {
        let expected_hash: [u8; 20] = Sha1::digest(b"expected").into();
        let handshake = Handshake {
            reserved: [0; 8],
            info_hash: [8; 20],
            peer_id: *b"-PL0001-client000002",
        };
        let server_handshake = handshake.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = [0; HANDSHAKE_LENGTH];
            stream.read_exact(&mut received).unwrap();
            stream.write_all(&server_handshake.encode()).unwrap();
            let _ = read_message(&mut stream).unwrap();
            write_message(&mut stream, &PeerMessage::Unchoke).unwrap();
            let _ = read_message(&mut stream).unwrap();
            write_message(
                &mut stream,
                &PeerMessage::Piece {
                    index: 0,
                    begin: 0,
                    block: b"corrupt!".to_vec(),
                },
            )
            .unwrap();
        });
        let root = std::env::temp_dir().join("peerlume-corrupt-piece-test");
        let _ = fs::remove_dir_all(&root);
        let destination = PositionedFile::create(&root, "piece.bin", 8).unwrap();
        assert!(
            download_single_v1_piece(address, &handshake, 0, &expected_hash, 8, &destination, 0)
                .is_err()
        );
        let mut actual = [1; 8];
        destination.read_exact_at(0, &mut actual).unwrap();
        assert_eq!(actual, [0; 8]);
        server.join().unwrap();
        drop(destination);
        fs::remove_dir_all(root).unwrap();
    }
}
