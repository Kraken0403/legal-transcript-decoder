//! Structural checks are gates, not claims of legal/factual correctness.
use super::{
    models::{
        LineKind, TranscriptBlock, TranscriptContext, TranscriptLine, VerificationIssue,
        VerificationStatus,
    },
    omni_models::{RowDisposition, TranscriptIr},
};
use crate::document::ExtractedDocument;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Serialize)]
pub struct QualityIssue {
    pub code: String,
    pub physical_page: Option<u32>,
    pub row_id: Option<String>,
    pub message: String,
}
#[derive(Debug, Serialize)]
pub struct QualityReport {
    pub status: String,
    pub ready_for_ai: bool,
    pub source_rows: usize,
    pub accounted_source_rows: usize,
    pub dialogue_rows: usize,
    pub assigned_dialogue_rows: usize,
    pub unknown_speaker_rows: usize,
    pub confidence_ceiling: f32,
    pub issues: Vec<QualityIssue>,
    pub meaning: String,
}

pub fn assess(
    document: &ExtractedDocument,
    context: &TranscriptContext,
    ir: &TranscriptIr,
    lines: &[TranscriptLine],
    blocks: &[TranscriptBlock],
    verification: &[VerificationIssue],
) -> QualityReport {
    let mut issues = Vec::new();
    let mut add = |code: &str, page: Option<u32>, row: Option<String>, message: String| {
        issues.push(QualityIssue {
            code: code.to_string(),
            physical_page: page,
            row_id: row,
            message,
        })
    };
    let expected: usize = document
        .pages
        .iter()
        .map(|p| {
            if p.layout.used_positioned_reconstruction {
                p.visual_rows.len()
            } else {
                p.text.lines().filter(|l| !l.trim().is_empty()).count()
            }
        })
        .sum();
    let ids: BTreeSet<_> = ir.canonical_rows.iter().map(|r| r.id.clone()).collect();
    if ids.len() != expected || ids.len() != ir.canonical_rows.len() {
        add(
            "source_inventory_mismatch",
            None,
            None,
            "Extracted source rows and canonical inventory disagree.".to_string(),
        );
    }
    let mut ownership: BTreeMap<&str, usize> = BTreeMap::new();
    for block in blocks {
        for id in &block.source_row_ids {
            *ownership.entry(id).or_default() += 1;
        }
    }
    for row in &ir.canonical_rows {
        if row.disposition == RowDisposition::Dialogue && ownership.get(row.id.as_str()) != Some(&1)
        {
            add(
                "dialogue_row_ownership",
                Some(row.physical_page),
                Some(row.id.clone()),
                "Dialogue row must belong to exactly one block.".to_string(),
            );
        }
        if row.disposition == RowDisposition::Unclassified {
            add(
                "unclassified_source_row",
                Some(row.physical_page),
                Some(row.id.clone()),
                "Source preserved, but section/dialogue status needs review.".to_string(),
            );
        }
        if row.ocr_confidence.is_some_and(|c| c < 0.85) {
            add(
                "low_ocr_confidence",
                Some(row.physical_page),
                Some(row.id.clone()),
                "Low OCR score: compare the text to the page image.".to_string(),
            );
        }
    }
    for page in &document.pages {
        if page.layout.requires_ocr {
            add(
                "ocr_required",
                Some(page.physical_page),
                None,
                "Native extraction is incomplete and OCR did not produce a usable result."
                    .to_string(),
            );
        }
        if page.layout.ocr_performed {
            add(
                "ocr_review_required",
                Some(page.physical_page),
                None,
                "OCR text must be reviewed before use in analysis.".to_string(),
            );
        }
        if page.text.trim().is_empty() {
            add(
                "empty_page",
                Some(page.physical_page),
                None,
                "No text recovered; confirm whether the physical page is blank.".to_string(),
            );
        }
        if page.layout.extraction_engine == "lopdf_fallback" {
            add(
                "fallback_extraction",
                Some(page.physical_page),
                None,
                "Estimated fallback geometry is not sufficient to certify extraction completeness."
                    .to_string(),
            );
        }
        if page.layout.reading_order_uncertain || page.layout.column_count > 1 {
            add(
                "reading_order_review",
                Some(page.physical_page),
                None,
                "Verify pane/rotation reading order and printed page references.".to_string(),
            );
        }
        for warning in &page.layout.warnings {
            add(
                "extraction_warning",
                Some(page.physical_page),
                None,
                warning.clone(),
            );
        }
    }
    let participant_ids: BTreeSet<_> = context.participants.iter().map(|p| p.id.as_str()).collect();
    let mut unknown = 0;
    for line in lines {
        let procedural = matches!(
            line.kind,
            LineKind::ExaminationHeading
                | LineKind::Heading
                | LineKind::Parenthetical
                | LineKind::ExhibitMarker
                | LineKind::RedactionMarker
                | LineKind::PageHeader
        );
        if !procedural && (line.participant_id.is_none() || line.kind == LineKind::Unknown) {
            unknown += 1;
            add(
                "unresolved_attribution",
                Some(line.physical_page),
                Some(line.source_row_id.clone()),
                "Text retained; speaker or discourse classification is unresolved.".to_string(),
            );
        }
        if line
            .participant_id
            .as_deref()
            .is_some_and(|id| !participant_ids.contains(id))
        {
            add(
                "participant_reference",
                Some(line.physical_page),
                Some(line.source_row_id.clone()),
                "Participant ID is absent from the registry.".to_string(),
            );
        }
        if line.speaker.as_ref().is_some_and(|s| s.redacted) {
            add(
                "redacted_identity",
                Some(line.physical_page),
                Some(line.source_row_id.clone()),
                "Anonymous ID identifies an observed turn/examination, not a verified person."
                    .to_string(),
            );
        }
        if line.verification == VerificationStatus::Conflict {
            add(
                "classification_conflict",
                Some(line.physical_page),
                Some(line.source_row_id.clone()),
                "Source and classification disagree.".to_string(),
            );
        }
    }
    for gap in &context.gaps {
        add(
            "source_gap_candidate",
            gap.physical_page,
            None,
            format!(
                "{:?}: unobserved line/page range; verify against source before asserting missing testimony.",
                gap.kind
            ),
        );
    }
    for issue in verification {
        add(
            &issue.code,
            Some(issue.physical_page),
            None,
            issue.message.clone(),
        );
    }
    if context.language.code != "en" {
        add(
            "language_review",
            None,
            None,
            "The current structural grammar targets English labels; review other languages."
                .to_string(),
        );
    }
    if ir.diagnostics.transcript_rows == 0 {
        add(
            "no_dialogue",
            None,
            None,
            "No reliable dialogue was identified; source pages are retained.".to_string(),
        );
    }
    let blocked = ir.diagnostics.transcript_rows == 0
        || issues.iter().any(|i| {
            matches!(
                i.code.as_str(),
                "source_inventory_mismatch"
                    | "dialogue_row_ownership"
                    | "ocr_required"
                    | "participant_reference"
            )
        });
    let ready = !blocked && issues.is_empty();
    QualityReport{status:if blocked{"blocked"}else if ready{"passed_structural_checks"}else{"needs_review"}.to_string(),ready_for_ai:ready,source_rows:expected,accounted_source_rows:ids.len(),dialogue_rows:ir.diagnostics.transcript_rows,assigned_dialogue_rows:ir.diagnostics.rows_assigned_to_utterances,unknown_speaker_rows:unknown,
        confidence_ceiling:if blocked{0.35}else if ready{0.95}else{0.75},issues,
        meaning:"Readiness checks extraction and structural consistency only. It is not a probability of accuracy, a factual verification, or a legal conclusion. Unseen text and redacted identities are never reconstructed.".to_string()}
}
