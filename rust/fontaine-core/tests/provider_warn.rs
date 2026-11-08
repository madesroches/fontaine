//! The process-wide provider warning sink. It is never unset, so every step that depends on it
//! lives in this one test of its own binary.

mod common;

use std::sync::{Arc, Mutex};

use fontaine_core::providers::warn::{Warn, set_sink};

#[cfg(unix)]
use common::World;
#[cfg(unix)]
use fontaine_core::credentials::{self, OAuthTokens};
#[cfg(unix)]
use fontaine_core::providers::device_flow::RefreshError;
#[cfg(unix)]
use fontaine_core::providers::token_source::{Refresher, TokenSource, now_unix};
#[cfg(unix)]
use futures::future::BoxFuture;

#[cfg(unix)]
struct RotateThenLockDisk(std::path::PathBuf);

#[cfg(unix)]
impl Refresher for RotateThenLockDisk {
    fn refresh<'a>(
        &'a self,
        _current: &'a OAuthTokens,
    ) -> BoxFuture<'a, Result<OAuthTokens, RefreshError>> {
        use std::os::unix::fs::PermissionsExt;
        Box::pin(async move {
            std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o555)).unwrap();
            Ok(OAuthTokens {
                access: "at-new".to_string(),
                refresh: "rt-new".to_string(),
                expires_at: now_unix() + 3600,
                extra: Default::default(),
            })
        })
    }
}

fn collector() -> (
    Arc<Mutex<Vec<String>>>,
    impl Fn(&str) + Send + Sync + 'static,
) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    (seen, move |m: &str| sink.lock().unwrap().push(m.to_owned()))
}

#[tokio::test]
async fn the_process_sink_binds_late_and_the_latest_one_wins() {
    let early = Warn::process();
    early.emit("no sink yet: falls back to tracing");

    let (a, sink_a) = collector();
    set_sink(sink_a);
    early.emit("to a");
    assert_eq!(*a.lock().unwrap(), ["to a"]);

    let (b, sink_b) = collector();
    set_sink(sink_b);
    early.emit("to b");
    assert_eq!(*a.lock().unwrap(), ["to a"]);
    assert_eq!(*b.lock().unwrap(), ["to b"]);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let w = World::new();
        let held = OAuthTokens {
            access: "at-1".to_string(),
            refresh: "rt-1".to_string(),
            expires_at: now_unix() + 60,
            extra: Default::default(),
        };
        credentials::store_oauth(&w.data, "go", &held).unwrap();
        let source = TokenSource::new(
            &w.data,
            "go",
            held,
            Arc::new(RotateThenLockDisk(w.data.clone())),
        );
        assert_eq!(source.token().await.unwrap(), "at-new");
        // Permission bits do not bind root, so the save cannot fail there.
        let bound = std::fs::write(w.data.join(".probe"), b"x").is_err();
        std::fs::set_permissions(&w.data, std::fs::Permissions::from_mode(0o755)).unwrap();
        if bound {
            let seen = b.lock().unwrap();
            assert_eq!(seen.len(), 2, "{seen:?}");
            assert!(
                seen[1].contains("could not save the refreshed sign-in"),
                "{seen:?}"
            );
        }
    }
}
