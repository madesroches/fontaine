//! Verification of the detached minisign signatures CI makes over every release archive.
//!
//! The trusted comment binds a signature to a channel and an asset name, so a validly signed
//! archive cannot be replayed as another target or on the other channel. Only [`Verified`] means
//! success; every other outcome, I/O included, is a failure and the caller must leave the
//! installed binary untouched.

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::OnceLock;

use minisign_verify::{PublicKey, Signature};

const EMBEDDED_RELEASE: &str = include_str!("../keys/release.pub");
const EMBEDDED_DEV: &str = include_str!("../keys/dev.pub");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Release,
    Dev,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Release => "release",
            Channel::Dev => "dev",
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One public key per channel; `None` is a channel whose key has not been bootstrapped yet.
pub struct TrustedKeys {
    release: Option<PublicKey>,
    dev: Option<PublicKey>,
}

impl TrustedKeys {
    /// The keys compiled into this binary.
    pub fn embedded() -> &'static TrustedKeys {
        static KEYS: OnceLock<TrustedKeys> = OnceLock::new();
        KEYS.get_or_init(|| {
            TrustedKeys::from_pub_files(EMBEDDED_RELEASE, EMBEDDED_DEV)
                .expect("shipped key files parse; enforced by the shipped-keys unit test")
        })
    }

    /// Parse `.pub` file contents. A file holding only a comment line is the bootstrap
    /// placeholder and yields no key for that channel.
    pub fn from_pub_files(release: &str, dev: &str) -> Result<TrustedKeys, KeyFileError> {
        Ok(TrustedKeys {
            release: parse_pub_file(release).map_err(KeyFileError::MalformedReleaseKey)?,
            dev: parse_pub_file(dev).map_err(KeyFileError::MalformedDevKey)?,
        })
    }

    fn key(&self, channel: Channel) -> Option<&PublicKey> {
        match channel {
            Channel::Release => self.release.as_ref(),
            Channel::Dev => self.dev.as_ref(),
        }
    }
}

fn parse_pub_file(contents: &str) -> Result<Option<PublicKey>, minisign_verify::Error> {
    let mut lines = contents.lines();
    let _comment = lines.next();
    if lines.all(|l| l.trim().is_empty()) {
        return if contents.trim().is_empty() {
            Err(minisign_verify::Error::InvalidEncoding)
        } else {
            Ok(None)
        };
    }
    PublicKey::decode(contents).map(Some)
}

#[derive(Debug, PartialEq, Eq)]
pub struct Verified {
    pub channel: Channel,
    pub file: String,
    pub version: String,
    pub commit: String,
}

#[derive(Debug)]
pub enum VerifyError {
    NoKeyForChannel(Channel),
    MissingSignature,
    MalformedSignature(minisign_verify::Error),
    /// The signature's key ID is not this channel's key (for example the other channel's).
    WrongKey {
        channel: Channel,
    },
    BadSignature(minisign_verify::Error),
    MalformedTrustedComment,
    ChannelMismatch {
        expected: Channel,
        found: String,
    },
    FileMismatch {
        expected: String,
        found: String,
    },
    Io(std::io::Error),
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VerifyError::NoKeyForChannel(c) => {
                write!(f, "no signing key is compiled in for the {c} channel")
            }
            VerifyError::MissingSignature => f.write_str("the archive has no signature"),
            VerifyError::MalformedSignature(_) => f.write_str("the signature is malformed"),
            VerifyError::WrongKey { channel } => {
                write!(
                    f,
                    "the signature was not made by the {channel} channel's key"
                )
            }
            VerifyError::BadSignature(_) => f.write_str("the signature does not match the archive"),
            VerifyError::MalformedTrustedComment => {
                f.write_str("the signed comment is not a fontaine-signature v1 binding")
            }
            VerifyError::ChannelMismatch { expected, found } => write!(
                f,
                "the signature is for channel {found:?}, expected {expected}"
            ),
            VerifyError::FileMismatch { expected, found } => write!(
                f,
                "the signature is for file {found:?}, expected {expected:?}"
            ),
            VerifyError::Io(_) => f.write_str("could not read the archive"),
        }
    }
}

impl std::error::Error for VerifyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            VerifyError::MalformedSignature(e) | VerifyError::BadSignature(e) => Some(e),
            VerifyError::Io(e) => Some(e),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum KeyFileError {
    MalformedReleaseKey(minisign_verify::Error),
    MalformedDevKey(minisign_verify::Error),
}

impl fmt::Display for KeyFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyFileError::MalformedReleaseKey(_) => {
                f.write_str("the release public key is malformed")
            }
            KeyFileError::MalformedDevKey(_) => f.write_str("the dev public key is malformed"),
        }
    }
}

impl std::error::Error for KeyFileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            KeyFileError::MalformedReleaseKey(e) | KeyFileError::MalformedDevKey(e) => Some(e),
        }
    }
}

fn classify(channel: Channel, e: minisign_verify::Error) -> VerifyError {
    use minisign_verify::Error as E;
    match e {
        E::InvalidEncoding
        | E::UnsupportedAlgorithm
        | E::UnexpectedAlgorithm
        | E::UnsupportedLegacyMode => VerifyError::MalformedSignature(e),
        E::UnexpectedKeyId => VerifyError::WrongKey { channel },
        E::InvalidSignature => VerifyError::BadSignature(e),
        E::IoError(io) => VerifyError::Io(io),
    }
}

/// Verify `archive` against `signature` (the text of its `.minisig`) with the channel's key.
///
/// `expected_asset` is the release asset name the caller asked for, not the on-disk name, so a
/// download saved under a temporary name still verifies. `version` and `commit` are returned for
/// the caller's downgrade and channel-switch rules; this function enforces neither.
pub fn verify_file(
    keys: &TrustedKeys,
    channel: Channel,
    archive: &Path,
    expected_asset: &str,
    signature: Option<&str>,
) -> Result<Verified, VerifyError> {
    let key = keys
        .key(channel)
        .ok_or(VerifyError::NoKeyForChannel(channel))?;
    let signature = signature.ok_or(VerifyError::MissingSignature)?;
    let signature = Signature::decode(signature).map_err(|e| classify(channel, e))?;
    let mut verifier = key
        .verify_stream(&signature)
        .map_err(|e| classify(channel, e))?;

    let mut file = File::open(archive).map_err(VerifyError::Io)?;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(VerifyError::Io)?;
        if n == 0 {
            break;
        }
        verifier.update(&buf[..n]);
    }
    verifier.finalize().map_err(|e| classify(channel, e))?;

    let bound = parse_trusted_comment(signature.trusted_comment())?;
    if bound.channel != channel.as_str() {
        return Err(VerifyError::ChannelMismatch {
            expected: channel,
            found: bound.channel.to_string(),
        });
    }
    if bound.file != expected_asset {
        return Err(VerifyError::FileMismatch {
            expected: expected_asset.to_string(),
            found: bound.file.to_string(),
        });
    }
    Ok(Verified {
        channel,
        file: bound.file.to_string(),
        version: bound.version.to_string(),
        commit: bound.commit.to_string(),
    })
}

struct Binding<'a> {
    channel: &'a str,
    file: &'a str,
    version: &'a str,
    commit: &'a str,
}

/// Parses `fontaine-signature v1 channel=.. file=.. version=.. commit=..`.
fn parse_trusted_comment(comment: &str) -> Result<Binding<'_>, VerifyError> {
    let mut parts = comment.split(' ');
    if parts.next() != Some("fontaine-signature") || parts.next() != Some("v1") {
        return Err(VerifyError::MalformedTrustedComment);
    }
    let mut field = |name: &str| -> Result<&str, VerifyError> {
        parts
            .next()
            .and_then(|p| p.strip_prefix(name))
            .and_then(|p| p.strip_prefix('='))
            .filter(|v| !v.is_empty())
            .ok_or(VerifyError::MalformedTrustedComment)
    };
    let binding = Binding {
        channel: field("channel")?,
        file: field("file")?,
        version: field("version")?,
        commit: field("commit")?,
    };
    if parts.next().is_some() {
        return Err(VerifyError::MalformedTrustedComment);
    }
    Ok(binding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use minisign::KeyPair;

    const SHIPPED: [(&str, &str); 4] = [
        ("release.pub", include_str!("../keys/release.pub")),
        ("dev.pub", include_str!("../keys/dev.pub")),
        (
            "release.signing.pub",
            include_str!("../keys/release.signing.pub"),
        ),
        ("dev.signing.pub", include_str!("../keys/dev.signing.pub")),
    ];

    struct Signer {
        pair: KeyPair,
    }

    impl Signer {
        fn new() -> Signer {
            Signer {
                pair: KeyPair::generate_unencrypted_keypair().expect("generate a disposable key"),
            }
        }

        fn pub_file(&self) -> String {
            self.pair.pk.to_box().expect("public key box").into_string()
        }

        fn sign(&self, data: &[u8], trusted_comment: &str) -> String {
            minisign::sign(
                Some(&self.pair.pk),
                &self.pair.sk,
                data,
                Some(trusted_comment),
                None,
            )
            .expect("sign")
            .into_string()
        }
    }

    fn comment(channel: &str, file: &str) -> String {
        format!("fontaine-signature v1 channel={channel} file={file} version=0.1.0 commit=abc123")
    }

    const ASSET: &str = "fontaine-x86_64-unknown-linux-gnu.tar.xz";
    const DATA: &[u8] = b"archive bytes";

    fn archive(dir: &tempfile::TempDir, name: &str, data: &[u8]) -> std::path::PathBuf {
        let p = dir.path().join(name);
        std::fs::write(&p, data).expect("write archive");
        p
    }

    fn keys(release: &Signer, dev: &Signer) -> TrustedKeys {
        TrustedKeys::from_pub_files(&release.pub_file(), &dev.pub_file()).expect("keys parse")
    }

    #[test]
    fn valid_signature_returns_the_parsed_fields() {
        let (release, dev) = (Signer::new(), Signer::new());
        let dir = tempfile::tempdir().unwrap();
        let path = archive(&dir, ASSET, DATA);
        let sig = release.sign(DATA, &comment("release", ASSET));
        let v = verify_file(
            &keys(&release, &dev),
            Channel::Release,
            &path,
            ASSET,
            Some(&sig),
        )
        .expect("verifies");
        assert_eq!(
            v,
            Verified {
                channel: Channel::Release,
                file: ASSET.into(),
                version: "0.1.0".into(),
                commit: "abc123".into(),
            }
        );
    }

    #[test]
    fn archive_saved_under_a_temp_name_verifies_by_asset_name() {
        let (release, dev) = (Signer::new(), Signer::new());
        let dir = tempfile::tempdir().unwrap();
        let path = archive(&dir, "download.tmp", DATA);
        let sig = release.sign(DATA, &comment("release", ASSET));
        verify_file(
            &keys(&release, &dev),
            Channel::Release,
            &path,
            ASSET,
            Some(&sig),
        )
        .expect("verifies");
    }

    #[test]
    fn missing_signature() {
        let (release, dev) = (Signer::new(), Signer::new());
        let dir = tempfile::tempdir().unwrap();
        let path = archive(&dir, ASSET, DATA);
        let err =
            verify_file(&keys(&release, &dev), Channel::Release, &path, ASSET, None).unwrap_err();
        assert!(matches!(err, VerifyError::MissingSignature), "{err:?}");
    }

    #[test]
    fn malformed_signatures() {
        let (release, dev) = (Signer::new(), Signer::new());
        let dir = tempfile::tempdir().unwrap();
        let path = archive(&dir, ASSET, DATA);
        let good = release.sign(DATA, &comment("release", ASSET));
        let lines: Vec<&str> = good.lines().collect();
        let truncated = format!("{}\n{}\n", lines[0], &lines[1][..lines[1].len() / 2]);

        // Re-encode only the algorithm bytes to the legacy `Ed`, keeping the key ID, so the
        // key-ID check passes and the legacy rejection is what fires.
        let mut raw = base64::engine::general_purpose::STANDARD
            .decode(lines[1])
            .unwrap();
        assert_eq!(&raw[..2], b"ED");
        raw[1] = b'd';
        let legacy = format!(
            "{}\n{}\n{}\n{}\n",
            lines[0],
            base64::engine::general_purpose::STANDARD.encode(raw),
            lines[2],
            lines[3]
        );

        let keys = keys(&release, &dev);
        for bad in [truncated.as_str(), "garbage", "", legacy.as_str()] {
            let err = verify_file(&keys, Channel::Release, &path, ASSET, Some(bad)).unwrap_err();
            assert!(
                matches!(err, VerifyError::MalformedSignature(_)),
                "{bad:?}: {err:?}"
            );
        }
    }

    #[test]
    fn signature_from_another_key_is_wrong_key() {
        let (release, dev, other) = (Signer::new(), Signer::new(), Signer::new());
        let dir = tempfile::tempdir().unwrap();
        let path = archive(&dir, ASSET, DATA);
        let sig = other.sign(DATA, &comment("release", ASSET));
        let err = verify_file(
            &keys(&release, &dev),
            Channel::Release,
            &path,
            ASSET,
            Some(&sig),
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                VerifyError::WrongKey {
                    channel: Channel::Release
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn dev_signature_on_the_release_channel_is_wrong_key() {
        let (release, dev) = (Signer::new(), Signer::new());
        let dir = tempfile::tempdir().unwrap();
        let path = archive(&dir, ASSET, DATA);
        let sig = dev.sign(DATA, &comment("release", ASSET));
        let err = verify_file(
            &keys(&release, &dev),
            Channel::Release,
            &path,
            ASSET,
            Some(&sig),
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                VerifyError::WrongKey {
                    channel: Channel::Release
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn tampered_archive_is_a_bad_signature() {
        let (release, dev) = (Signer::new(), Signer::new());
        let dir = tempfile::tempdir().unwrap();
        let path = archive(&dir, ASSET, b"tampered bytes");
        let sig = release.sign(DATA, &comment("release", ASSET));
        let err = verify_file(
            &keys(&release, &dev),
            Channel::Release,
            &path,
            ASSET,
            Some(&sig),
        )
        .unwrap_err();
        assert!(matches!(err, VerifyError::BadSignature(_)), "{err:?}");
    }

    #[test]
    fn channel_field_must_match() {
        let (release, dev) = (Signer::new(), Signer::new());
        let dir = tempfile::tempdir().unwrap();
        let path = archive(&dir, ASSET, DATA);
        let sig = release.sign(DATA, &comment("dev", ASSET));
        let err = verify_file(
            &keys(&release, &dev),
            Channel::Release,
            &path,
            ASSET,
            Some(&sig),
        )
        .unwrap_err();
        match err {
            VerifyError::ChannelMismatch { expected, found } => {
                assert_eq!(expected, Channel::Release);
                assert_eq!(found, "dev");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn file_field_must_match_the_expected_asset() {
        let (release, dev) = (Signer::new(), Signer::new());
        let dir = tempfile::tempdir().unwrap();
        let path = archive(&dir, ASSET, DATA);
        let sig = release.sign(DATA, &comment("release", ASSET));
        let err = verify_file(
            &keys(&release, &dev),
            Channel::Release,
            &path,
            "fontaine-aarch64-apple-darwin.tar.xz",
            Some(&sig),
        )
        .unwrap_err();
        assert!(matches!(err, VerifyError::FileMismatch { .. }), "{err:?}");
    }

    #[test]
    fn trusted_comment_without_the_v1_prefix_is_malformed() {
        let (release, dev) = (Signer::new(), Signer::new());
        let dir = tempfile::tempdir().unwrap();
        let path = archive(&dir, ASSET, DATA);
        let keys = keys(&release, &dev);
        for bad in [
            format!("channel=release file={ASSET} version=0.1.0 commit=abc123"),
            format!("fontaine-signature v2 channel=release file={ASSET} version=0.1.0 commit=abc"),
            format!("fontaine-signature v1 channel=release file={ASSET} version=0.1.0"),
            format!("{} extra=1", comment("release", ASSET)),
        ] {
            let sig = release.sign(DATA, &bad);
            let err = verify_file(&keys, Channel::Release, &path, ASSET, Some(&sig)).unwrap_err();
            assert!(
                matches!(err, VerifyError::MalformedTrustedComment),
                "{bad}: {err:?}"
            );
        }
    }

    #[test]
    fn placeholder_key_is_no_key_for_the_channel() {
        let release = Signer::new();
        let placeholder = "untrusted comment: fontaine dev key not yet bootstrapped\n";
        let keys = TrustedKeys::from_pub_files(&release.pub_file(), placeholder).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = archive(&dir, ASSET, DATA);
        let sig = release.sign(DATA, &comment("dev", ASSET));
        let err = verify_file(&keys, Channel::Dev, &path, ASSET, Some(&sig)).unwrap_err();
        assert!(
            matches!(err, VerifyError::NoKeyForChannel(Channel::Dev)),
            "{err:?}"
        );
    }

    #[test]
    fn malformed_key_files_are_rejected() {
        let good = Signer::new().pub_file();
        let bad = "untrusted comment: x\nnot-base64!\n";
        assert!(matches!(
            TrustedKeys::from_pub_files(bad, &good),
            Err(KeyFileError::MalformedReleaseKey(_))
        ));
        assert!(matches!(
            TrustedKeys::from_pub_files(&good, bad),
            Err(KeyFileError::MalformedDevKey(_))
        ));
    }

    fn key_id(contents: &str) -> Option<Vec<u8>> {
        let key_line = contents.lines().nth(1)?;
        let key = minisign::PublicKey::from_base64(key_line.trim()).expect("shipped key parses");
        Some(key.keynum().to_vec())
    }

    #[test]
    fn shipped_key_files_are_keys_or_placeholders_and_channels_differ() {
        for (name, contents) in SHIPPED {
            if key_id(contents).is_none() {
                assert!(
                    contents.starts_with("untrusted comment: fontaine ")
                        && contents.trim_end().lines().count() == 1,
                    "{name} is neither a key nor the bootstrap placeholder"
                );
            } else {
                parse_pub_file(contents)
                    .unwrap_or_else(|e| panic!("{name} does not parse: {e}"))
                    .unwrap_or_else(|| panic!("{name} parsed as a placeholder"));
            }
        }
        TrustedKeys::from_pub_files(SHIPPED[0].1, SHIPPED[1].1).expect("embedded keys parse");

        let release_ids: Vec<_> = [SHIPPED[0].1, SHIPPED[2].1]
            .into_iter()
            .filter_map(key_id)
            .collect();
        let dev_ids: Vec<_> = [SHIPPED[1].1, SHIPPED[3].1]
            .into_iter()
            .filter_map(key_id)
            .collect();
        for id in &release_ids {
            assert!(
                !dev_ids.contains(id),
                "the release and dev channels share a key"
            );
        }
    }

    /// Runs only in CI's release-signing step, on files made by the real minisign CLI and
    /// `release-sign.sh`, so the CLI's output format and this verifier cannot drift apart.
    #[test]
    #[ignore = "needs FONTAINE_SIGNING_FIXTURE_DIR from CI's release-signing step"]
    fn cli_interop() {
        let dir = std::path::PathBuf::from(
            std::env::var("FONTAINE_SIGNING_FIXTURE_DIR")
                .expect("FONTAINE_SIGNING_FIXTURE_DIR is set"),
        );
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).expect("read fixture key");
        let keys = TrustedKeys::from_pub_files(&read("release.pub"), &read("dev.pub"))
            .expect("fixture keys parse");

        let mut verified = 0;
        for entry in std::fs::read_dir(&dir).expect("read fixture dir") {
            let path = entry.expect("dir entry").path();
            let name = path.file_name().unwrap().to_str().unwrap().to_string();
            if !name.starts_with("fontaine-")
                || name.ends_with(".minisig")
                || name.ends_with(".sha256")
            {
                continue;
            }
            let sig = std::fs::read_to_string(dir.join(format!("{name}.minisig")))
                .expect("read signature");
            let v = verify_file(&keys, Channel::Release, &path, &name, Some(&sig))
                .unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(v.channel, Channel::Release);
            assert_eq!(v.file, name);
            verified += 1;

            let err = verify_file(&keys, Channel::Dev, &path, &name, Some(&sig)).unwrap_err();
            assert!(matches!(err, VerifyError::WrongKey { .. }), "{err:?}");
        }
        assert!(verified > 0, "no archives in the fixture directory");
    }
}
