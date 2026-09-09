use thiserror::Error;

#[derive(Debug, Error)]
pub enum TranscriptParseError {
    #[error("No transcript lines could be detected.")]
    NoTranscriptLines,
}
