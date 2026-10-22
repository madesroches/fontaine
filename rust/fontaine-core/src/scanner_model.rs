//! Fetching the PromptGuard 2 files. The weights are a gated download on Hugging Face and are
//! never bundled: only a user who has accepted the license there and holds a token gets them.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where the files are downloaded from; the repository is gated.
pub const BASE_URL: &str =
    "https://huggingface.co/meta-llama/Llama-Prompt-Guard-2-86M/resolve/main";

pub const FILES: [&str; 3] = ["model.safetensors", "tokenizer.json", "config.json"];

/// A server that accepts the connection and then says nothing must not hold startup forever.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest silence tolerated between two pieces of a body; the whole download has no bound
/// of its own because the weights are large and connections vary.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// `<data dir>/models/promptguard2-86m`.
pub fn default_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join("promptguard2-86m")
}

#[derive(Debug)]
pub enum FetchError {
    /// The token variable is unset or empty.
    MissingToken,
    /// 401 or 403: the token is wrong or the license was not accepted.
    Denied(u16),
    Failed(String),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::MissingToken => write!(f, "no Hugging Face token to download the model"),
            FetchError::Denied(status) => {
                write!(f, "Hugging Face refused the download (HTTP {status})")
            }
            FetchError::Failed(message) => write!(f, "the model download failed: {message}"),
        }
    }
}

fn part_of(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    path.with_file_name(name)
}

/// Downloads the files of `dir` that are missing from `base_url`. A file lands under its final
/// name only after a complete, successful response; an interrupted one leaves nothing behind.
/// With every file present, no token is needed and nothing is requested.
pub async fn ensure(dir: &Path, base_url: &str, token: Option<&str>) -> Result<(), FetchError> {
    let missing: Vec<&str> = FILES
        .into_iter()
        .filter(|name| !dir.join(name).is_file())
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let token = token
        .filter(|t| !t.is_empty())
        .ok_or(FetchError::MissingToken)?;
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| FetchError::Failed(format!("cannot create {}: {e}", dir.display())))?;
    let client = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .build()
        .map_err(|e| FetchError::Failed(e.to_string()))?;
    for name in missing {
        let target = dir.join(name);
        let part = part_of(&target);
        let result = fetch_one(&client, &format!("{base_url}/{name}"), token, &part).await;
        match result {
            Ok(()) => tokio::fs::rename(&part, &target).await.map_err(|e| {
                FetchError::Failed(format!("cannot place {}: {e}", target.display()))
            })?,
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(e);
            }
        }
    }
    Ok(())
}

async fn fetch_one(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    part: &Path,
) -> Result<(), FetchError> {
    use tokio::io::AsyncWriteExt;

    // The error text of a failed request can echo the URL; the token is in a header, never in it.
    let failed = |e: reqwest::Error| FetchError::Failed(e.without_url().to_string());
    let mut response = client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(failed)?;
    let status = response.status();
    if matches!(status.as_u16(), 401 | 403) {
        return Err(FetchError::Denied(status.as_u16()));
    }
    if !status.is_success() {
        return Err(FetchError::Failed(format!("HTTP {}", status.as_u16())));
    }
    let expected = response.content_length();
    let mut file = tokio::fs::File::create(part)
        .await
        .map_err(|e| FetchError::Failed(format!("cannot write {}: {e}", part.display())))?;
    let mut written: u64 = 0;
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        written += chunk.len() as u64;
        file.write_all(&chunk)
            .await
            .map_err(|e| FetchError::Failed(format!("cannot write {}: {e}", part.display())))?;
    }
    file.flush()
        .await
        .map_err(|e| FetchError::Failed(format!("cannot write {}: {e}", part.display())))?;
    if expected.is_some_and(|expected| expected != written) {
        return Err(FetchError::Failed("the response ended early".into()));
    }
    Ok(())
}
