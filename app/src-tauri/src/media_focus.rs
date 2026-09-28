//! Owns temporary interruption of external media, never HereWord's audio or volume.
//! Leases share one interruption; only unchanged players we paused may be resumed.
//! Player/track identity stays in memory and must never enter operational logs.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod platform;

#[cfg(target_os = "windows")]
pub(crate) fn run_quiet_worker(root: u32) {
    platform::run_quiet_worker(root);
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct Snapshot {
    player: String,
    track: String,
    playing: bool,
}

trait MediaPlatform: Send + Sync {
    fn snapshots(&self) -> Result<Vec<Snapshot>, ()>;
    /// Recheck identity and state immediately before sending an explicit command.
    fn set_playing(&self, expected: &Snapshot, playing: bool) -> bool;
    fn quiet(&self) -> Option<Box<dyn QuietAudio>> {
        None
    }
}

trait QuietAudio: Send {
    fn refresh(&mut self);
}

#[derive(Default)]
struct Interruption {
    next_id: u64,
    leases: HashSet<u64>,
    paused: Vec<Snapshot>,
    closed: bool,
    quiet: Option<Box<dyn QuietAudio>>,
}

pub(crate) struct MediaFocus {
    state: Mutex<Interruption>,
    platform: Box<dyn MediaPlatform>,
}

impl MediaFocus {
    pub(crate) fn new() -> Arc<Self> {
        let focus = Arc::new(Self {
            state: Mutex::new(Interruption::default()),
            platform: Box::new(platform::Native),
        });
        let weak = Arc::downgrade(&focus);
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(300));
            let Some(focus) = weak.upgrade() else { break };
            if !focus.observe() {
                break;
            }
        });
        focus
    }

    pub(crate) fn acquire(self: &Arc<Self>, enabled: bool) -> Option<Lease> {
        if !enabled {
            return None;
        }
        let mut state = self.state.lock().ok()?;
        if state.closed {
            return None;
        }
        if state.leases.is_empty() {
            state.quiet = self.platform.quiet();
            state.paused.clear();
            if let Ok(snapshots) = self.platform.snapshots() {
                for mut snapshot in snapshots.into_iter().filter(|s| s.playing) {
                    if self.platform.set_playing(&snapshot, false) {
                        snapshot.playing = false;
                        state.paused.push(snapshot);
                    }
                }
            }
        }
        state.next_id += 1;
        let id = state.next_id;
        state.leases.insert(id);
        Some(Lease {
            focus: self.clone(),
            id,
        })
    }

    /// A manual play, track/player switch, disappearance, or unreadable state
    /// permanently relinquishes ownership for this interruption.
    fn observe(&self) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.closed {
            return false;
        }
        if let Some(quiet) = state.quiet.as_mut() {
            quiet.refresh();
        }
        if !state.paused.is_empty() {
            match self.platform.snapshots() {
                Ok(current) if current.iter().any(|s| s.playing) => state.paused.clear(),
                Ok(current) => state.paused.retain(|paused| current.contains(paused)),
                Err(()) => state.paused.clear(),
            }
        }
        true
    }

    fn release(&self, id: u64) {
        if let Ok(mut state) = self.state.lock() {
            if state.leases.remove(&id) && state.leases.is_empty() {
                self.restore(&mut state);
            }
        }
    }

    fn restore(&self, state: &mut Interruption) {
        // Remove our quieting first so restored media is immediately audible.
        state.quiet.take();
        // Each adapter verifies the same paused identity again before playing.
        let current = self.platform.snapshots().unwrap_or_default();
        if current.iter().any(|s| s.playing) {
            state.paused.clear();
        }
        for paused in state.paused.drain(..) {
            if current.contains(&paused) {
                let _ = self.platform.set_playing(&paused, true);
            }
        }
    }

    pub(crate) fn shutdown(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
            state.leases.clear();
            self.restore(&mut state);
        }
    }
}

pub(crate) struct Lease {
    focus: Arc<MediaFocus>,
    id: u64,
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.focus.release(self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct Fake(Arc<Mutex<Vec<Snapshot>>>);
    impl MediaPlatform for Fake {
        fn snapshots(&self) -> Result<Vec<Snapshot>, ()> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn set_playing(&self, expected: &Snapshot, playing: bool) -> bool {
            let mut state = self.0.lock().unwrap();
            if let Some(item) = state.iter_mut().find(|s| *s == expected) {
                item.playing = playing;
                true
            } else {
                false
            }
        }
    }
    fn fixture(playing: bool) -> (Arc<MediaFocus>, Fake) {
        let fake = Fake(Arc::new(Mutex::new(vec![Snapshot {
            player: "music".into(),
            track: "one".into(),
            playing,
        }])));
        (
            Arc::new(MediaFocus {
                state: Mutex::new(Interruption::default()),
                platform: Box::new(fake.clone()),
            }),
            fake,
        )
    }
    #[test]
    fn disabled_and_already_paused_players_are_untouched() {
        let (focus, fake) = fixture(true);
        assert!(focus.acquire(false).is_none());
        assert!(fake.0.lock().unwrap()[0].playing);
        fake.0.lock().unwrap()[0].playing = false;
        drop(focus.acquire(true));
        assert!(!fake.0.lock().unwrap()[0].playing);
    }
    #[test]
    fn overlaps_restore_only_after_last_lease_and_shutdown_restores_once() {
        let (focus, fake) = fixture(true);
        let first = focus.acquire(true);
        let second = focus.acquire(true);
        assert!(!fake.0.lock().unwrap()[0].playing);
        drop(first);
        assert!(!fake.0.lock().unwrap()[0].playing);
        drop(second);
        assert!(fake.0.lock().unwrap()[0].playing);
        let lease = focus.acquire(true);
        focus.shutdown();
        assert!(fake.0.lock().unwrap()[0].playing);
        fake.0.lock().unwrap()[0].playing = false;
        drop(lease);
        assert!(!fake.0.lock().unwrap()[0].playing);
        assert!(focus.acquire(true).is_none());
    }
    #[test]
    fn manual_resume_then_pause_and_track_changes_revoke_ownership() {
        let (focus, fake) = fixture(true);
        let lease = focus.acquire(true);
        fake.0.lock().unwrap()[0].playing = true;
        focus.observe();
        fake.0.lock().unwrap()[0].playing = false;
        drop(lease);
        assert!(!fake.0.lock().unwrap()[0].playing);
        fake.0.lock().unwrap()[0].playing = true;
        let lease = focus.acquire(true);
        fake.0.lock().unwrap()[0].track = "two".into();
        drop(lease);
        assert!(!fake.0.lock().unwrap()[0].playing);
    }
    #[test]
    fn missing_player_and_replaced_player_are_never_resumed() {
        let (focus, fake) = fixture(true);
        let lease = focus.acquire(true);
        fake.0.lock().unwrap().clear();
        focus.observe();
        fake.0.lock().unwrap().push(Snapshot {
            player: "video".into(),
            track: "one".into(),
            playing: false,
        });
        drop(lease);
        assert!(!fake.0.lock().unwrap()[0].playing);
    }

    #[test]
    fn another_player_started_before_release_prevents_automatic_resume() {
        let (focus, fake) = fixture(true);
        let lease = focus.acquire(true);
        fake.0.lock().unwrap().push(Snapshot {
            player: "video".into(),
            track: "two".into(),
            playing: true,
        });
        drop(lease);
        assert!(!fake.0.lock().unwrap()[0].playing);
    }

    struct Unavailable;
    impl MediaPlatform for Unavailable {
        fn snapshots(&self) -> Result<Vec<Snapshot>, ()> {
            Err(())
        }
        fn set_playing(&self, _: &Snapshot, _: bool) -> bool {
            panic!("unreadable media must never be commanded")
        }
    }
    #[test]
    fn unavailable_platform_is_a_noop_and_still_releases_sessions() {
        let focus = Arc::new(MediaFocus {
            state: Mutex::new(Interruption::default()),
            platform: Box::new(Unavailable),
        });
        drop(focus.acquire(true));
        assert!(focus.state.lock().unwrap().leases.is_empty());
        focus.shutdown();
    }

    struct QuietCounter(Arc<std::sync::atomic::AtomicUsize>);
    impl QuietAudio for QuietCounter {
        fn refresh(&mut self) {}
    }
    impl Drop for QuietCounter {
        fn drop(&mut self) {
            self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
    struct QuietOnly(Arc<std::sync::atomic::AtomicUsize>);
    impl MediaPlatform for QuietOnly {
        fn snapshots(&self) -> Result<Vec<Snapshot>, ()> {
            Err(())
        }
        fn set_playing(&self, _: &Snapshot, _: bool) -> bool {
            false
        }
        fn quiet(&self) -> Option<Box<dyn QuietAudio>> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(Box::new(QuietCounter(self.0.clone())))
        }
    }
    #[test]
    fn quiet_fallback_survives_unavailable_media_controls_and_restores_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = Arc::new(AtomicUsize::new(0));
        let focus = Arc::new(MediaFocus {
            state: Mutex::new(Interruption::default()),
            platform: Box::new(QuietOnly(count.clone())),
        });
        let first = focus.acquire(true);
        let second = focus.acquire(true);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        drop(first);
        assert_eq!(count.load(Ordering::SeqCst), 1);
        focus.shutdown();
        assert_eq!(count.load(Ordering::SeqCst), 0);
        drop(second);
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "temporarily quiets external audio; explicit local verification only"]
    fn native_quiet_lifecycle() {
        let mut quiet = platform::Native
            .quiet()
            .expect("Core Audio quieting unavailable");
        quiet.refresh();
        std::thread::sleep(Duration::from_millis(500));
        drop(quiet);
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "requires deliberately playing media; never run in ordinary CI"]
    fn native_media_pause_and_resume() {
        let focus = MediaFocus::new();
        let before = focus
            .platform
            .snapshots()
            .expect("native media query failed");
        assert!(
            before.iter().any(|s| s.playing),
            "start media before running the live test"
        );
        let lease = focus.acquire(true);
        assert_eq!(
            focus.state.lock().unwrap().paused.len(),
            1,
            "player did not accept pause"
        );
        assert!(focus
            .platform
            .snapshots()
            .unwrap()
            .iter()
            .all(|s| !s.playing));
        std::thread::sleep(Duration::from_secs(1));
        drop(lease);
        assert_eq!(
            focus.platform.snapshots().unwrap(),
            before,
            "media did not resume"
        );
        focus.shutdown();
    }
}
