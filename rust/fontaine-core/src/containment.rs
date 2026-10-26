//! Whether the process runs inside a container, and the guardian profile that follows. The
//! `minimal` profile narrows what the guardian judges because the container bounds the blast
//! radius, so it is only honored when containment is verified: the user asserts it with
//! `--in-container` and a runtime probe confirms it. Either alone is not enough.

use std::path::Path;

use fontaine_guardian::Profile;

use crate::config::GuardianProfile;
use crate::notice::Notice;

/// Substrings of `/proc/1/cgroup` that mark a container runtime.
const CGROUP_MARKERS: [&str; 5] = ["docker", "kubepods", "containerd", "libpod", "lxc"];

/// Whether the filesystem rooted at `root` looks like the inside of a container. Linux only:
/// macOS and Windows report "not contained", so `minimal` falls back to `full` there. Any one
/// signal is enough; an unreadable file is no signal.
pub fn probe(root: &Path) -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    if root.join(".dockerenv").exists() || root.join("run/.containerenv").exists() {
        return true;
    }
    if let Ok(environ) = std::fs::read(root.join("proc/1/environ"))
        && environ
            .split(|byte| *byte == 0)
            .any(|entry| entry.starts_with(b"container="))
    {
        return true;
    }
    std::fs::read_to_string(root.join("proc/1/cgroup"))
        .is_ok_and(|cgroup| CGROUP_MARKERS.iter().any(|marker| cgroup.contains(marker)))
}

/// The probe against the real filesystem.
pub fn detect() -> bool {
    probe(Path::new("/"))
}

/// The profile a session runs, and what to tell the user when it differs from the configured one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub profile: Profile,
    pub notice: Option<Notice>,
}

/// `minimal` needs both the user's assertion and a positive probe; anything else is `full`,
/// with a notice when `minimal` was configured.
pub fn resolve(configured: GuardianProfile, in_container_flag: bool, probed: bool) -> Resolved {
    match configured {
        GuardianProfile::Full => Resolved {
            profile: Profile::Full,
            notice: None,
        },
        GuardianProfile::Minimal if in_container_flag && probed => Resolved {
            profile: Profile::Minimal,
            notice: None,
        },
        GuardianProfile::Minimal => {
            let why = match (in_container_flag, probed) {
                (false, true) => {
                    "it is only used when `--in-container` asserts the container, and that flag was not given"
                }
                (true, false) => {
                    "this host shows no sign of a container, so `--in-container` could not be verified"
                }
                _ => {
                    "it needs `--in-container` and a host that shows the signs of a container, and neither holds"
                }
            };
            Resolved {
                profile: Profile::Full,
                notice: Some(Notice::warning(
                    format!(
                        "`[guardian] profile = \"minimal\"` is not in effect: {why}; running the full guardian"
                    ),
                    None,
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_never_needs_verification() {
        let r = resolve(GuardianProfile::Full, true, true);
        assert_eq!(r.profile, Profile::Full);
        assert_eq!(r.notice, None);
    }

    #[test]
    fn minimal_needs_the_flag_and_the_probe() {
        let both = resolve(GuardianProfile::Minimal, true, true);
        assert_eq!((both.profile, both.notice), (Profile::Minimal, None));
        for (flag, probed) in [(false, true), (true, false), (false, false)] {
            let r = resolve(GuardianProfile::Minimal, flag, probed);
            assert_eq!(r.profile, Profile::Full, "{flag} {probed}");
            let notice = r.notice.expect("a notice");
            assert!(notice.contains("minimal"), "{notice}");
        }
    }
}
