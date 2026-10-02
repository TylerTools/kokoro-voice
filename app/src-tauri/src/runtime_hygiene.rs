//! Private runtime-file classification, cleanup, and accounting.
//!
//! This module owns only disposable files created by HereWord clients. It must
//! never treat exported audio, preferences, tokens, logs, or unknown files as
//! removable runtime state.

use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, serde::Serialize, PartialEq, Eq)]
pub(crate) struct CleanupReport {
    pub(crate) files_removed: u64,
    pub(crate) bytes_removed: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManagedFile {
    TtsChunk { pid: u32 },
    DictationControl,
}

fn managed_file(name: &str) -> Option<ManagedFile> {
    for suffix in [".wav.part", ".wav"] {
        if let Some(stem) = name
            .strip_prefix("kokoro-")
            .and_then(|v| v.strip_suffix(suffix))
        {
            let mut pieces = stem.split('-');
            let pid = pieces.next()?.parse().ok()?;
            let index = pieces.next()?;
            if pieces.next().is_none()
                && !index.is_empty()
                && index.bytes().all(|v| v.is_ascii_digit())
            {
                return Some(ManagedFile::TtsChunk { pid });
            }
        }
    }
    for suffix in [".stop", ".cancel"] {
        if let Some(session) = name
            .strip_prefix("dictate-")
            .and_then(|value| value.strip_suffix(suffix))
        {
            if !session.is_empty()
                && session.len() <= 80
                && session
                    .bytes()
                    .all(|value| value.is_ascii_alphanumeric() || value == b'-' || value == b'_')
            {
                return Some(ManagedFile::DictationControl);
            }
        }
    }
    None
}

fn old_enough(metadata: &std::fs::Metadata, minimum_age: Duration) -> bool {
    minimum_age.is_zero()
        || metadata
            .modified()
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= minimum_age)
}

pub(crate) fn cleanup_managed_runtime<F>(
    directory: &std::path::Path,
    minimum_age: Duration,
    mut speaker_is_live: F,
) -> CleanupReport
where
    F: FnMut(u32) -> bool,
{
    let mut report = CleanupReport::default();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return report;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || !old_enough(&metadata, minimum_age)
        {
            continue;
        }
        let Some(kind) = entry.file_name().to_str().and_then(managed_file) else {
            continue;
        };
        if matches!(kind, ManagedFile::TtsChunk { pid } if speaker_is_live(pid)) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            report.files_removed += 1;
            report.bytes_removed += metadata.len();
        }
    }
    report
}

pub(crate) fn managed_runtime_bytes(directory: &std::path::Path) -> u64 {
    std::fs::read_dir(directory)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let metadata = std::fs::symlink_metadata(entry.path()).ok()?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return None;
            }
            entry
                .file_name()
                .to_str()
                .and_then(managed_file)
                .map(|_| metadata.len())
        })
        .sum()
}

pub(crate) fn speaker_is_live(pid: u32) -> bool {
    #[cfg(not(target_os = "windows"))]
    let output = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output();
    #[cfg(target_os = "windows")]
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-Command",
            &format!("(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').CommandLine"),
        ])
        .output();
    output.ok().is_some_and(|output| {
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("speak.py")
    })
}

#[cfg(test)]
mod tests {
    use super::{cleanup_managed_runtime, managed_runtime_bytes};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    fn fixture(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "kokoro-hygiene-{label}-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn removes_only_dead_managed_files() {
        let path = fixture("managed");
        for (name, contents) in [
            ("kokoro-41-001.wav", b"dead".as_slice()),
            ("kokoro-42-002.wav.part", b"live".as_slice()),
            ("dictate-session.stop", b"".as_slice()),
            ("dictate-session.cancel", b"".as_slice()),
            ("kokoro-export-41-001.wav", b"export".as_slice()),
            ("recording.wav", b"user".as_slice()),
            ("kokoro-not-a-pid-001.wav", b"unknown".as_slice()),
        ] {
            std::fs::write(path.join(name), contents).unwrap();
        }
        assert_eq!(managed_runtime_bytes(&path), 8);
        let report = cleanup_managed_runtime(&path, Duration::ZERO, |pid| pid == 42);
        assert_eq!(report.files_removed, 3);
        assert_eq!(report.bytes_removed, 4);
        assert!(path.join("kokoro-42-002.wav.part").exists());
        assert!(path.join("kokoro-export-41-001.wav").exists());
        assert!(path.join("recording.wav").exists());
        assert!(path.join("kokoro-not-a-pid-001.wav").exists());
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn age_gate_preserves_recent_files() {
        let path = fixture("age");
        std::fs::write(path.join("kokoro-41-001.wav"), b"recent").unwrap();
        let report = cleanup_managed_runtime(&path, Duration::from_secs(300), |_| false);
        assert_eq!(report.files_removed, 0);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_removed() {
        use std::os::unix::fs::symlink;
        let path = fixture("symlink");
        symlink("outside", path.join("kokoro-41-001.wav")).unwrap();
        let report = cleanup_managed_runtime(&path, Duration::ZERO, |_| false);
        assert_eq!(report.files_removed, 0);
        assert!(std::fs::symlink_metadata(path.join("kokoro-41-001.wav"))
            .unwrap()
            .file_type()
            .is_symlink());
        std::fs::remove_dir_all(path).unwrap();
    }
}
