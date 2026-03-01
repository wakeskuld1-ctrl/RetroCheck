use anyhow::{anyhow, Context, Result};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::config::EdgeGatewayConfig;
use crate::hub::edge_schema::{
    decode_auth_hello, decode_session_request, encode_auth_ack, encode_signed_response, AuthAck,
    EdgeRequest,
};
use crate::raft::router::Router;
use flatbuffers::FlatBufferBuilder;
use uuid::Uuid;

pub const HEADER_SIZE: usize = 21;
pub const MAGIC: [u8; 4] = *b"EDGE";
pub const VERSION: u8 = 1;
pub const MSG_TYPE_AUTH_HELLO: u8 = 1;
pub const MSG_TYPE_RESPONSE: u8 = 2;
pub const MSG_TYPE_SESSION_REQUEST: u8 = 3;

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Header {
    pub version: u8,
    pub msg_type: u8,
    pub request_id: u64,
    pub payload_len: u32,
    pub checksum: [u8; 3],
}

pub fn validate_header(buf: &[u8]) -> Result<Header> {
    if buf.len() != HEADER_SIZE {
        return Err(anyhow!("invalid header length"));
    }
    if buf[0..4] != MAGIC {
        return Err(anyhow!("invalid magic"));
    }
    let version = buf[4];
    if version != VERSION {
        return Err(anyhow!("invalid version"));
    }
    let msg_type = buf[5];
    let request_id = u64::from_be_bytes([
        buf[6], buf[7], buf[8], buf[9], buf[10], buf[11], buf[12], buf[13],
    ]);
    let payload_len = u32::from_be_bytes([buf[14], buf[15], buf[16], buf[17]]);
    let checksum = [buf[18], buf[19], buf[20]];
    Ok(Header {
        version,
        msg_type,
        request_id,
        payload_len,
        checksum,
    })
}

pub type SessionId = u64;

#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub device_id: u64,
    pub session_key: Vec<u8>,
    pub created_at_ms: u64,
    pub last_seen_ms: u64,
}

#[derive(Debug, Default)]
struct GroupSessions {
    entries: HashMap<SessionId, SessionEntry>,
}

pub struct SessionManager {
    ttl_ms: u64,
    resolver: Box<dyn Fn(u64) -> u64 + Send + Sync>,
    groups: HashMap<u64, GroupSessions>,
    next_id: SessionId,
    locations: HashMap<SessionId, u64>,
}

impl SessionManager {
    pub fn new(ttl_ms: u64) -> Self {
        Self::new_with_resolver(ttl_ms, |device_id| device_id)
    }

    pub fn new_with_resolver<F>(ttl_ms: u64, resolver: F) -> Self
    where
        F: Fn(u64) -> u64 + Send + Sync + 'static,
    {
        Self {
            ttl_ms,
            resolver: Box::new(resolver),
            groups: HashMap::new(),
            next_id: 1,
            locations: HashMap::new(),
        }
    }

    pub fn create(&mut self, device_id: u64, session_key: Vec<u8>, now_ms: u64) -> SessionId {
        let group_id = (self.resolver)(device_id);
        let group = self.groups.entry(group_id).or_insert_with(|| GroupSessions {
            entries: HashMap::new(),
        });
        let session_id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.locations.insert(session_id, group_id);
        let entry = SessionEntry {
            device_id,
            session_key,
            created_at_ms: now_ms,
            last_seen_ms: now_ms,
        };
        group.entries.insert(session_id, entry);
        session_id
    }

    pub fn get(&mut self, session_id: SessionId, device_id: u64, now_ms: u64) -> Result<SessionEntry> {
        let group_id = (self.resolver)(device_id);
        self.get_internal(session_id, group_id, now_ms)
    }

    pub fn get_by_id(&mut self, session_id: SessionId, now_ms: u64) -> Result<SessionEntry> {
        let group_id = *self
            .locations
            .get(&session_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        self.get_internal(session_id, group_id, now_ms)
    }

    fn get_internal(
        &mut self,
        session_id: SessionId,
        group_id: u64,
        now_ms: u64,
    ) -> Result<SessionEntry> {
        let group = self
            .groups
            .get_mut(&group_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        let entry = group
            .entries
            .get_mut(&session_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        let age = now_ms.saturating_sub(entry.created_at_ms);
        if age > self.ttl_ms {
            group.entries.remove(&session_id);
            self.locations.remove(&session_id);
            return Err(anyhow!("session expired"));
        }
        entry.last_seen_ms = now_ms;
        Ok(entry.clone())
    }

    pub fn peek_key(&self, session_id: SessionId) -> Option<Vec<u8>> {
        let group_id = self.locations.get(&session_id)?;
        let group = self.groups.get(group_id)?;
        group.entries.get(&session_id).map(|e| e.session_key.clone())
    }

    pub fn touch(&mut self, session_id: SessionId, now_ms: u64) -> Result<()> {
        self.get_by_id(session_id, now_ms).map(|_| ())
    }

    pub fn get_device_id(&self, session_id: SessionId) -> Option<u64> {
        let group_id = self.locations.get(&session_id)?;
        let group = self.groups.get(group_id)?;
        group.entries.get(&session_id).map(|e| e.device_id)
    }

    pub fn insert_for_test(
        &mut self,
        device_id: u64,
        session_key: Vec<u8>,
        now_ms: u64,
    ) -> SessionId {
        self.create(device_id, session_key, now_ms)
    }
}

struct NoncePersistence {
    db: sled::Db,
}

impl NoncePersistence {
    fn open(path: &str) -> Result<Self> {
        Ok(Self { db: sled::open(path)? })
    }

    fn load_all(&self) -> Result<HashMap<u64, VecDeque<u64>>> {
        let mut out = HashMap::new();
        for item in self.db.iter() {
            let (key, value) = item?;
            let group_id = u64::from_be_bytes(key.as_ref().try_into()?);
            let list: Vec<u64> = bincode::deserialize(&value)?;
            out.insert(group_id, VecDeque::from(list));
        }
        Ok(out)
    }

    fn save_group(&self, group_id: u64, list: &VecDeque<u64>) -> Result<()> {
        let key = group_id.to_be_bytes();
        let value = bincode::serialize(&list.iter().copied().collect::<Vec<u64>>())?;
        self.db.insert(key, value)?;
        // Flush asynchronously if possible, but sled::Db::flush is synchronous.
        // For performance, we might skip flush on every write, but here we prioritize safety as per requirements.
        self.db.flush()?;
        Ok(())
    }
}

pub struct NonceCache {
    limit: usize,
    items: HashMap<u64, VecDeque<u64>>,
    persistence: Option<NoncePersistence>,
}

impl NonceCache {
    pub fn new(limit: usize) -> Self {
        let limit = limit.max(1);
        Self {
            limit,
            items: HashMap::new(),
            persistence: None,
        }
    }

    pub fn new_with_persistence(limit: usize, persist_path: Option<String>) -> Result<Self> {
        let limit = limit.max(1);
        let mut items = HashMap::new();
        let persistence = if let Some(path) = persist_path {
            let p = NoncePersistence::open(&path)?;
            items = p.load_all()?;
            Some(p)
        } else {
            None
        };
        
        Ok(Self {
            limit,
            items,
            persistence,
        })
    }

    pub fn seen(&mut self, group_id: u64, nonce: u64) -> Result<()> {
        let list = self.items.entry(group_id).or_default();
        if list.iter().any(|item| *item == nonce) {
            return Err(anyhow!("replay detected"));
        }
        if list.len() >= self.limit {
            list.pop_front();
        }
        list.push_back(nonce);
        
        if let Some(p) = &self.persistence {
            p.save_group(group_id, list)?;
        }
        
        Ok(())
    }
}

pub fn handle_auth_hello(
    buf: &[u8],
    ttl_ms: u64,
    sessions: &mut SessionManager,
    nonce_cache: &mut NonceCache,
    lookup_key: impl Fn(u64) -> Option<Vec<u8>>,
    now_ms: u64,
) -> Result<Vec<u8>> {
    let (meta, _) = decode_auth_hello(buf, lookup_key)?;
    nonce_cache.seen(meta.device_id, meta.nonce)?;
    let session_key = Uuid::new_v4().as_bytes().to_vec();
    let session_id = sessions.create(meta.device_id, session_key.clone(), now_ms);
    let ack = AuthAck {
        session_id,
        session_key,
        ttl_ms,
    };
    let mut builder = FlatBufferBuilder::new();
    let root = encode_auth_ack(&mut builder, &ack);
    builder.finish(root, None);
    Ok(builder.finished_data().to_vec())
}

pub fn handle_session_request(
    buf: &[u8],
    sessions: &mut SessionManager,
    nonce_cache: &mut NonceCache,
    now_ms: u64,
) -> Result<(EdgeRequest, u64, u64)> {
    let (meta, req) = {
        let lookup = |id| sessions.peek_key(id);
        decode_session_request(buf, lookup)?
    };

    let device_id = sessions
        .get_device_id(meta.session_id)
        .ok_or_else(|| anyhow!("Session not found after decode"))?;

    nonce_cache.seen(device_id, meta.nonce)?;
    sessions.touch(meta.session_id, now_ms)?;

    Ok((req, meta.session_id, meta.nonce))
}

pub fn pack_signed_response(
    payload: &[u8],
    session_id: u64,
    timestamp_ms: u64,
    nonce: u64,
    req_header: &Header,
    session_key: &[u8],
) -> Vec<u8> {
    let mut header_prefix = [0u8; 14];
    header_prefix[0..4].copy_from_slice(&MAGIC);
    header_prefix[4] = VERSION;
    header_prefix[5] = MSG_TYPE_RESPONSE;
    header_prefix[6..14].copy_from_slice(&req_header.request_id.to_be_bytes());

    let mut builder = FlatBufferBuilder::new();
    let root = encode_signed_response(
        &mut builder,
        session_id,
        timestamp_ms,
        nonce,
        &header_prefix,
        payload,
        session_key,
    );
    builder.finish(root, None);
    let signed_payload = builder.finished_data();

    let mut header = [0u8; HEADER_SIZE];
    header[0..4].copy_from_slice(&MAGIC);
    header[4] = VERSION;
    header[5] = MSG_TYPE_RESPONSE;
    header[6..14].copy_from_slice(&req_header.request_id.to_be_bytes());
    header[14..18].copy_from_slice(&(signed_payload.len() as u32).to_be_bytes());
    header[18..21].copy_from_slice(&[0u8; 3]);

    let mut out = Vec::with_capacity(HEADER_SIZE + signed_payload.len());
    out.extend_from_slice(&header);
    out.extend_from_slice(signed_payload);
    out
}

pub struct EdgeGateway {
    addr: String,
    router: Arc<Router>,
    sessions: Arc<Mutex<SessionManager>>,
    nonce_cache: Arc<Mutex<NonceCache>>,
    ttl_ms: u64,
}

impl EdgeGateway {
    pub fn new(addr: String, router: Arc<Router>, config: EdgeGatewayConfig) -> Result<Self> {
        let sessions = SessionManager::new(config.session_ttl_ms);
        let nonce_cache = NonceCache::new_with_persistence(
            config.nonce_cache_limit,
            if config.nonce_persist_enabled {
                Some(config.nonce_persist_path)
            } else {
                None
            },
        )?;
        Ok(Self {
            addr,
            router,
            sessions: Arc::new(Mutex::new(sessions)),
            nonce_cache: Arc::new(Mutex::new(nonce_cache)),
            ttl_ms: config.session_ttl_ms,
        })
    }

    pub async fn run(self: Arc<Self>) -> Result<()> {
        let listener = TcpListener::bind(&self.addr)
            .await
            .context("Failed to bind Edge TCP port")?;
        println!("EdgeGateway listening on {}", self.addr);

        loop {
            let (socket, _) = listener.accept().await?;
            let gateway = self.clone();
            tokio::spawn(async move {
                if let Err(e) = gateway.handle_connection(socket).await {
                    eprintln!("Edge connection error: {:?}", e);
                }
            });
        }
    }

    async fn handle_connection(&self, mut socket: TcpStream) -> Result<()> {
        let mut buf = vec![0u8; 1024];

        loop {
            let mut header_buf = [0u8; HEADER_SIZE];
            match socket.read_exact(&mut header_buf).await {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
                Err(e) => return Err(e.into()),
            }

            let header = validate_header(&header_buf)?;
            let payload_len = header.payload_len as usize;

            if payload_len > 1024 * 1024 {
                return Err(anyhow!("Payload too large"));
            }

            if buf.len() < payload_len {
                buf.resize(payload_len, 0);
            }
            socket.read_exact(&mut buf[..payload_len]).await?;
            let payload = &buf[..payload_len];

            match header.msg_type {
                MSG_TYPE_AUTH_HELLO => {
                    let now_ms =
                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis()
                            as u64;
                    let lookup_key = |_id: u64| Some(b"device_secret".to_vec());

                    let response_payload = {
                        let mut sessions = self.sessions.lock().unwrap();
                        let mut nonce_cache = self.nonce_cache.lock().unwrap();
                        handle_auth_hello(
                            payload,
                            self.ttl_ms,
                            &mut sessions,
                            &mut nonce_cache,
                            lookup_key,
                            now_ms,
                        )?
                    };

                    let resp_len = response_payload.len();
                    let mut resp_header = [0u8; HEADER_SIZE];
                    resp_header[0..4].copy_from_slice(&MAGIC);
                    resp_header[4] = VERSION;
                    resp_header[5] = MSG_TYPE_RESPONSE;
                    resp_header[6..14].copy_from_slice(&header.request_id.to_be_bytes());
                    resp_header[14..18].copy_from_slice(&(resp_len as u32).to_be_bytes());

                    socket.write_all(&resp_header).await?;
                    socket.write_all(&response_payload).await?;
                }
                MSG_TYPE_SESSION_REQUEST => {
                    let now_ms =
                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis()
                            as u64;
                    let (req, session_id, nonce, session_key) = {
                        let mut sessions = self.sessions.lock().unwrap();
                        let mut nonce_cache = self.nonce_cache.lock().unwrap();
                        let (req, sid, nonce) =
                            handle_session_request(payload, &mut sessions, &mut nonce_cache, now_ms)?;
                        let key = sessions
                            .peek_key(sid)
                            .ok_or_else(|| anyhow!("Session key not found"))?;
                        (req, sid, nonce, key)
                    };

                    let response_data = match req {
                        EdgeRequest::Heartbeat { .. } => b"OK".to_vec(),
                        EdgeRequest::UploadData { records } => {
                            for record in records {
                                let val_hex: String =
                                    record.value.iter().map(|b| format!("{:02X}", b)).collect();
                                let sql = format!(
                                    "INSERT OR REPLACE INTO data (key, value, timestamp) VALUES ('{}', x'{}', {})",
                                    record.key, val_hex, record.timestamp
                                );
                                self.router.write(sql).await?;
                            }
                            b"OK".to_vec()
                        }
                        EdgeRequest::AuthHello { .. } => {
                            return Err(anyhow!("Invalid request type inside session"))
                        }
                    };

                    let signed_resp = pack_signed_response(
                        &response_data,
                        session_id,
                        now_ms,
                        nonce,
                        &header,
                        &session_key,
                    );

                    socket.write_all(&signed_resp).await?;
                }
                _ => return Err(anyhow!("Unknown msg_type {}", header.msg_type)),
            }
        }
    }
}
