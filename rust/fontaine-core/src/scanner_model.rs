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

/// Marks a download in progress, so a later start can tell what an interrupted one left.
const PART_PREFIX: &str = ".part-";
/// Old enough that no live download of another session can be mistaken for a leftover.
const STALE_AFTER: Duration = Duration::from_secs(10 * 60);

/// The files of `dir` that a download would still fetch.
pub fn missing(dir: &Path) -> Vec<&'static str> {
    FILES
        .into_iter()
        .filter(|name| !dir.join(name).is_file())
        .collect()
}

/// Removes partial downloads that a killed process left behind.
fn sweep_stale(dir: &Path, grace: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let is_part = entry.file_name().to_string_lossy().starts_with(PART_PREFIX);
        let age = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok());
        if is_part && age.is_some_and(|age| age >= grace) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Downloads the files of `dir` that are missing from `base_url`. A file lands under its final
/// name only after a complete, successful response; a failed fetch removes its partial file, and
/// one a killed process left behind is swept by the next download once it is stale.
/// With every file present, no token is needed and nothing is requested.
pub async fn ensure(dir: &Path, base_url: &str, token: Option<&str>) -> Result<(), FetchError> {
    let missing = missing(dir);
    if missing.is_empty() {
        return Ok(());
    }
    let token = token
        .filter(|t| !t.is_empty())
        .ok_or(FetchError::MissingToken)?;
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| FetchError::Failed(format!("cannot create {}: {e}", dir.display())))?;
    sweep_stale(dir, STALE_AFTER);
    let client = fontaine_tools::tls::build_client(|| {
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
    })
    .map_err(|e| FetchError::Failed(fontaine_tools::error_chain::chain(&e.without_url())))?;
    for name in missing {
        let target = dir.join(name);
        // A temp file of its own, so a second session fetching the same files never writes
        // into this one's; a failed fetch drops it.
        let part = fetch_one(&client, &format!("{base_url}/{name}"), token, dir, name).await?;
        // Another session may have finished first; its complete file stays.
        if let Err(e) = part.persist_noclobber(&target)
            && !target.is_file()
        {
            return Err(FetchError::Failed(format!(
                "cannot place {}: {}",
                target.display(),
                e.error
            )));
        }
    }
    Ok(())
}

async fn fetch_one(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    dir: &Path,
    name: &str,
) -> Result<tempfile::TempPath, FetchError> {
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
    let cannot_write =
        |e: std::io::Error| FetchError::Failed(format!("cannot write in {}: {e}", dir.display()));
    let (file, part) = tempfile::Builder::new()
        .prefix(&format!("{PART_PREFIX}{name}-"))
        .tempfile_in(dir)
        .map_err(cannot_write)?
        .into_parts();
    let mut file = tokio::fs::File::from_std(file);
    let mut written: u64 = 0;
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        written += chunk.len() as u64;
        file.write_all(&chunk).await.map_err(cannot_write)?;
    }
    file.flush().await.map_err(cannot_write)?;
    if expected.is_some_and(|expected| expected != written) {
        return Err(FetchError::Failed("the response ended early".into()));
    }
    Ok(part)
}
