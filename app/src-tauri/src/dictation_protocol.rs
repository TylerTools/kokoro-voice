//! Typed parser for the line protocol emitted by `client/dictate.py`.
//!
//! This is a process boundary, not ordinary console output. The Python child
//! writes one event per stdout line; stderr is diagnostic-only. Keep parsing in
//! this module so orchestration code never invents a second interpretation of
//! protocol prefixes.

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event<'a> {
    Recording,
    Transcribing(Option<f64>),
    InactivityWarning,
    MaxLength,
    PreviewFull(&'a str),
    PreviewRolling(&'a str),
    Metrics(&'a str),
    FinalText(&'a str),
    RetryingEngine,
    Cancelled,
    Error(&'a str),
    Unknown(&'a str),
}

/// Parse one newline-free stdout record from the dictation child.
pub fn parse(line: &str) -> Event<'_> {
    if line == "RECORDING" {
        Event::Recording
    } else if line == "INACTIVITY_WARNING" {
        Event::InactivityWarning
    } else if line == "MAXLEN" {
        Event::MaxLength
    } else if line == "RETRYING engine" {
        Event::RetryingEngine
    } else if line == "CANCELLED" {
        Event::Cancelled
    } else if let Some(seconds) = line.strip_prefix("TRANSCRIBING ") {
        Event::Transcribing(seconds.parse().ok())
    } else if line == "TRANSCRIBING" {
        Event::Transcribing(None)
    } else if let Some(text) = line.strip_prefix("PREVIEW_FULL ") {
        Event::PreviewFull(text)
    } else if let Some(text) = line.strip_prefix("PREVIEW_ROLLING ") {
        Event::PreviewRolling(text)
    } else if let Some(json) = line.strip_prefix("METRICS ") {
        Event::Metrics(json)
    } else if let Some(text) = line.strip_prefix("TEXT ") {
        Event::FinalText(text)
    } else if let Some(message) = line.strip_prefix("ERROR ") {
        Event::Error(message)
    } else {
        Event::Unknown(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_documented_record_shape() {
        assert_eq!(parse("RECORDING"), Event::Recording);
        assert_eq!(parse("TRANSCRIBING 2.5"), Event::Transcribing(Some(2.5)));
        assert_eq!(parse("TRANSCRIBING"), Event::Transcribing(None));
        assert_eq!(parse("INACTIVITY_WARNING"), Event::InactivityWarning);
        assert_eq!(parse("MAXLEN"), Event::MaxLength);
        assert_eq!(parse("PREVIEW_FULL hello"), Event::PreviewFull("hello"));
        assert_eq!(
            parse("PREVIEW_ROLLING hello"),
            Event::PreviewRolling("hello")
        );
        assert_eq!(parse("METRICS {}"), Event::Metrics("{}"));
        assert_eq!(parse("TEXT final words"), Event::FinalText("final words"));
        assert_eq!(parse("RETRYING engine"), Event::RetryingEngine);
        assert_eq!(parse("CANCELLED"), Event::Cancelled);
        assert_eq!(
            parse("ERROR no audio captured"),
            Event::Error("no audio captured")
        );
        assert_eq!(parse("STATUS ignored"), Event::Unknown("STATUS ignored"));
    }

    #[test]
    fn prefixes_require_the_protocol_separator() {
        assert_eq!(parse("TEXTURE"), Event::Unknown("TEXTURE"));
        assert_eq!(parse("PREVIEW_FULL"), Event::Unknown("PREVIEW_FULL"));
        assert_eq!(parse("ERROR"), Event::Unknown("ERROR"));
    }
}
