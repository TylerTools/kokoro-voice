//! macOS microphone-authorization boundary.
//!
//! This module owns only AVFoundation permission state and prompting. Audio
//! capture remains in the Python client; keeping authorization here prevents a
//! successfully opened-but-silent stream from being mistaken for a healthy mic.

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaType, AVMediaTypeAudio};
use objc2_avf_audio::{AVAudioApplication, AVAudioApplicationRecordPermission};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    NotDetermined,
    Restricted,
    Denied,
    Authorized,
    Unknown,
}

impl Status {
    pub(crate) fn is_authorized(self) -> bool {
        self == Self::Authorized
    }

    pub(crate) fn as_permission_state(self) -> &'static str {
        if self.is_authorized() {
            "available"
        } else {
            "required"
        }
    }

    pub(crate) fn as_code(self) -> &'static str {
        match self {
            Self::NotDetermined => "not-determined",
            Self::Restricted => "restricted",
            Self::Denied => "denied",
            Self::Authorized => "authorized",
            Self::Unknown => "unknown",
        }
    }
}

fn normalize(status: AVAuthorizationStatus) -> Status {
    match status {
        AVAuthorizationStatus::NotDetermined => Status::NotDetermined,
        AVAuthorizationStatus::Restricted => Status::Restricted,
        AVAuthorizationStatus::Denied => Status::Denied,
        AVAuthorizationStatus::Authorized => Status::Authorized,
        _ => Status::Unknown,
    }
}

fn audio_media_type() -> &'static AVMediaType {
    // AVMediaTypeAudio is available on every supported macOS version. Keeping
    // the unwrap at this framework boundary avoids propagating an impossible
    // nullable constant through the rest of the app.
    unsafe { AVMediaTypeAudio.expect("AVMediaTypeAudio is unavailable") }
}

pub(crate) fn status() -> Status {
    // macOS 14+ owns recording consent at the audio-application boundary.
    // Keep AVCaptureDevice only as the fallback for macOS 13.
    if objc2::runtime::AnyClass::get(c"AVAudioApplication").is_some() {
        return normalize_audio(unsafe { AVAudioApplication::sharedInstance().recordPermission() });
    }
    normalize(unsafe { AVCaptureDevice::authorizationStatusForMediaType(audio_media_type()) })
}

fn normalize_audio(permission: AVAudioApplicationRecordPermission) -> Status {
    match permission {
        AVAudioApplicationRecordPermission::Granted => Status::Authorized,
        AVAudioApplicationRecordPermission::Denied => Status::Denied,
        AVAudioApplicationRecordPermission::Undetermined => Status::NotDetermined,
        _ => Status::Unknown,
    }
}

pub(crate) fn request_access(completion: impl Fn(bool) + 'static) {
    let handler = RcBlock::new(move |granted: Bool| completion(granted.as_bool()));
    unsafe {
        if objc2::runtime::AnyClass::get(c"AVAudioApplication").is_some() {
            AVAudioApplication::requestRecordPermissionWithCompletionHandler(&handler);
        } else {
            AVCaptureDevice::requestAccessForMediaType_completionHandler(
                audio_media_type(),
                &handler,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_application_consent_is_fail_closed() {
        assert_eq!(
            normalize_audio(AVAudioApplicationRecordPermission::Granted),
            Status::Authorized
        );
        assert_eq!(
            normalize_audio(AVAudioApplicationRecordPermission::Denied),
            Status::Denied
        );
        assert_eq!(
            normalize_audio(AVAudioApplicationRecordPermission::Undetermined),
            Status::NotDetermined
        );
        assert_eq!(
            normalize_audio(AVAudioApplicationRecordPermission(99)),
            Status::Unknown
        );
    }

    #[test]
    fn authorization_statuses_have_fail_closed_product_states() {
        assert_eq!(
            normalize(AVAuthorizationStatus::Authorized),
            Status::Authorized
        );
        assert_eq!(normalize(AVAuthorizationStatus::Denied), Status::Denied);
        assert_eq!(
            normalize(AVAuthorizationStatus::Restricted),
            Status::Restricted
        );
        assert_eq!(
            normalize(AVAuthorizationStatus::NotDetermined),
            Status::NotDetermined
        );
        assert_eq!(normalize(AVAuthorizationStatus(99)), Status::Unknown);
        assert!(Status::Authorized.is_authorized());
        assert!(!Status::Unknown.is_authorized());
    }
}
