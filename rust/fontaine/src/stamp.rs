//! The once-a-day throttle shared by the background passes: a stamp file in the data dir whose
//! modification time is when the last pass ran.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// An offline machine or a quick restart does not retry on every start.
const THROTTLE: Duration = Duration::from_secs(24 * 60 * 60);

/// Whether the pass that keeps `file` in `data_dir` last ran long enough ago for another.
pub(crate) fn stamp_due(data_dir: &Path, file: &str, now: SystemTime) -> bool {
    let modified = std::fs::metadata(data_dir.join(file)).and_then(|m| m.modified());
    match modified {
        // A stamp well in the future (the clock moved back) must not stall updates for good.
        Ok(at) => match now.duration_since(at) {
            Ok(age) => age >= THROTTLE,
            Err(ahead) => ahead.duration() > Duration::from_secs(60),
        },
        Err(_) => true,
    }
}

pub(crate) fn write_stamp(data_dir: &Path, file: &str, now: SystemTime) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let seconds = now.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    fontaine_core::owner_only::write_atomic(&data_dir.join(file), format!("{seconds}\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stamp_throttles_the_pass_to_once_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        assert!(stamp_due(dir.path(), "a.stamp", now), "no stamp yet");
        write_stamp(dir.path(), "a.stamp", now).unwrap();
        assert!(!stamp_due(dir.path(), "a.stamp", now));
        assert!(!stamp_due(
            dir.path(),
            "a.stamp",
            now + Duration::from_secs(23 * 3600)
        ));
        assert!(stamp_due(
            dir.path(),
            "a.stamp",
            now + Duration::from_secs(25 * 3600)
        ));
        // A clock that moved back must not stall updates for good.
        assert!(stamp_due(
            dir.path(),
            "a.stamp",
            now - Duration::from_secs(3 * 86_400)
        ));
        assert!(stamp_due(dir.path(), "b.stamp", now), "stamps are per file");
    }
}
