use thiserror::Error;

#[derive(Debug, Error)]
pub enum DocumentError {
    #[error("Unsupported file type. Please upload a .txt or .pdf transcript.")]
    UnsupportedFileType,

    #[error("The TXT file is not valid UTF-8 text.")]
    InvalidUtf8,

    #[error("The uploaded file has a .pdf extension but does not appear to be a valid PDF.")]
    InvalidPdfHeader,

    #[error("Could not load PDF: {0}")]
    PdfLoad(String),

    #[error("Could not extract text from PDF page {page}: {message}")]
    PdfTextExtraction { page: u32, message: String },

    #[error(
        "The PDF contains no extractable text. It may be a scanned/image-only transcript and will require OCR."
    )]
    NoExtractableText,
}
