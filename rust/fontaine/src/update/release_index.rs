//! Finding the newest build on a channel, and comparing it with the installed one.

use std::cmp::Ordering;

use serde::Deserialize;

use crate::release_signature::Channel;

pub const DEFAULT_API_BASE: &str = "https://api.github.com";
const REPO: &str = "madesroches/fontaine";
const PER_PAGE: usize = 100;
// Dev builds are published once per `main` run, so the newest is always on the first pages.
const MAX_PAGES: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub build: Build,
    pub assets: Vec<Asset>,
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

/// What a tag stands for: a semver on the release channel, a build number on dev.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Build {
    Release(SemVer),
    Dev(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SemVer(pub u64, pub u64, pub u64);

impl SemVer {
    /// `X.Y.Z`, ignoring any `-pre` or `+build` suffix.
    pub fn parse(text: &str) -> Option<SemVer> {
        let core = text.split(['-', '+']).next()?;
        let mut parts = core.split('.');
        let mut next = || parts.next()?.parse::<u64>().ok();
        let version = SemVer(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }
}

/// `<semver>-dev.<n>+<hex>` -> `n`.
pub fn dev_build_number(version: &str) -> Option<u64> {
    let (head, build) = version.split_once('+')?;
    if build.is_empty() || !build.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let (base, n) = head.split_once("-dev.")?;
    SemVer::parse(base).filter(|_| !base.contains(['-', '+']))?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

/// The build a receipt or signed version string names on `channel`.
pub fn installed_build(channel: Channel, version: &str) -> Option<Build> {
    match channel {
        Channel::Release => SemVer::parse(version).map(Build::Release),
        Channel::Dev => dev_build_number(version).map(Build::Dev),
    }
}

/// How `candidate` compares with `installed`; `None` when they are on different channels.
pub fn compare(installed: Build, candidate: Build) -> Option<Ordering> {
    match (installed, candidate) {
        (Build::Release(a), Build::Release(b)) => Some(b.cmp(&a)),
        (Build::Dev(a), Build::Dev(b)) => Some(b.cmp(&a)),
        _ => None,
    }
}

/// The tag's build on `channel`, if the tag has the channel's shape.
pub fn tag_build(channel: Channel, tag: &str) -> Option<Build> {
    match channel {
        Channel::Release => {
            let rest = tag.strip_prefix('v')?;
            if rest.contains(['-', '+']) {
                return None;
            }
            SemVer::parse(rest).map(Build::Release)
        }
        Channel::Dev => {
            let n = tag.strip_prefix("dev-")?;
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            n.parse().ok().map(Build::Dev)
        }
    }
}

#[derive(Debug)]
pub enum IndexError {
    Request(reqwest::Error),
    Status(reqwest::StatusCode, String),
    Parse(serde_json::Error),
    NoRelease(Channel),
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Request(_) => f.write_str("the release list could not be fetched"),
            IndexError::Status(status, url) => write!(f, "{url} answered {status}"),
            IndexError::Parse(_) => f.write_str("the release list is not the expected JSON"),
            IndexError::NoRelease(c) => write!(f, "no published {c} release was found"),
        }
    }
}

impl std::error::Error for IndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IndexError::Request(e) => Some(e),
            IndexError::Parse(e) => Some(e),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
}

impl ApiRelease {
    fn into_release(self, build: Build) -> Release {
        Release {
            tag: self.tag_name,
            build,
            assets: self
                .assets
                .into_iter()
                .map(|a| Asset {
                    name: a.name,
                    url: a.browser_download_url,
                })
                .collect(),
        }
    }
}

async fn get(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, IndexError> {
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(IndexError::Request)?;
    let status = response.status();
    if !status.is_success() {
        return Err(IndexError::Status(status, url.to_string()));
    }
    Ok(response
        .bytes()
        .await
        .map_err(IndexError::Request)?
        .to_vec())
}

/// The newest published build on `channel`. `api_base` is a parameter so tests can point it at a
/// mock server.
pub async fn latest(
    client: &reqwest::Client,
    api_base: &str,
    channel: Channel,
) -> Result<Release, IndexError> {
    let base = api_base.trim_end_matches('/');
    match channel {
        Channel::Release => {
            let body = get(client, &format!("{base}/repos/{REPO}/releases/latest")).await?;
            let release: ApiRelease = serde_json::from_slice(&body).map_err(IndexError::Parse)?;
            let build =
                tag_build(channel, &release.tag_name).ok_or(IndexError::NoRelease(channel))?;
            Ok(release.into_release(build))
        }
        Channel::Dev => {
            let mut best: Option<(u64, ApiRelease)> = None;
            for page in 1..=MAX_PAGES {
                let url = format!("{base}/repos/{REPO}/releases?per_page={PER_PAGE}&page={page}");
                let body = get(client, &url).await?;
                let releases: Vec<ApiRelease> =
                    serde_json::from_slice(&body).map_err(IndexError::Parse)?;
                let last_page = releases.len() < PER_PAGE;
                for release in releases.into_iter().filter(|r| !r.draft) {
                    if let Some(Build::Dev(n)) = tag_build(channel, &release.tag_name)
                        && best.as_ref().is_none_or(|(m, _)| n > *m)
                    {
                        best = Some((n, release));
                    }
                }
                if last_page {
                    break;
                }
            }
            let (n, release) = best.ok_or(IndexError::NoRelease(channel))?;
            Ok(release.into_release(Build::Dev(n)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_versions_parse() {
        assert_eq!(dev_build_number("0.1.0-dev.42+abcdef1"), Some(42));
        assert_eq!(dev_build_number("0.1.0-dev.x+abcdef1"), None);
        assert_eq!(dev_build_number("0.1.0-dev.4"), None);
        assert_eq!(dev_build_number("0.1.0+abc"), None);
        assert_eq!(dev_build_number("0.1-dev.4+abc"), None);
        assert_eq!(dev_build_number("0.1.0-dev.4+xyz"), None);
    }

    #[test]
    fn tags_have_channel_shapes() {
        assert_eq!(
            tag_build(Channel::Release, "v1.2.3"),
            Some(Build::Release(SemVer(1, 2, 3)))
        );
        assert_eq!(tag_build(Channel::Release, "v1.2.3-rc.1"), None);
        assert_eq!(tag_build(Channel::Release, "dev-4"), None);
        assert_eq!(tag_build(Channel::Dev, "dev-12"), Some(Build::Dev(12)));
        assert_eq!(tag_build(Channel::Dev, "dev"), None);
        assert_eq!(tag_build(Channel::Dev, "dev-1a"), None);
    }

    #[test]
    fn comparison_is_numeric_and_per_channel() {
        let rel = |a, b, c| Build::Release(SemVer(a, b, c));
        assert_eq!(
            compare(rel(0, 9, 0), rel(0, 10, 0)),
            Some(Ordering::Greater)
        );
        assert_eq!(compare(rel(1, 0, 0), rel(1, 0, 0)), Some(Ordering::Equal));
        assert_eq!(compare(rel(1, 0, 1), rel(1, 0, 0)), Some(Ordering::Less));
        assert_eq!(
            compare(Build::Dev(9), Build::Dev(10)),
            Some(Ordering::Greater)
        );
        assert_eq!(compare(Build::Dev(10), Build::Dev(9)), Some(Ordering::Less));
        assert_eq!(compare(Build::Dev(1), rel(1, 0, 0)), None);
    }
}
