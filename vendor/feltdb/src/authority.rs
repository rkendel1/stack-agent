//! One logical durable authority across embedded and local multi-process use.
//!
//! The authority server is the sole process which opens the storage log. IPC
//! clients never mutate storage artifacts and every authoritative read crosses
//! the socket boundary.

use crate::{
    AtomicCommit, AtomicMutation, AtomicPrecondition, FeltDb, RecordPrecondition, StoredRow,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    io,
    path::{Path, PathBuf},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadKind {
    Snapshot,
    Authoritative,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorityRead {
    pub observed_revision: u64,
    pub authoritative_revision: u64,
    pub kind: ReadKind,
    pub content_hash: String,
    pub rows: Vec<StoredRow>,
}

impl AuthorityRead {
    fn current(db: &FeltDb, kind: ReadKind) -> crate::Result<Self> {
        let (revision, mut rows) = db.authority_state();
        rows.sort_by(|left, right| {
            (&left.capability, &left.key).cmp(&(&right.capability, &right.key))
        });
        let mut hasher = Sha256::new();
        hasher.update(revision.to_le_bytes());
        hasher.update(serde_json::to_vec(&rows)?);
        Ok(Self {
            observed_revision: revision,
            authoritative_revision: revision,
            kind,
            content_hash: format!("sha256:{:x}", hasher.finalize()),
            rows,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorityTransaction {
    pub transaction_id: String,
    /// Explicit optimistic fence. `None` asks the authority to begin and
    /// commit an unconditional transaction at its current revision.
    #[serde(default)]
    pub base_revision: Option<u64>,
    #[serde(default)]
    pub preconditions: Vec<AtomicPrecondition>,
    #[serde(default)]
    pub record_preconditions: Vec<RecordPrecondition>,
    pub mutations: Vec<AtomicMutation>,
    #[serde(default)]
    pub audit: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorityError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub authoritative_revision: Option<u64>,
}

impl std::fmt::Display for AuthorityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AuthorityError {}

#[async_trait]
pub trait FeltDbAuthority: Send + Sync {
    async fn read_authoritative(&self) -> Result<AuthorityRead, AuthorityError>;
    async fn current_revision(&self) -> Result<u64, AuthorityError>;
    async fn commit(
        &self,
        transaction: AuthorityTransaction,
    ) -> Result<AtomicCommit, AuthorityError>;
}

#[derive(Clone)]
pub struct EmbeddedAuthority {
    db: FeltDb,
}

impl EmbeddedAuthority {
    pub fn open(path: impl AsRef<Path>) -> crate::Result<Self> {
        Ok(Self {
            db: FeltDb::open(path)?,
        })
    }

    pub fn from_db(db: FeltDb) -> Self {
        Self { db }
    }

    /// An explicitly historical value. It never claims freshness beyond its
    /// captured `observed_revision`.
    pub fn snapshot(&self) -> crate::Result<AuthorityRead> {
        AuthorityRead::current(&self.db, ReadKind::Snapshot)
    }
}

fn authority_error(error: crate::FlowError) -> AuthorityError {
    match error {
        crate::FlowError::RevisionConflict {
            expected: _,
            actual,
        } => AuthorityError {
            code: "REVISION_CONFLICT".into(),
            message: error.to_string(),
            authoritative_revision: Some(actual),
        },
        other => AuthorityError {
            code: "AUTHORITY_ERROR".into(),
            message: other.to_string(),
            authoritative_revision: None,
        },
    }
}

#[async_trait]
impl FeltDbAuthority for EmbeddedAuthority {
    async fn read_authoritative(&self) -> Result<AuthorityRead, AuthorityError> {
        AuthorityRead::current(&self.db, ReadKind::Authoritative).map_err(authority_error)
    }

    async fn current_revision(&self) -> Result<u64, AuthorityError> {
        self.db.current_revision().map_err(authority_error)
    }

    async fn commit(
        &self,
        transaction: AuthorityTransaction,
    ) -> Result<AtomicCommit, AuthorityError> {
        self.db
            .apply_atomic_transaction_guarded(
                &transaction.transaction_id,
                None,
                transaction.base_revision,
                &transaction.preconditions,
                &transaction.record_preconditions,
                &transaction.mutations,
                transaction.audit,
            )
            .map_err(authority_error)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Request {
    Read,
    CurrentRevision,
    Commit { transaction: AuthorityTransaction },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", content = "value", rename_all = "snake_case")]
enum Response {
    Read(AuthorityRead),
    Revision(u64),
    Commit(AtomicCommit),
    Error(AuthorityError),
}

pub struct AuthorityServer {
    socket_path: PathBuf,
    authority: EmbeddedAuthority,
}

impl AuthorityServer {
    pub fn open(
        socket_path: impl Into<PathBuf>,
        storage_path: impl AsRef<Path>,
    ) -> crate::Result<Self> {
        Ok(Self {
            socket_path: socket_path.into(),
            authority: EmbeddedAuthority::open(storage_path)?,
        })
    }

    pub async fn serve(self) -> io::Result<()> {
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if self.socket_path.exists() {
            use std::os::unix::fs::FileTypeExt;
            if !std::fs::symlink_metadata(&self.socket_path)?
                .file_type()
                .is_socket()
            {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "authority socket path is not a socket",
                ));
            }
            std::fs::remove_file(&self.socket_path)?;
        }
        let listener = UnixListener::bind(&self.socket_path)?;
        loop {
            let (stream, _) = listener.accept().await?;
            let authority = self.authority.clone();
            tokio::spawn(async move {
                let _ = handle_connection(stream, authority).await;
            });
        }
    }
}

async fn handle_connection(stream: UnixStream, authority: EmbeddedAuthority) -> io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(Request::Read) => authority.read_authoritative().await.map(Response::Read),
            Ok(Request::CurrentRevision) => {
                authority.current_revision().await.map(Response::Revision)
            }
            Ok(Request::Commit { transaction }) => {
                authority.commit(transaction).await.map(Response::Commit)
            }
            Err(error) => Err(AuthorityError {
                code: "INVALID_REQUEST".into(),
                message: error.to_string(),
                authoritative_revision: None,
            }),
        }
        .unwrap_or_else(Response::Error);
        writer
            .write_all(&serde_json::to_vec(&response).map_err(io::Error::other)?)
            .await?;
        writer.write_all(b"\n").await?;
    }
    Ok(())
}

#[derive(Clone)]
pub struct AuthorityClient {
    socket_path: PathBuf,
}

impl AuthorityClient {
    pub fn connect(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }

    async fn request(&self, request: Request) -> Result<Response, AuthorityError> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .await
            .map_err(|error| AuthorityError {
                code: "AUTHORITY_UNAVAILABLE".into(),
                message: error.to_string(),
                authoritative_revision: None,
            })?;
        stream
            .write_all(
                &serde_json::to_vec(&request).map_err(|error| AuthorityError {
                    code: "INVALID_REQUEST".into(),
                    message: error.to_string(),
                    authoritative_revision: None,
                })?,
            )
            .await
            .map_err(io_authority_error)?;
        stream.write_all(b"\n").await.map_err(io_authority_error)?;
        let mut response = String::new();
        BufReader::new(stream)
            .read_line(&mut response)
            .await
            .map_err(io_authority_error)?;
        match serde_json::from_str(&response).map_err(|error| AuthorityError {
            code: "INVALID_RESPONSE".into(),
            message: error.to_string(),
            authoritative_revision: None,
        })? {
            Response::Error(error) => Err(error),
            response => Ok(response),
        }
    }
}

fn io_authority_error(error: io::Error) -> AuthorityError {
    AuthorityError {
        code: "AUTHORITY_UNAVAILABLE".into(),
        message: error.to_string(),
        authoritative_revision: None,
    }
}

#[async_trait]
impl FeltDbAuthority for AuthorityClient {
    async fn read_authoritative(&self) -> Result<AuthorityRead, AuthorityError> {
        match self.request(Request::Read).await? {
            Response::Read(read) => Ok(read),
            _ => Err(AuthorityError {
                code: "INVALID_RESPONSE".into(),
                message: "expected authority read".into(),
                authoritative_revision: None,
            }),
        }
    }

    async fn current_revision(&self) -> Result<u64, AuthorityError> {
        match self.request(Request::CurrentRevision).await? {
            Response::Revision(revision) => Ok(revision),
            _ => Err(AuthorityError {
                code: "INVALID_RESPONSE".into(),
                message: "expected authority revision".into(),
                authoritative_revision: None,
            }),
        }
    }

    async fn commit(
        &self,
        transaction: AuthorityTransaction,
    ) -> Result<AtomicCommit, AuthorityError> {
        match self.request(Request::Commit { transaction }).await? {
            Response::Commit(commit) => Ok(commit),
            _ => Err(AuthorityError {
                code: "INVALID_RESPONSE".into(),
                message: "expected authority commit".into(),
                authoritative_revision: None,
            }),
        }
    }
}
