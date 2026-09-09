//! Canonical source inventory. Every nonempty extracted row gets exactly one stable ID.
use super::{
    format::{explicit_page_marker, parse_source_line, pure_numeric_page},
    markers::detect_marker_candidate,
    models::TranscriptContext,
    omni_models::{CanonicalRow, RowDisposition, RowEventKind},
    profile::TranscriptProfile,
    speaker::{split_colon_label, split_titled_speaker_prefix},
};
use crate::document::{ExtractedDocument, PageSection, TextSource};
use std::collections::{BTreeMap, BTreeSet};

pub fn collect_rows(
    document: &ExtractedDocument,
    context: &TranscriptContext,
    profile: &TranscriptProfile,
) -> Vec<CanonicalRow> {
    let mut rows = Vec::new();
    let mut conversation_started = false;
    let mut margin_counts: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    for page in &document.pages {
        if !page.section.is_dialogue() {
            conversation_started = false;
        }
        let from_visual = page.layout.used_positioned_reconstruction;
        let raw_rows: Vec<_> = if from_visual {
            page.visual_rows.iter().map(|r| r.text.as_str()).collect()
        } else {
            page.text.lines().filter(|r| !r.trim().is_empty()).collect()
        };
        for (index, raw) in raw_rows.iter().enumerate() {
            let visual = if from_visual {
                page.visual_rows.get(index)
            } else {
                None
            };
            let (number, body) = parse_source_line(page, raw, profile.max_line_number);
            let printed = visual
                .and_then(|r| r.printed_page.clone())
                .or_else(|| context.transcript_page_for(page.physical_page));
            let header = number.is_none()
                && printed.as_deref().is_some_and(|label| {
                    pure_numeric_page(body)
                        .or_else(|| explicit_page_marker(body))
                        .map(|n| n.to_string())
                        .as_deref()
                        == Some(label)
                });
            let legacy_header = number.is_some()
                && raw
                    .trim()
                    .split_whitespace()
                    .nth(1)
                    .is_some_and(|v| v.len() >= 3 && v.starts_with('0'))
                && index < 3
                && printed.as_deref().is_some_and(|label| {
                    pure_numeric_page(body).map(|n| n.to_string()).as_deref() == Some(label)
                });
            let anchor = split_colon_label(body).is_some()
                || split_titled_speaker_prefix(body).is_some()
                || detect_marker_candidate(body).is_some()
                || body.trim().to_ascii_uppercase().starts_with("BY ");
            let mut disposition = if header || legacy_header {
                RowDisposition::PageFurniture
            } else if body.trim().is_empty() {
                RowDisposition::BlankNumberedLine
            } else if page.section.is_reference_only() {
                RowDisposition::Reference
            } else if matches!(
                page.section,
                PageSection::Cover | PageSection::Caption | PageSection::Appearances
            ) {
                RowDisposition::FrontMatter
            } else if page.section.is_dialogue() {
                RowDisposition::Dialogue
            } else {
                RowDisposition::Unclassified
            };
            if page.section.is_dialogue() && !header && !legacy_header && !body.is_empty() {
                if anchor {
                    conversation_started = true;
                }
                // Mixed appearances + opening testimony: preserve the preamble as front matter.
                if !conversation_started
                    && page.text.lines().any(|line| {
                        let (_, body) =
                            parse_source_line(page, line.trim(), profile.max_line_number);
                        let u = body.to_ascii_uppercase();
                        u.starts_with("APPEARANCES")
                            || (u.starts_with("FOR ") && u.trim_end().ends_with(':'))
                    })
                {
                    disposition = RowDisposition::FrontMatter;
                }
            }
            let margin = visual.and_then(|v| v.bbox).is_some_and(|b| b[1] < 45.0);
            if number.is_none() && margin && !anchor && !body.trim().is_empty() {
                margin_counts
                    .entry(body.trim().to_string())
                    .or_default()
                    .insert(page.physical_page);
            }
            rows.push(CanonicalRow {
                id: format!("p{}_r{}", page.physical_page, index + 1),
                physical_page: page.physical_page,
                transcript_page: printed,
                row_index: index,
                line_number: number,
                text: body.trim().to_string(),
                raw_text: raw.trim().to_string(),
                event_kind: if header || legacy_header {
                    RowEventKind::PageHeader
                } else {
                    RowEventKind::Unknown
                },
                speaker_anchor_label: None,
                speaker_participant_id: None,
                format_region_id: None,
                source: visual
                    .and_then(|v| v.fragments.first().map(|f| f.source))
                    .unwrap_or(if document.kind == crate::document::DocumentKind::Text {
                        TextSource::PlainText
                    } else {
                        TextSource::FallbackText
                    }),
                disposition,
                section: page.section,
                panel: visual.map(|v| v.panel).unwrap_or(0),
                bbox: visual.and_then(|v| v.bbox),
                ocr_confidence: visual.and_then(|v| v.ocr_confidence),
                timestamp: None,
            });
        }
    }
    // Repetition alone is NEVER a reason to remove testimony. Only repeated, unnumbered top margin rows qualify.
    for row in &mut rows {
        if row.line_number.is_none()
            && row.bbox.is_some_and(|b| b[1] < 45.0)
            && margin_counts
                .get(&row.text)
                .is_some_and(|pages| pages.len() >= 3)
        {
            row.disposition = RowDisposition::PageFurniture;
            row.event_kind = RowEventKind::PageHeader;
        }
    }
    rows
}
