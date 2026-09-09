use serde::Serialize;

use crate::document::{DocumentMetadata, PageSection};

use super::omni_models::TranscriptIr;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerRole {
    Witness,
    Attorney,
    Interpreter,
    CourtReporter,
    Judge,
    Videographer,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantFunction {
    Witness,
    Interviewee,
    Interviewer,
    Counsel,
    GovernmentCounsel,
    LawEnforcement,
    Interpreter,
    CourtReporter,
    Judge,
    Videographer,
    Other,
    Unknown,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Speaker {
    pub participant_id: Option<String>,
    pub label: String,
    pub role: SpeakerRole,
    pub redacted: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Question,
    Answer,
    Objection,
    AttorneyStatement,
    SpeakerStatement,
    WitnessStatement,
    InterpreterStatement,
    ReporterStatement,
    JudgeStatement,
    VideographerStatement,
    ExhibitMarker,
    Parenthetical,
    ExaminationHeading,
    Heading,
    RedactionMarker,
    PageHeader,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationConfidence {
    Explicit,
    Inferred,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Verified,
    VerifiedWithInference,
    Uncertain,
    SourceMissing,
    Redacted,
    Conflict,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceContinuity {
    FirstObserved,
    Continuous,
    CrossPageContinuous,
    GapBefore,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DocumentType {
    Deposition,
    Hearing,
    TrialTranscript,
    Arbitration,
    Interview,
    Examination,
    OtherTranscript,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MarkerKind {
    Question,
    Answer,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MarkerStyle {
    Dot,
    BareUppercase,
    Colon,
    Word,
    SpeakerPrefixed,
}

#[derive(Debug, Clone, Serialize)]
pub struct MarkerRule {
    pub kind: MarkerKind,
    pub style: MarkerStyle,
    pub pattern: String,
    pub priority: u8,
    pub observations: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Participant {
    pub id: String,
    pub display_label: String,
    pub canonical_name: Option<String>,
    pub observed_labels: Vec<String>,
    pub role: SpeakerRole,
    pub role_confidence: f32,
    pub function: ParticipantFunction,
    pub function_confidence: f32,
    pub redacted: bool,
    pub observations: usize,
    pub question_observations: usize,
    pub answer_observations: usize,
    pub objection_observations: usize,
    pub statement_observations: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct LanguageHint {
    pub code: String,
    pub name: String,
    pub confidence: f32,
    pub dominant_script: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageContext {
    pub physical_page: u32,
    pub transcript_page: Option<String>,
    pub section: PageSection,
    pub section_confidence: f32,
    pub layout_reconstruction_confidence: f32,
    pub requires_ocr: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GapKind {
    MissingLines,
    MissingTranscriptPages,
    PartialPageStart,
    PartialPageEnd,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GapReason {
    MissingFromSource,
    UnobservedInExtractedText,
    RedactionPossible,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptGap {
    pub kind: GapKind,
    pub physical_page: Option<u32>,
    pub transcript_page: Option<String>,
    pub missing_line_start: Option<u16>,
    pub missing_line_end: Option<u16>,
    pub missing_transcript_page_start: Option<u32>,
    pub missing_transcript_page_end: Option<u32>,
    pub reason: GapReason,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RedactionSignalKind {
    PdfRedactAnnotation,
    DarkAnnotationCandidate,
    DarkFilledRectangleCandidate,
    TextualRedactionMarker,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SignalConfidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize)]
pub struct RedactionCandidate {
    pub physical_page: u32,
    pub rect: Option<[f64; 4]>,
    pub kind: RedactionSignalKind,
    pub confidence: SignalConfidence,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RedactionSummary {
    pub explicit_redaction_annotations: usize,
    pub dark_annotation_candidates: usize,
    pub dark_filled_rectangle_candidates: usize,
    pub textual_redaction_markers: usize,
    pub pages_with_images: usize,
    pub requires_visual_confirmation: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct FormatFingerprint {
    pub question_markers: Vec<MarkerRule>,
    pub answer_markers: Vec<MarkerRule>,
    pub dominant_question_marker: Option<String>,
    pub dominant_answer_marker: Option<String>,
    pub speaker_prefixed_dialogue: bool,
    pub colon_speaker_prefixed_dialogue: bool,
    pub titled_speaker_prefixed_dialogue: bool,
    pub examination_heading_style_detected: bool,
    pub redacted_speaker_labels_detected: bool,
    pub typical_line_min: Option<u16>,
    pub typical_line_max: Option<u16>,
    pub transcript_page_labels_detected: usize,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    ExplicitMarker,
    SpeakerLabel,
    CurrentExaminer,
    KnownWitness,
    PreviousLine,
    NextLine,
    QuestionPunctuation,
    FormatFingerprint,
    RoleBehavior,
    SourceGap,
    Redaction,
    StructuralRecovery,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidenceItem {
    pub source: EvidenceSource,
    pub weight: f32,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ClassificationEvidence {
    pub question_score: f32,
    pub answer_score: f32,
    pub statement_score: f32,
    pub objection_score: f32,
    pub items: Vec<EvidenceItem>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct DocumentLayoutSummary {
    pub positioned_text_pages: usize,
    pub positioned_reconstructed_pages: usize,
    pub transcript_pages: usize,
    pub cover_pages: usize,
    pub caption_pages: usize,
    pub appearances_pages: usize,
    pub word_index_pages: usize,
    pub exhibit_index_pages: usize,
    pub certificate_pages: usize,
    pub errata_pages: usize,
    pub appendix_pages: usize,
    pub attachment_pages: usize,
    pub unknown_pages: usize,
    pub reference_only_pages: usize,
    pub ocr_required_pages: usize,
    pub masked_redaction_fragments: usize,
    pub likely_line_number_column_x: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptContext {
    pub document_type: DocumentType,
    pub profile_name: String,
    pub metadata: DocumentMetadata,
    pub language: LanguageHint,
    pub participants: Vec<Participant>,
    pub primary_questioner: Option<Speaker>,
    pub primary_witness: Option<Speaker>,
    pub marker_rules: Vec<MarkerRule>,
    pub format_fingerprint: FormatFingerprint,
    pub layout_summary: DocumentLayoutSummary,
    pub typical_line_min: Option<u16>,
    pub typical_line_max: Option<u16>,
    pub page_contexts: Vec<PageContext>,
    pub gaps: Vec<TranscriptGap>,
    pub redaction_summary: RedactionSummary,
    pub redaction_candidates: Vec<RedactionCandidate>,
    pub ai_router_recommended: bool,
    pub format_confidence: f32,
    pub source_completeness: f32,
}

impl TranscriptContext {
    pub fn transcript_page_for(&self, physical_page: u32) -> Option<String> {
        self.page_contexts
            .iter()
            .find(|page| page.physical_page == physical_page)
            .and_then(|page| page.transcript_page.clone())
    }

    pub fn participant_by_id(&self, id: &str) -> Option<&Participant> {
        self.participants
            .iter()
            .find(|participant| participant.id == id)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptLocation {
    pub physical_page: u32,
    pub transcript_page: Option<String>,
    pub line_number: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptLine {
    pub source_row_id: String,
    pub physical_page: u32,
    pub transcript_page: Option<String>,
    pub line_number: Option<u16>,
    pub kind: LineKind,
    pub speaker: Option<Speaker>,
    pub participant_id: Option<String>,
    pub confidence: ClassificationConfidence,
    pub verification: VerificationStatus,
    pub source_continuity: SourceContinuity,
    pub evidence: ClassificationEvidence,
    pub starts_new_block: bool,
    pub text: String,
    pub raw_text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptBlock {
    pub source_row_ids: Vec<String>,
    pub id: String,
    pub kind: LineKind,
    pub speaker: Option<Speaker>,
    pub participant_id: Option<String>,
    pub confidence: ClassificationConfidence,
    pub verification: VerificationStatus,
    pub source_continuity: SourceContinuity,
    pub start: TranscriptLocation,
    pub end: TranscriptLocation,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestimonyExchange {
    pub question: TranscriptBlock,
    pub response_sequence: Vec<TranscriptBlock>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GraphEdgeKind {
    RespondsTo,
    Interrupts,
    ResumesAfter,
    Follows,
    Interprets,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptGraphNode {
    pub id: String,
    pub block_id: String,
    pub kind: LineKind,
    pub participant_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptGraphEdge {
    pub from: String,
    pub to: String,
    pub kind: GraphEdgeKind,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct TranscriptGraph {
    pub nodes: Vec<TranscriptGraphNode>,
    pub edges: Vec<TranscriptGraphEdge>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerificationIssue {
    pub physical_page: u32,
    pub transcript_page: Option<String>,
    pub line_number: Option<u16>,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct ParseDiagnostics {
    pub inferred_lines: usize,
    pub unknown_lines: usize,
    pub verified_lines: usize,
    pub verified_with_inference_lines: usize,
    pub conflict_lines: usize,
    pub uncertain_lines: usize,
    pub detected_transcript_pages: usize,
    pub physical_pages_without_transcript_page: usize,
    pub unanswered_questions: usize,
    pub unpaired_answers: usize,
    pub detected_gaps: usize,
    pub anonymous_participants: usize,
    pub explicit_redaction_annotations: usize,
    pub dark_redaction_candidates: usize,
    pub textual_redaction_markers: usize,
    pub parse_confidence: f32,
}

#[derive(Debug, Serialize)]
pub struct ParsedTranscript {
    pub schema_version: String,
    pub source_sha256: Option<String>,
    pub quality: super::quality::QualityReport,
    pub source_pages: Vec<crate::document::ExtractedPage>,
    pub filename: String,
    pub profile: String,
    pub physical_page_count: usize,
    pub transcript_page_count: usize,
    pub line_count: usize,
    pub context: TranscriptContext,
    pub lines: Vec<TranscriptLine>,
    pub blocks: Vec<TranscriptBlock>,
    pub exchanges: Vec<TestimonyExchange>,
    pub graph: TranscriptGraph,
    pub omni: TranscriptIr,
    pub unpaired_answers: Vec<TranscriptBlock>,
    pub verification_issues: Vec<VerificationIssue>,
    pub diagnostics: ParseDiagnostics,
}
