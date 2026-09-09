use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    Text,
    Pdf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextSource {
    NativePdf,
    PlainText,
    Ocr,
    FallbackText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageSection {
    Cover,
    Caption,
    Appearances,
    Transcript,
    WordIndex,
    ExhibitIndex,
    Certificate,
    Errata,
    Appendix,
    Attachment,
    Unknown,
}

impl PageSection {
    pub fn is_dialogue(&self) -> bool {
        matches!(self, Self::Transcript)
    }

    pub fn is_transcript_family(&self) -> bool {
        matches!(
            self,
            Self::Cover | Self::Caption | Self::Appearances | Self::Transcript
        )
    }

    pub fn is_reference_only(&self) -> bool {
        matches!(
            self,
            Self::WordIndex
                | Self::ExhibitIndex
                | Self::Certificate
                | Self::Errata
                | Self::Appendix
                | Self::Attachment
        )
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocumentMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    pub creation_date: Option<String>,
    pub modification_date: Option<String>,
    pub pdf_version: Option<String>,
    pub encrypted: bool,
    pub declared_page_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationSignal {
    pub physical_page: u32,
    pub subtype: String,
    pub rect: Option<[f64; 4]>,
    pub color_components: Vec<f64>,
    pub is_explicit_redaction: bool,
    pub is_dark_overlay_candidate: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilledRectangleSignal {
    pub physical_page: u32,
    pub rect: [f64; 4],
    pub color_space: String,
    pub color_components: Vec<f64>,
    pub is_dark: bool,
    pub possible_redaction: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionedTextFragment {
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub font_name: Option<String>,
    pub font_size: Option<f64>,
    pub source: TextSource,
    pub sequence: usize,
    pub geometry_estimated: bool,
    /// True when the visible PDF layer covers this fragment with a strong redaction signal.
    pub redaction_masked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisualRow {
    #[serde(default)]
    pub panel: u16,
    #[serde(default)]
    pub printed_page: Option<String>,
    #[serde(default)]
    pub bbox: Option<[f64; 4]>,
    #[serde(default)]
    pub ocr_confidence: Option<f32>,
    pub y: f64,
    pub left_x: Option<f64>,
    pub line_number_x: Option<f64>,
    pub line_number: Option<u16>,
    pub text: String,
    pub fragments: Vec<PositionedTextFragment>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PageLayoutDiagnostics {
    pub extraction_engine: String,
    pub warnings: Vec<String>,
    pub ocr_performed: bool,
    pub ocr_mean_confidence: Option<f32>,
    pub column_count: usize,
    pub reading_order_uncertain: bool,
    pub positioned_text_available: bool,
    pub used_positioned_reconstruction: bool,
    pub fragment_count: usize,
    pub visual_row_count: usize,
    pub numbered_row_count: usize,
    pub masked_redaction_fragments: usize,
    pub likely_line_number_column_x: Option<f64>,
    pub reconstruction_confidence: f32,
    pub fallback_reason: Option<String>,
    pub requires_ocr: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedPage {
    pub physical_page: u32,
    /// Canonical page text consumed by transcript preflight/parser.
    /// For PDFs this prefers position-aware visual-row reconstruction.
    pub text: String,
    /// Plain text extraction retained for diagnostics/fallback comparison.
    pub raw_text: String,
    pub section: PageSection,
    pub section_confidence: f32,
    pub section_reasons: Vec<String>,
    pub visual_rows: Vec<VisualRow>,
    pub layout: PageLayoutDiagnostics,
    pub annotations: Vec<AnnotationSignal>,
    pub filled_rectangles: Vec<FilledRectangleSignal>,
    pub image_count: usize,
}

impl ExtractedPage {
    pub fn should_feed_dialogue_parser(&self) -> bool {
        self.section.is_dialogue()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedDocument {
    pub kind: DocumentKind,
    pub metadata: DocumentMetadata,
    pub pages: Vec<ExtractedPage>,
}

impl ExtractedDocument {
    pub fn source_page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn has_positioned_text(&self) -> bool {
        self.pages
            .iter()
            .any(|page| page.layout.positioned_text_available)
    }

    pub fn requires_ocr(&self) -> bool {
        self.pages.iter().any(|page| page.layout.requires_ocr)
    }

    pub fn transcript_pages(&self) -> impl Iterator<Item = &ExtractedPage> {
        self.pages.iter().filter(|page| page.section.is_dialogue())
    }
}
