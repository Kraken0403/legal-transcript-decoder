use serde::Serialize;

use crate::document::{PageSection, TextSource};

use super::models::{DocumentType, Participant, VerificationStatus};

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FormatGrammarKind {
    SpeakerPrefixed,
    QaMarkers,
    Hybrid,
    Procedural,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RowEventKind {
    SpeakerAnchor,
    QuestionMarker,
    AnswerMarker,
    ExaminationHeading,
    Parenthetical,
    Procedural,
    PageHeader,
    Redaction,
    Text,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiscourseRole {
    Question,
    Response,
    Statement,
    Objection,
    Interjection,
    Clarification,
    Interpretation,
    Procedural,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Person,
    Organization,
    Location,
    Exhibit,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConversationRelation {
    Follows,
    RespondsTo,
    Interrupts,
    Resumes,
    Clarifies,
    ObjectsTo,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptIrDocument {
    pub document_type: DocumentType,
    pub physical_page_count: usize,
    pub transcript_page_count: usize,
    pub positioned_text_pages: usize,
    pub ocr_required_pages: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FormatRegion {
    pub id: String,
    pub grammar: FormatGrammarKind,
    pub physical_page_start: u32,
    pub physical_page_end: u32,
    pub speaker_anchor_observations: usize,
    pub question_marker_observations: usize,
    pub answer_marker_observations: usize,
    pub examination_heading_observations: usize,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct CanonicalRow {
    pub disposition: RowDisposition,
    pub section: PageSection,
    pub panel: u16,
    pub bbox: Option<[f64; 4]>,
    pub ocr_confidence: Option<f32>,
    pub timestamp: Option<String>,
    pub id: String,
    pub physical_page: u32,
    pub transcript_page: Option<String>,
    pub row_index: usize,
    pub line_number: Option<u16>,
    pub text: String,
    pub raw_text: String,
    pub event_kind: RowEventKind,
    pub speaker_anchor_label: Option<String>,
    pub speaker_participant_id: Option<String>,
    pub format_region_id: Option<String>,
    pub source: TextSource,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageContinuity {
    pub utterance_id: String,
    pub participant_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageFrame {
    pub physical_page: u32,
    pub transcript_page: Option<String>,
    pub section: PageSection,
    pub row_ids: Vec<String>,
    pub format_region_ids: Vec<String>,
    pub continuity_in: Option<PageContinuity>,
    pub continuity_out: Option<PageContinuity>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceSpan {
    pub physical_page_start: u32,
    pub physical_page_end: u32,
    pub transcript_page_start: Option<String>,
    pub transcript_page_end: Option<String>,
    pub line_start: Option<u16>,
    pub line_end: Option<u16>,
    pub row_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Entity {
    pub id: String,
    pub canonical_label: String,
    pub kind: EntityKind,
    pub participant_id: Option<String>,
    pub observed_mentions: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntityMention {
    pub id: String,
    pub entity_id: String,
    pub utterance_id: String,
    pub text: String,
    pub char_start: usize,
    pub char_end: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct UtteranceConfidence {
    pub speaker: f32,
    pub boundary: f32,
    pub discourse_role: f32,
    pub source: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Utterance {
    pub id: String,
    pub participant_id: Option<String>,
    pub speaker_label: Option<String>,
    pub discourse_role: DiscourseRole,
    pub text: String,
    pub source_spans: Vec<SourceSpan>,
    pub confidence: UtteranceConfidence,
    pub verification: VerificationStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationEdge {
    pub from_utterance_id: String,
    pub to_utterance_id: String,
    pub relation: ConversationRelation,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct OmniDiagnostics {
    pub canonical_rows: usize,
    pub transcript_rows: usize,
    pub rows_assigned_to_utterances: usize,
    pub unresolved_speaker_rows: usize,
    pub format_regions: usize,
    pub cross_page_utterances: usize,
    pub utterances: usize,
    pub entity_mentions: usize,
    pub source_row_coverage: f32,
    pub conversation_confidence: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptIr {
    pub document: TranscriptIrDocument,
    pub format_regions: Vec<FormatRegion>,
    pub participants: Vec<Participant>,
    pub entities: Vec<Entity>,
    pub entity_mentions: Vec<EntityMention>,
    pub page_frames: Vec<PageFrame>,
    pub canonical_rows: Vec<CanonicalRow>,
    pub utterances: Vec<Utterance>,
    pub conversation_edges: Vec<ConversationEdge>,
    pub diagnostics: OmniDiagnostics,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RowDisposition {
    Dialogue,
    FrontMatter,
    Reference,
    PageFurniture,
    BlankNumberedLine,
    Unclassified,
}
