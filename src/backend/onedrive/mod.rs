use std::{
    fmt::Debug,
    path::{Path, PathBuf},
    sync::{atomic::AtomicU64, Arc},
};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use snafu::Snafu;
use tracing::{debug, warn};

use crate::{AsyncBufReadSeek, Backend};

pub mod auth;
pub mod upload;

struct OnedriveInner {
    client: auth::Client,
    access_token: ArcSwap<String>,
    refresh_token: ArcSwap<String>,
    expires_at: AtomicU64,
    api_type: ApiType,
    folder: PathBuf,
}

impl Debug for OnedriveInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnedriveInner")
            .field("api_type", &self.api_type)
            .field("folder", &self.folder)
            .finish()
    }
}

#[derive(Debug)]
pub struct Onedrive {
    inner: Arc<OnedriveInner>,
    refresh_handle: tokio::task::JoinHandle<()>,
}

impl Drop for Onedrive {
    fn drop(&mut self) {
        self.refresh_handle.abort();
    }
}

#[async_trait]
impl Backend for Onedrive {
    async fn upload(
        &self,
        reader: &mut dyn AsyncBufReadSeek,
        size: u64,
        path: PathBuf,
    ) -> Result<(), Box<dyn snafu::Error>> {
        debug!("Uploading file to onedrive: {:?}", &path);
        self.inner.upload(reader, size, path).await
    }
}

impl Onedrive {
    pub fn refresh_token(&self) -> String {
        self.inner.refresh_token.load().to_string()
    }

    pub async fn new_with_code(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        redirect_url: impl Into<String>,
        api_type: ApiType,
        path: impl AsRef<Path>,
    ) -> Result<Self, Error> {
        let inner =
            OnedriveInner::new_with_code(client_id, client_secret, redirect_url, api_type, path)
                .await
                .map(Arc::new)?;
        let inner_clone = inner.clone();
        let refresh_handle = refresh_handle(inner_clone);

        Ok(Self {
            inner,
            refresh_handle,
        })
    }

    pub async fn new_with_refresh_token(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        refresh_token: impl Into<String>,
        api_type: ApiType,
        path: impl AsRef<Path>,
    ) -> Result<Self, Error> {
        let inner = OnedriveInner::new_with_refresh_token(
            client_id,
            client_secret,
            refresh_token,
            api_type,
            path,
        )
        .await
        .map(Arc::new)?;

        let inner_clone = inner.clone();

        let refresh_handle = refresh_handle(inner_clone);

        Ok(Self {
            inner,
            refresh_handle,
        })
    }
}

fn refresh_handle(inner: Arc<OnedriveInner>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let expires_in = inner.expires_at.load(std::sync::atomic::Ordering::Acquire);
            let now = chrono::Utc::now().timestamp() as u64;
            if now + 120 > expires_in {
                match inner.refresh().await {
                    Ok(_) => {
                        debug!("Onedrive Token refreshed");
                    }
                    Err(e) => {
                        warn!("Failed to refresh onedrive token: {}", e);
                    }
                }
            }
            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
        }
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ApiType {
    Common,
    Consumers,
    Organizations,
    ChinaApi,
}

impl ApiType {
    fn get_auth_url(&self) -> &'static str {
        match self {
            ApiType::Common => "https://login.microsoftonline.com/common/oauth2/v2.0/authorize",
            ApiType::Consumers => {
                "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize"
            }
            ApiType::Organizations => {
                "https://login.microsoftonline.com/organizations/oauth2/v2.0/authorize"
            }
            ApiType::ChinaApi => "https://login.chinacloudapi.cn/common/oauth2/v2.0/authorize",
        }
    }

    fn get_token_url(&self) -> &'static str {
        match self {
            ApiType::Common => "https://login.microsoftonline.com/common/oauth2/v2.0/token",
            ApiType::Consumers => "https://login.microsoftonline.com/consumers/oauth2/v2.0/token",
            ApiType::Organizations => {
                "https://login.microsoftonline.com/organizations/oauth2/v2.0/token"
            }
            ApiType::ChinaApi => "https://login.chinacloudapi.cn/common/oauth2/v2.0/token",
        }
    }

    #[allow(dead_code)]
    fn get_graph_url(&self) -> &'static str {
        match self {
            ApiType::Common => "https://graph.microsoft.com/v1.0",
            ApiType::Consumers => "https://graph.microsoft.com/v1.0",
            ApiType::Organizations => "https://graph.microsoft.com/v1.0",
            ApiType::ChinaApi => "https://microsoftgraph.chinacloudapi.cn/v1.0",
        }
    }
}

#[derive(Debug, Snafu)]
pub enum Error {
    #[snafu(display("Failed to refresh token: {}", message))]
    RefreshToken { message: String },

    #[snafu(display("Failed to verify csrf token"))]
    CsrfToken,

    #[snafu(display("The file {file} is too large {size}. The maximum file size is 250 GB"))]
    FileTooLarge { file: String, size: String },

    #[snafu(display("Failed to get parent id for path: {}, error: {}", path, source))]
    GetParentId {
        source: reqwest::Error,
        path: String,
    },

    #[snafu(display("Failed to parse response: {}", context))]
    Parsing { context: String },

    #[snafu(display("Invalid Path: {}", path))]
    InvalidPath { path: String },

    #[snafu(display("Failed to create directory for path: {}, error: {}", path, source))]
    CreateDir {
        source: reqwest::Error,
        path: String,
    },

    #[snafu(display("Failed to create upload session: {}", source))]
    CreateUploadSessionRequest { source: reqwest::Error },

    #[snafu(display("Failed to create upload session: {}", message))]
    CreateUploadSession { message: String },

    #[snafu(display("Failed to upload file with session: {}", source))]
    UploadFileSessionRequest { source: reqwest::Error },

    #[snafu(display("Failed to upload file with session: {}", message))]
    UploadFileSession { message: String },

    #[snafu(display("Failed to read file: {}", source))]
    ReadFile { source: std::io::Error },

    #[snafu(display("Failed to upload file: {}", source))]
    UploadFile { source: reqwest::Error },
}
