//! The runtime containment probe, against a fake root directory, and the profile it gates.

use std::path::Path;

use fontaine_core::config::GuardianProfile;
use fontaine_core::containment::{probe, resolve};
use fontaine_guardian::Profile;

fn write(root: &Path, relative: &str, bytes: &[u8]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn root() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    #[test]
    fn each_signal_alone_means_contained() {
        let signals: [(&str, &str, &[u8]); 12] = [
            ("dockerenv", ".dockerenv", b""),
            ("podman", "run/.containerenv", b""),
            (
                "environ",
                "proc/1/environ",
                b"PATH=/bin\0container=podman\0HOME=/root\0",
            ),
            ("docker cgroup", "proc/1/cgroup", b"0::/docker/abc123\n"),
            (
                "kubepods cgroup",
                "proc/1/cgroup",
                b"0::/kubepods/besteffort/pod1\n",
            ),
            (
                "containerd cgroup",
                "proc/1/cgroup",
                b"0::/system.slice/containerd.service\n",
            ),
            ("libpod cgroup", "proc/1/cgroup", b"0::/libpod-abc\n"),
            ("lxc cgroup", "proc/1/cgroup", b"0::/lxc/web\n"),
            (
                "docker v1 cgroup",
                "proc/1/cgroup",
                b"12:cpu:/docker/abc\n11:memory:/docker/abc\n",
            ),
            ("environ only", "proc/1/environ", b"container=docker\0"),
            ("environ last", "proc/1/environ", b"A=1\0container=lxc"),
            ("both cgroup and env", "proc/1/cgroup", b"0::/docker/x\n"),
        ];
        for (name, relative, bytes) in signals {
            let dir = root();
            write(dir.path(), relative, bytes);
            assert!(probe(dir.path()), "{name}");
        }
    }

    #[test]
    fn an_empty_root_and_a_plain_host_are_not_contained() {
        let dir = root();
        assert!(!probe(dir.path()));
        write(dir.path(), "proc/1/cgroup", b"0::/init.scope\n");
        write(dir.path(), "proc/1/environ", b"PATH=/bin\0HOME=/root\0");
        assert!(!probe(dir.path()));
        // A variable that merely contains the word is not the marker.
        write(
            dir.path(),
            "proc/1/environ",
            b"MY_container=1\0container_x=1\0",
        );
        assert!(!probe(dir.path()));
    }

    #[test]
    fn an_unreadable_environ_is_no_signal_and_not_an_error() {
        let dir = root();
        // A directory where the file should be cannot be read as one.
        std::fs::create_dir_all(dir.path().join("proc/1/environ")).unwrap();
        write(dir.path(), "proc/1/cgroup", b"0::/init.scope\n");
        assert!(!probe(dir.path()));
        // Another signal still counts.
        write(dir.path(), ".dockerenv", b"");
        assert!(probe(dir.path()));
    }
}

#[cfg(not(target_os = "linux"))]
#[test]
fn macos_and_windows_always_report_not_contained() {
    let dir = root();
    write(dir.path(), ".dockerenv", b"");
    write(dir.path(), "proc/1/cgroup", b"0::/docker/x\n");
    assert!(!probe(dir.path()));
}

#[test]
fn a_positive_probe_without_the_flag_and_the_flag_without_a_probe_both_fall_back_to_full() {
    for (flag, probed) in [(false, true), (true, false), (false, false)] {
        let resolved = resolve(GuardianProfile::Minimal, flag, probed);
        assert_eq!(resolved.profile, Profile::Full, "{flag} {probed}");
        let notice = resolved.notice.expect("one notice");
        assert!(notice.contains("minimal"), "{notice}");
    }
    let resolved = resolve(GuardianProfile::Minimal, true, true);
    assert_eq!(resolved.profile, Profile::Minimal);
    assert_eq!(resolved.notice, None);
}

#[test]
fn the_flag_alone_never_selects_minimal() {
    let resolved = resolve(GuardianProfile::Full, true, true);
    assert_eq!(resolved.profile, Profile::Full);
    assert_eq!(resolved.notice, None);
}
