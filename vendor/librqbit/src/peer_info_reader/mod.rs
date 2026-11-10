use std::{net::SocketAddr, sync::Arc};

use bencode::from_bytes;
use buffers::{ByteBuf, ByteBufOwned};
use bytes::Bytes;
use librqbit_core::{
    constants::CHUNK_SIZE,
    hash_id::Id20,
    lengths::{ChunkInfo, last_element_size},
    torrent_metainfo::TorrentMetaV1Info,
};
use parking_lot::{Mutex, RwLock};
use peer_binary_protocol::{
    Handshake, Message,
    extended::{
        ExtendedMessage,
        handshake::ExtendedHandshake,
        ut_metadata::{UtMetadata, UtMetadataData},
    },
};
use sha1w::{ISha1, Sha1};
use tokio::sync::mpsc::UnboundedSender;
use tracing::trace;

use crate::{
    peer_connection::{
        PeerConnection, PeerConnectionHandler, PeerConnectionOptions, WriterRequest,
    },
    spawn_utils::BlockingSpawner,
    stream_connect::{ConnectionKind, StreamConnector},
};

pub(crate) async fn read_metainfo_from_peer(
    addr: SocketAddr,
    peer_id: Id20,
    info_hash: Id20,
    peer_connection_options: Option<PeerConnectionOptions>,
    spawner: BlockingSpawner,
    connector: Arc<StreamConnector>,
    client_name_and_version: String,
) -> anyhow::Result<TorrentAndInfoBytes> {
    let bytes = read_metadata_bytes_from_peer(
        MetadataPeer {
            addr,
            peer_id,
            info_hash,
            max_bytes: 32 * 1024 * 1024,
            preview: false,
        },
        peer_connection_options,
        spawner,
        connector,
        client_name_and_version,
    )
    .await?;
    use clone_to_owned::CloneToOwned;
    let info = from_bytes::<TorrentMetaV1Info<ByteBuf>>(&bytes.0)
        .map_err(|error| error.into_kind())?
        .clone_to_owned(Some(&bytes.0));
    Ok((info, bytes))
}

pub(crate) struct MetadataPeer {
    pub addr: SocketAddr,
    pub peer_id: Id20,
    pub info_hash: Id20,
    pub max_bytes: u32,
    pub preview: bool,
}

pub(crate) async fn read_metadata_bytes_from_peer(
    request: MetadataPeer,
    peer_connection_options: Option<PeerConnectionOptions>,
    spawner: BlockingSpawner,
    connector: Arc<StreamConnector>,
    client_name_and_version: String,
) -> anyhow::Result<ByteBufOwned> {
    let MetadataPeer {
        addr,
        peer_id,
        info_hash,
        max_bytes,
        preview,
    } = request;
    let (result_tx, result_rx) = tokio::sync::oneshot::channel::<ByteBufOwned>();
    let (writer_tx, writer_rx) = tokio::sync::mpsc::unbounded_channel::<WriterRequest>();
    let handler = Handler {
        addr,
        info_hash,
        writer_tx,
        result_tx: Mutex::new(Some(result_tx)),
        locked: RwLock::new(None),
        client_name_and_version,
        max_bytes,
        preview,
    };
    let connection = PeerConnection::new(
        addr,
        info_hash,
        peer_id,
        handler,
        peer_connection_options,
        spawner,
        connector,
    );

    let result_reader = result_rx;
    let (_, brx) = tokio::sync::broadcast::channel(1);
    let connection_runner = async move { connection.manage_peer_outgoing(writer_rx, brx).await };

    tokio::select! {
        result = result_reader => Ok(result?),
        whatever = connection_runner => match whatever {
            Ok(_) => anyhow::bail!("connection runner completed first"),
            Err(e) => Err(e.into())
        }
    }
}

#[derive(Default)]
struct HandlerLocked {
    metadata_size: u32,
    total_pieces: usize,
    buffer: Vec<u8>,
    received_pieces: Vec<bool>,
}

impl HandlerLocked {
    fn new(metadata_size: u32, max_bytes: u32) -> anyhow::Result<Self> {
        if metadata_size == 0 || metadata_size > max_bytes {
            anyhow::bail!("metadata size {} is too big", metadata_size);
        }
        let buffer = vec![0u8; metadata_size as usize];
        let total_pieces: usize = (metadata_size as u64)
            .div_ceil(CHUNK_SIZE as u64)
            .try_into()?;
        let received_pieces = vec![false; total_pieces];
        Ok(Self {
            metadata_size,
            received_pieces,
            buffer,
            total_pieces,
        })
    }
    fn piece_size(&self, index: u32) -> usize {
        if index as usize == self.total_pieces - 1 {
            last_element_size(self.metadata_size as u64, CHUNK_SIZE as u64)
                .try_into()
                .unwrap()
        } else {
            CHUNK_SIZE as usize
        }
    }
    fn record_piece(
        &mut self,
        d: &UtMetadataData<ByteBuf>,
        info_hash: &Id20,
    ) -> anyhow::Result<bool> {
        let piece = d.piece();
        if piece as usize >= self.total_pieces {
            anyhow::bail!("wrong index");
        }
        let offset = (piece * CHUNK_SIZE) as usize;
        let size = self.piece_size(piece);
        if d.len() != size {
            anyhow::bail!(
                "expected length of piece {} to be {}, but got {}",
                piece,
                size,
                d.len()
            );
        }
        if self.received_pieces[piece as usize] {
            anyhow::bail!("already received piece {}", piece);
        }
        d.copy_to_slice(&mut self.buffer[offset..offset + d.len()]);
        self.received_pieces[piece as usize] = true;

        if self.received_pieces.iter().all(|p| *p) {
            // check metadata
            let mut hash = Sha1::new();
            hash.update(&self.buffer);
            if hash.finish() != info_hash.0 {
                anyhow::bail!("info checksum invalid");
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

pub type TorrentAndInfoBytes = (TorrentMetaV1Info<ByteBufOwned>, ByteBufOwned);

struct Handler {
    addr: SocketAddr,
    info_hash: Id20,
    writer_tx: UnboundedSender<WriterRequest>,
    result_tx: Mutex<Option<tokio::sync::oneshot::Sender<ByteBufOwned>>>,
    locked: RwLock<Option<HandlerLocked>>,
    client_name_and_version: String,
    max_bytes: u32,
    preview: bool,
}

impl PeerConnectionHandler for Handler {
    fn should_send_bitfield(&self) -> bool {
        false
    }

    fn should_send_initial_unchoke(&self) -> bool {
        !self.preview
    }

    fn serialize_bitfield_message_to_buf(&self, _buf: &mut [u8]) -> anyhow::Result<usize> {
        Ok(0)
    }

    fn on_handshake(&self, handshake: Handshake, _kind: ConnectionKind) -> anyhow::Result<()> {
        if !handshake.supports_extended() {
            anyhow::bail!(
                "this peer does not support extended handshaking, which is a prerequisite to download metadata"
            )
        }
        Ok(())
    }

    async fn on_received_message(&self, msg: Message<'_>) -> anyhow::Result<()> {
        trace!("{}: received message: {:?}", self.addr, msg);

        if let Message::Extended(ExtendedMessage::UtMetadata(UtMetadata::Data(utdata))) = msg {
            let mut locked = self.locked.write();
            let piece_ready = locked
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("unexpected metadata data before negotiation"))?
                .record_piece(&utdata, &self.info_hash)?;
            if piece_ready {
                let buf = Bytes::from(locked.take().expect("validated metadata buffer").buffer);

                self.result_tx
                    .lock()
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("oneshot is consumed"))?
                    .send(ByteBufOwned(buf))
                    .map_err(|_| {
                        anyhow::anyhow!("torrent info deserialized, but consumer closed")
                    })?;
            }
        }
        Ok(())
    }

    fn on_uploaded_bytes(&self, _bytes: u32) {}

    fn read_chunk(&self, _chunk: &ChunkInfo, _buf: &mut [u8]) -> anyhow::Result<()> {
        anyhow::bail!("the peer is not supposed to be requesting chunks")
    }

    fn on_extended_handshake(
        &self,
        extended_handshake: &ExtendedHandshake<ByteBuf>,
    ) -> anyhow::Result<()> {
        let metadata_size = match extended_handshake.metadata_size {
            Some(metadata_size) => metadata_size,
            None => anyhow::bail!("peer does not have metadata_size"),
        };

        if !extended_handshake.m.ut_metadata.is_some_and(|id| id > 0) {
            anyhow::bail!("peer does not support ut_metadata");
        }

        let mut locked = self.locked.write();
        if locked.is_some() || self.result_tx.lock().is_none() {
            anyhow::bail!("duplicate metadata negotiation");
        }
        if !self.preview {
            self.writer_tx
                .send(WriterRequest::Message(Message::Unchoke))?;
            self.writer_tx
                .send(WriterRequest::Message(Message::Interested))?;
        }

        let inner = HandlerLocked::new(metadata_size, self.max_bytes)?;
        let total_pieces = inner.total_pieces;

        locked.replace(inner);

        for i in 0..total_pieces {
            self.writer_tx
                .send(WriterRequest::Message(Message::Extended(
                    ExtendedMessage::UtMetadata(UtMetadata::Request(i.try_into()?)),
                )))?;
        }
        Ok(())
    }

    fn update_my_extended_handshake(
        &self,
        handshake: &mut ExtendedHandshake<ByteBuf>,
    ) -> anyhow::Result<()> {
        if self.preview {
            // BEP 10 uses zero to disable an extension. This pinned
            // serializer also requires an integer for each mapping.
            handshake.m.ut_pex = Some(0);
        }
        Ok(())
    }

    fn should_transmit_have(&self, _id: librqbit_core::lengths::ValidPieceIndex) -> bool {
        false
    }

    fn client_name_and_version(&self) -> &str {
        &self.client_name_and_version
    }
}
