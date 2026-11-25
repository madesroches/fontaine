//! Finding the newest build on a channel, and comparing it with the installed one.

use std::cmp::Ordering;

use serde::Deserialize;

use crate::release_signature::Channel;

pub const DEFAULT_DOWNLOAD_BASE: &str = "https://github.com";
const REPO: &str = "madesroches/fontaine";

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
    /// The release pointer {url} could not be fetched or is invalid.
    Pointer {
        url: String,
        cause: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IndexError::Pointer { url, .. } => write!(
                f,
                "the release pointer {url} could not be fetched or is invalid; try again shortly"
            ),
        }
    }
}

impl std::error::Error for IndexError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IndexError::Pointer { cause, .. } => cause.as_deref().map(|e| e as _),
        }
    }
}

#[derive(Deserialize)]
struct Pointer {
    tag: String,
    assets: Vec<String>,
}

/// Fetches and parses a pointer with a plain GET: it lives on the download host, not the API,
/// so every failure maps to `IndexError`.
async fn fetch_pointer(client: &reqwest::Client, url: &str) -> Result<Pointer, IndexError> {
    let fail = |cause: Option<Box<dyn std::error::Error + Send + Sync>>| IndexError::Pointer {
        url: url.to_string(),
        cause,
    };
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| fail(Some(Box::new(e))))?;
    let response = response
        .error_for_status()
        .map_err(|e| fail(Some(Box::new(e))))?;
    let body = response
        .bytes()
        .await
        .map_err(|e| fail(Some(Box::new(e))))?;
    serde_json::from_slice(&body).map_err(|e| fail(Some(Box::new(e))))
}

/// Where a channel's pointer lives. The release one is GitHub's `latest` redirect, which skips
/// prereleases and drafts.
fn pointer_url(base: &str, channel: Channel) -> String {
    match channel {
        Channel::Release => format!("{base}/{REPO}/releases/latest/download/latest.json"),
        Channel::Dev => format!("{base}/{REPO}/releases/download/dev/latest-dev.json"),
    }
}

/// The newest published build on `channel`, read from its pointer. `download_base` is a
/// parameter so tests can point it at a mock server.
pub async fn latest(
    client: &reqwest::Client,
    download_base: &str,
    channel: Channel,
) -> Result<Release, IndexError> {
    let base = download_base.trim_end_matches('/');
    let url = pointer_url(base, channel);
    let pointer = fetch_pointer(client, &url).await?;
    let build = tag_build(channel, &pointer.tag).ok_or_else(|| IndexError::Pointer {
        url: url.clone(),
        cause: Some(format!("{:?} is not a {channel} build tag", pointer.tag).into()),
    })?;
    let tag = pointer.tag;
    let assets = pointer
        .assets
        .into_iter()
        .map(|name| Asset {
            url: format!("{base}/{REPO}/releases/download/{tag}/{name}"),
            name,
        })
        .collect();
    Ok(Release { tag, build, assets })
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

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const DEV_POINTER_PATH: &str = "/madesroches/fontaine/releases/download/dev/latest-dev.json";
    const RELEASE_POINTER_PATH: &str = "/madesroches/fontaine/releases/latest/download/latest.json";

    async fn serve_pointer(server: &MockServer, at: &str, response: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(response)
            .mount(server)
            .await;
    }

    async fn no_api_calls(server: &MockServer) {
        Mock::given(wiremock::matchers::path_regex("^/repos/"))
            .respond_with(ResponseTemplate::new(403))
            .expect(0)
            .mount(server)
            .await;
    }

    async fn latest_on(server: &MockServer, channel: Channel) -> Result<Release, IndexError> {
        latest(&reqwest::Client::new(), &server.uri(), channel).await
    }

    #[tokio::test]
    async fn the_dev_pointer_names_the_release_without_an_api_call() {
        let server = MockServer::start().await;
        no_api_calls(&server).await;
        serve_pointer(
            &server,
            DEV_POINTER_PATH,
            ResponseTemplate::new(200)
                .set_body_string(r#"{"tag":"dev-7","assets":["a.tar.gz","a.tar.gz.minisig"]}"#),
        )
        .await;
        let release = latest_on(&server, Channel::Dev).await.unwrap();
        assert_eq!(release.tag, "dev-7");
        assert_eq!(release.build, Build::Dev(7));
        let asset = release.asset("a.tar.gz.minisig").unwrap();
        assert_eq!(
            asset.url,
            format!(
                "{}/madesroches/fontaine/releases/download/dev-7/a.tar.gz.minisig",
                server.uri()
            )
        );
        assert_eq!(release.assets.len(), 2);
    }

    #[tokio::test]
    async fn the_release_pointer_names_the_release_without_an_api_call() {
        let server = MockServer::start().await;
        no_api_calls(&server).await;
        serve_pointer(
            &server,
            RELEASE_POINTER_PATH,
            ResponseTemplate::new(200)
                .set_body_string(r#"{"tag":"v1.2.3","assets":["a.tar.xz","a.tar.xz.minisig"]}"#),
        )
        .await;
        let release = latest_on(&server, Channel::Release).await.unwrap();
        assert_eq!(release.tag, "v1.2.3");
        assert_eq!(release.build, Build::Release(SemVer(1, 2, 3)));
        assert_eq!(
            release.asset("a.tar.xz").unwrap().url,
            format!(
                "{}/madesroches/fontaine/releases/download/v1.2.3/a.tar.xz",
                server.uri()
            )
        );
    }

    #[tokio::test]
    async fn the_release_pointer_is_followed_through_githubs_redirect() {
        let server = MockServer::start().await;
        serve_pointer(
            &server,
            RELEASE_POINTER_PATH,
            ResponseTemplate::new(302).insert_header(
                "location",
                "/madesroches/fontaine/releases/download/v1.2.3/latest.json",
            ),
        )
        .await;
        serve_pointer(
            &server,
            "/madesroches/fontaine/releases/download/v1.2.3/latest.json",
            ResponseTemplate::new(200).set_body_string(r#"{"tag":"v1.2.3","assets":["a"]}"#),
        )
        .await;
        let client = crate::update::build_client(&Default::default())
            .expect("the default network settings build a client");
        let release = latest(&client, &server.uri(), Channel::Release)
            .await
            .unwrap();
        assert_eq!(release.tag, "v1.2.3");
    }

    #[tokio::test]
    async fn a_missing_pointer_is_an_error_that_keeps_the_status() {
        for (channel, at) in [
            (Channel::Dev, DEV_POINTER_PATH),
            (Channel::Release, RELEASE_POINTER_PATH),
        ] {
            let server = MockServer::start().await;
            serve_pointer(&server, at, ResponseTemplate::new(404)).await;
            let err = latest_on(&server, channel).await.unwrap_err();
            assert!(matches!(err, IndexError::Pointer { .. }));
            let chain = fontaine_sdk::tools::error_chain::chain(&err);
            assert!(chain.contains("404"), "{chain}");
        }
    }

    #[tokio::test]
    async fn a_malformed_pointer_is_an_error() {
        for (channel, at) in [
            (Channel::Dev, DEV_POINTER_PATH),
            (Channel::Release, RELEASE_POINTER_PATH),
        ] {
            let server = MockServer::start().await;
            serve_pointer(
                &server,
                at,
                ResponseTemplate::new(200).set_body_string("not json"),
            )
            .await;
            let err = latest_on(&server, channel).await.unwrap_err();
            assert!(matches!(err, IndexError::Pointer { .. }));
        }
    }

    #[tokio::test]
    async fn a_pointer_with_a_bad_tag_is_an_error() {
        let cases = [
            (Channel::Dev, DEV_POINTER_PATH, "v1.0.0"),
            (Channel::Release, RELEASE_POINTER_PATH, "dev-3"),
            (Channel::Release, RELEASE_POINTER_PATH, "v1.0.0-rc.1"),
        ];
        for (channel, at, tag) in cases {
            let server = MockServer::start().await;
            let body = format!(r#"{{"tag":"{tag}","assets":[]}}"#);
            serve_pointer(
                &server,
                at,
                ResponseTemplate::new(200).set_body_string(body),
            )
            .await;
            let err = latest_on(&server, channel).await.unwrap_err();
            assert!(matches!(err, IndexError::Pointer { .. }), "{tag}");
        }
    }
}
