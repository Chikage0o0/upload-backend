use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use snafu::ResultExt;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt};

use crate::{AsyncBufReadSeek, Backend};

/// The maximum file size that can be uploaded to OneDrive.  
/// 250 GB
const MAX_FILE_LIMIT: u64 = 250 * 1024 * 1024 * 1024;
/// The size of each chunk when uploading a file.  
/// 10 MB
const CHUNK_SIZE: u64 = 10 * 1024 * 1024;

use super::{
    CreateDirSnafu, CreateUploadSessionRequestSnafu, Error, GetParentIdSnafu, OnedriveInner,
    ReadFileSnafu, UploadFileSessionRequestSnafu, UploadFileSnafu,
};

#[async_trait]
impl Backend for OnedriveInner {
    async fn upload(
        &self,
        reader: Box<dyn AsyncBufReadSeek>,
        size: u64,
        path: PathBuf,
    ) -> Result<(), Box<dyn snafu::Error>> {
        if size > MAX_FILE_LIMIT {
            return Err(Error::FileTooLarge {
                file: path.to_string_lossy().to_string(),
                size: u64_to_size_string(size),
            }
            .into());
        }

        if size < CHUNK_SIZE {
            self.upload_file(reader, size, &path).await?
        } else {
            self.upload_file_with_session(reader, size, &path).await?
        }

        Ok(())
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FirstUploadSessionResponse {
    upload_url: String,
    expiration_date_time: DateTime<Utc>,
}
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadSession {
    next_expected_ranges: Vec<String>,
    expiration_date_time: DateTime<Utc>,
}

impl OnedriveInner {
    async fn upload_file(
        &self,
        mut reader: Box<dyn AsyncBufReadSeek>,
        size: u64,
        path: &Path,
    ) -> Result<(), Error> {
        let (parent_id, file_name) = self.calu_path(path).await?;

        let mut buf = Vec::with_capacity(size as usize);
        reader
            .seek(tokio::io::SeekFrom::Start(0))
            .await
            .context(ReadFileSnafu)?;
        reader.read_to_end(&mut buf).await.context(ReadFileSnafu)?;

        let url = format!(
            "{}/me/drive/items/{}:/{}:/content",
            self.api_type.get_graph_url(),
            parent_id,
            file_name
        );
        let response = reqwest::Client::new()
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/octet-stream")
            .body(buf)
            .send()
            .await
            .context(UploadFileSnafu)?;

        match response.status() {
            reqwest::StatusCode::CREATED | reqwest::StatusCode::OK => Ok(()),
            _ => Err(Error::UploadFile {
                source: response.error_for_status().unwrap_err(),
            }),
        }
    }

    async fn upload_file_with_session(
        &self,
        mut reader: Box<dyn AsyncBufReadSeek>,
        size: u64,
        path: &Path,
    ) -> Result<(), Error> {
        let session = self.create_session(path).await?;
        if session.expiration_date_time < Utc::now() {
            return Err(Error::UploadFileSession {
                message: "Upload session expired".to_string(),
            });
        }
        let mut start_pos = 0;
        let mut uploading = self
            .upload_session(&session.upload_url, &mut reader, size, start_pos)
            .await?
            .ok_or(Error::UploadFileSession {
                message: "Upload session expired".to_string(),
            })?;

        while !uploading.next_expected_ranges.is_empty() {
            if uploading.expiration_date_time < Utc::now() {
                return Err(Error::UploadFileSession {
                    message: "Upload session expired".to_string(),
                });
            }
            let range = uploading.next_expected_ranges[0]
                .split('-')
                .map(|s| s.parse::<u64>().unwrap())
                .collect::<Vec<u64>>();

            start_pos = range[0];
            let ret = self
                .upload_session(&session.upload_url, &mut reader, size, start_pos)
                .await?;
            uploading = match ret {
                Some(session) => session,
                None => break,
            };
        }

        Ok(())
    }

    async fn upload_session(
        &self,
        url: &str,
        reader: &mut dyn AsyncBufReadSeek,
        size: u64,
        start_pos: u64,
    ) -> Result<Option<UploadSession>, Error> {
        // Generate a buffer to store the chunk
        let mut buffer = vec![0; CHUNK_SIZE as usize];
        reader
            .seek(tokio::io::SeekFrom::Start(start_pos))
            .await
            .context(ReadFileSnafu)?;
        let mut len = 0u64;
        while len < CHUNK_SIZE {
            let read_len = reader
                .read(&mut buffer[len as usize..])
                .await
                .context(ReadFileSnafu)?;
            if read_len == 0 {
                break;
            }
            len += read_len as u64;
        }
        buffer.resize(len as usize, 0);

        let response = reqwest::Client::new()
            .put(url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Length", len)
            .header(
                "Content-Range",
                format!("bytes {}-{}/{}", start_pos, start_pos + len - 1, size),
            )
            .body(buffer)
            .send()
            .await
            .context(UploadFileSessionRequestSnafu)?;

        match response.status() {
            reqwest::StatusCode::ACCEPTED => {
                let json = response
                    .json::<UploadSession>()
                    .await
                    .context(UploadFileSessionRequestSnafu)?;

                Ok(Some(json))
            }
            reqwest::StatusCode::CREATED | reqwest::StatusCode::OK => Ok(None),
            _ => Err(Error::UploadFileSessionRequest {
                source: response.error_for_status().unwrap_err(),
            }),
        }
    }

    async fn create_session(&self, path: &Path) -> Result<FirstUploadSessionResponse, Error> {
        let (parent_id, file_name) = self.calu_path(path).await?;

        let url = format!(
            "{}/me/drive/items/{}:/{}:/createUploadSession",
            self.api_type.get_graph_url(),
            parent_id,
            file_name
        );
        let response = reqwest::Client::new()
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .json(&serde_json::json!({
               "item": {
                "@microsoft.graph.conflictBehavior": "replace"
              },
              "deferCommit": false
            }))
            .send()
            .await
            .context(CreateUploadSessionRequestSnafu)?;

        match response.status() {
            reqwest::StatusCode::OK => {
                let json = response
                    .json::<FirstUploadSessionResponse>()
                    .await
                    .context(CreateUploadSessionRequestSnafu)?;

                Ok(json)
            }
            _ => Err(Error::CreateUploadSessionRequest {
                source: response.error_for_status().unwrap_err(),
            }),
        }
    }

    async fn get_parent_id(&self, folder: &Path) -> Result<String, Error> {
        let forder_str = folder.to_string_lossy();
        let url = if forder_str == "/" {
            format!("{}/me/drive/root", self.api_type.get_graph_url())
        } else {
            format!(
                "{}/me/drive/root:/{}/",
                self.api_type.get_graph_url(),
                forder_str
            )
        };

        let response = reqwest::Client::new()
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .send()
            .await
            .with_context(|_| GetParentIdSnafu {
                path: folder.to_string_lossy().to_string(),
            })?;

        match response.status() {
            reqwest::StatusCode::OK => {
                let json = response
                    .json::<serde_json::Value>()
                    .await
                    .with_context(|_| GetParentIdSnafu {
                        path: folder.to_string_lossy().to_string(),
                    })?;

                let folder_id =
                    json.get("id")
                        .and_then(|id| id.as_str())
                        .ok_or_else(|| Error::Parsing {
                            context: json.to_string(),
                        })?;

                Ok(folder_id.to_string())
            }
            reqwest::StatusCode::NOT_FOUND => Box::pin(self.create_folder(folder)).await,
            _ => Err(Error::GetParentId {
                path: folder.to_string_lossy().to_string(),
                source: response.error_for_status().unwrap_err(),
            }),
        }
    }

    async fn create_folder(&self, folder: &Path) -> Result<String, Error> {
        let parent = folder.parent().ok_or(Error::InvalidPath {
            path: folder.to_string_lossy().to_string(),
        })?;
        let parent_id = self.get_parent_id(parent).await?;

        let url = format!(
            "{}/me/drive/items/{}/children",
            self.api_type.get_graph_url(),
            parent_id
        );

        let response = reqwest::Client::new()
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .json(&serde_json::json!({
                "name": folder.file_name().unwrap().to_string_lossy(),
                "folder": {},
                "@microsoft.graph.conflictBehavior": "replace",
            }))
            .send()
            .await
            .with_context(|_| CreateDirSnafu {
                path: folder.to_string_lossy().to_string(),
            })?;

        match response.status() {
            reqwest::StatusCode::CREATED => {
                let json = response
                    .json::<serde_json::Value>()
                    .await
                    .with_context(|_| GetParentIdSnafu {
                        path: folder.to_string_lossy().to_string(),
                    })?;

                let folder_id =
                    json.get("id")
                        .and_then(|id| id.as_str())
                        .ok_or_else(|| Error::Parsing {
                            context: json.to_string(),
                        })?;

                Ok(folder_id.to_string())
            }
            StatusCode::NOT_FOUND => Box::pin(self.create_folder(parent)).await,
            _ => Err(Error::CreateDir {
                path: folder.to_string_lossy().to_string(),
                source: response.error_for_status().unwrap_err(),
            }),
        }
    }

    async fn calu_path(&self, path: &Path) -> Result<(String, String), Error> {
        if path.has_root() {
            return Err(Error::InvalidPath {
                path: path.to_string_lossy().to_string(),
            });
        }
        let path = self.folder.join(path);
        let parent = path.parent().ok_or(Error::InvalidPath {
            path: path.to_string_lossy().to_string(),
        })?;
        let parent_id = self.get_parent_id(parent).await?;
        let file_name = path
            .file_name()
            .ok_or(Error::InvalidPath {
                path: path.to_string_lossy().to_string(),
            })?
            .to_string_lossy()
            .to_string();

        Ok((parent_id, file_name))
    }
}

// u64转换为常规单位，保留两位小数
fn u64_to_size_string(size: u64) -> String {
    let size = size as f64;
    let units = ["B", "KB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"];
    let unit = units
        .iter()
        .enumerate()
        .find(|(i, _)| size < 1024_f64.powi((i + 1).try_into().unwrap()))
        .unwrap_or((units.len() - 1, &"YB"));
    format!("{:.2} {}", size / 1024_f64.powi(unit.0 as i32), unit.1)
}

#[cfg(test)]
mod test {

    use super::*;

    #[test]
    fn convert_u64() {
        let size = 1024;
        let size_str = u64_to_size_string(size);
        assert_eq!(size_str, "1.00 KB");

        let size = 2 * 1024 * 1024;
        let size_str = u64_to_size_string(size);
        assert_eq!(size_str, "2.00 MB");

        let size = MAX_FILE_LIMIT;
        let size_str = u64_to_size_string(size);
        assert_eq!(size_str, "250.00 GB");
    }
}
