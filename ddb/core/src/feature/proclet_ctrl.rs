use anyhow::Result;
use bytes::{BufMut, Bytes, BytesMut};
use dashmap::DashMap;
use std::sync::Arc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{tcp, TcpStream},
    sync::oneshot,
};
use tracing::{debug, error, info};

use super::next_rpc_req_id;

type RPCToken = u64;

#[derive(Debug)]
pub enum ProcletCtrlCmd {
    Query(u64 /* proclet_id */),
}

#[derive(Debug, Clone)]
pub enum ProcletCtrlCmdResp {
    QueryResp(u64 /* proclet_id */, u32 /* Caladan IP addr */),
    Empty,
}

impl ProcletCtrlCmdResp {
    fn from_hdr(hdr: &ProcletCtrlHdr, bytes: Bytes) -> Self {
        match hdr.cmd {
            0x01 => {
                // Query response
                let proclet_id_size = std::mem::size_of::<u64>();
                let caladan_ip_size = std::mem::size_of::<u32>();
                let proclet_id = u64::from_be_bytes(bytes[0..proclet_id_size].try_into().unwrap());
                let caladan_ip = u32::from_be_bytes(
                    bytes[proclet_id_size..proclet_id_size + caladan_ip_size]
                        .try_into()
                        .unwrap(),
                );
                ProcletCtrlCmdResp::QueryResp(proclet_id, caladan_ip)
            }
            _ => ProcletCtrlCmdResp::Empty,
        }
    }
}

#[derive(Debug)]
// #[repr(packed)]
pub struct ProcletCtrlHdr {
    pub cmd: u32,
    pub len: u32,   // length of the following payload
    pub token: u64, // for completion handler
}

impl ProcletCtrlHdr {
    fn from_bytes(bytes: &[u8]) -> Self {
        // follow big-endian convention
        let cmd_size = std::mem::size_of::<u32>();
        let len_size = std::mem::size_of::<u32>();
        let token_size = std::mem::size_of::<u64>();
        let mut start = 0;
        let mut end = cmd_size;
        let cmd = u32::from_be_bytes(bytes[0..end].try_into().unwrap());
        start = end;
        end += len_size;
        let len = u32::from_be_bytes(bytes[start..end].try_into().unwrap());
        start = end;
        end += token_size;
        let token = u64::from_be_bytes(bytes[start..end].try_into().unwrap());

        ProcletCtrlHdr { cmd, len, token }
    }
}

const HDR_SIZE: usize =
    std::mem::size_of::<u32>() + std::mem::size_of::<u32>() + std::mem::size_of::<u64>();

impl ProcletCtrlCmd {
    fn to_hdr(&self) -> (RPCToken, ProcletCtrlHdr) {
        let token = next_rpc_req_id();

        let hdr = match self {
            ProcletCtrlCmd::Query(proclet_id) => ProcletCtrlHdr {
                cmd: 0x01,
                len: std::mem::size_of_val(proclet_id) as u32,
                token,
            },
        };
        (token, hdr)
    }

    fn to_bytes(&self) -> (RPCToken, Bytes) {
        // Ensure the header size is correct, in the case of Rust API changes.
        // idk, but Rust ABI is rather unstable...
        // assert_eq!(std::mem::size_of::<ProcletCtrlHdr>(), HDR_SIZE);
        let (token, hdr) = self.to_hdr();

        let mut bytes = BytesMut::with_capacity(HDR_SIZE + hdr.len as usize);

        // CAREFUL! Follow big-endian convention.
        bytes.put_u32(hdr.cmd);
        bytes.put_u32(hdr.len);
        bytes.put_u64(hdr.token);

        match self {
            ProcletCtrlCmd::Query(proclet_id) => {
                bytes.put_u64(*proclet_id);
            }
        }
        (token, bytes.freeze())
    }
}

#[derive(Debug)]
pub struct QueryProcletResp {
    pub proclet_id: u64,
    pub caladan_ip: u32,
}

pub struct ProcletCtrlClient {
    to_send: flume::Sender<Bytes>,
    sender_handle: tokio::task::JoinHandle<()>,
    receiver_handle: tokio::task::JoinHandle<()>,
    inflights: Arc<DashMap<RPCToken, oneshot::Sender<ProcletCtrlCmdResp>>>,
}

impl ProcletCtrlClient {
    /// Creates a new ProcletCtrlClient and connects to the specified address.
    pub async fn try_new(addr: &str) -> Result<Self> {
        let inflights = Arc::new(DashMap::new());
        let stream = TcpStream::connect(addr).await?;
        let (r, w) = stream.into_split();

        let (to_send, sender_recv) = flume::unbounded();
        let sender_handle = Self::start_sender(w, sender_recv);
        let receiver_handle = Self::start_receiver(r, inflights.clone());

        // Optionally, configure the stream further (e.g., timeouts)
        // stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        // stream.set_write_timeout(Some(Duration::from_secs(5)))?;

        Ok(Self {
            to_send,
            sender_handle,
            receiver_handle,
            inflights,
        })
    }

    fn start_sender(
        mut stream_writer: tcp::OwnedWriteHalf,
        sender_recv: flume::Receiver<Bytes>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match sender_recv.recv_async().await {
                    Ok(data) => match stream_writer.write_all(&data).await {
                        Ok(_) => {}
                        Err(e) => {
                            error!("Failed to send data: {}", e);
                            break;
                        }
                    },
                    Err(_) => {
                        error!("Receiver closed");
                        break;
                    }
                }
            }
        })
    }

    async fn read_payload(
        hdr: &ProcletCtrlHdr,
        stream_reader: &mut tcp::OwnedReadHalf,
    ) -> Result<ProcletCtrlCmdResp> {
        let payload_len = hdr.len as usize;
        if payload_len > 0 {
            let mut payload_buf = BytesMut::zeroed(payload_len);
            match stream_reader.read_exact(&mut payload_buf).await {
                Ok(len) => {
                    assert_eq!(len, payload_len);
                    debug!("Received Payload ({} bytes)", payload_len);

                    let payload_buf = payload_buf.freeze();
                    return Ok(ProcletCtrlCmdResp::from_hdr(&hdr, payload_buf));
                }
                Err(e) => {
                    return Err(e.into());
                }
            }
        } else {
            debug!("Received header with zero payload length.");
            return Ok(ProcletCtrlCmdResp::Empty);
        }
    }

    fn start_receiver(
        mut stream_reader: tcp::OwnedReadHalf,
        inflights: Arc<DashMap<u64, oneshot::Sender<ProcletCtrlCmdResp>>>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut hdr_buf = [0u8; HDR_SIZE];
            loop {
                match stream_reader.read_exact(&mut hdr_buf).await {
                    Ok(0) => {
                        error!("Connection closed");
                        break;
                    }
                    Ok(len) => {
                        assert_eq!(len, HDR_SIZE);
                        let header = ProcletCtrlHdr::from_bytes(&hdr_buf);
                        debug!("Received Header: {:?}", header);

                        match Self::read_payload(&header, &mut stream_reader).await {
                            Ok(resp) => {
                                if let Some((_token, sender)) = inflights.remove(&header.token) {
                                    match sender.send(resp) {
                                        Ok(_) => info!("Response sent for token {}", header.token),
                                        Err(_) => error!("Failed to send response: receiver dropped for token {}", header.token),
                                    }
                                } else {
                                    error!("No inflight request found for token {}", header.token);
                                }
                            }
                            Err(e) => {
                                error!("Failed to read payload: {}", e);
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        error!("Failed to read header data: {}", e);
                        break;
                    }
                }
            }
        })
    }

    pub async fn try_connect_default() -> Result<Self> {
        Self::try_new("127.0.0.1:20202").await
    }

    pub async fn send_command(&self, cmd: ProcletCtrlCmd) -> Result<ProcletCtrlCmdResp> {
        let (token, data) = cmd.to_bytes();
        let (sender, receiver) = oneshot::channel();
        self.inflights.insert(token, sender);
        self.to_send.send_async(data).await?;
        Ok(receiver.await?)
    }

    pub async fn query_proclet(&self, proclet_id: u64) -> Result<QueryProcletResp> {
        let cmd = ProcletCtrlCmd::Query(proclet_id);
        let resp = self.send_command(cmd).await?;
        match resp {
            ProcletCtrlCmdResp::QueryResp(proc_id, caladan_ip) => {
                debug!("Proclet ID: {}, Caladan IP: {}", proc_id, caladan_ip);
                // some sanity checks...
                assert_eq!(
                    proclet_id,
                    proc_id,
                    "Proclet ID mismatch: expected {}, got {}",
                    proclet_id,
                    proc_id
                );
                Ok(QueryProcletResp { proclet_id, caladan_ip })
            }
            _ => {
                error!("Unexpected response type");
                Err(anyhow::anyhow!("Unexpected response type"))
            }
        }
    }
}
