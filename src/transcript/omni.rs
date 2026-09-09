//! The AI-facing IR is a projection of the verified parser, never a second parse.
use super::{
    models::{
        ClassificationConfidence, GraphEdgeKind, LineKind, TranscriptBlock, TranscriptContext,
        TranscriptLine, VerificationStatus,
    },
    omni_models::*,
};
use crate::document::ExtractedDocument;
use std::collections::{BTreeMap, BTreeSet};

pub fn build_transcript_ir(
    document: &ExtractedDocument,
    context: &TranscriptContext,
    rows: &mut [CanonicalRow],
    lines: &[TranscriptLine],
    blocks: &[TranscriptBlock],
) -> TranscriptIr {
    let line_map: BTreeMap<_, _> = lines
        .iter()
        .map(|l| (l.source_row_id.as_str(), l))
        .collect();
    for row in rows.iter_mut() {
        if let Some(line) = line_map.get(row.id.as_str()) {
            row.speaker_anchor_label = line.speaker.as_ref().map(|s| s.label.clone());
            row.speaker_participant_id = line.participant_id.clone();
            row.event_kind = match line.kind {
                LineKind::Question => RowEventKind::QuestionMarker,
                LineKind::Answer => RowEventKind::AnswerMarker,
                LineKind::ExaminationHeading => RowEventKind::ExaminationHeading,
                LineKind::Parenthetical | LineKind::ExhibitMarker => RowEventKind::Parenthetical,
                LineKind::RedactionMarker => RowEventKind::Redaction,
                LineKind::Heading => RowEventKind::Procedural,
                LineKind::PageHeader => RowEventKind::PageHeader,
                _ if line.starts_new_block && line.speaker.is_some() => RowEventKind::SpeakerAnchor,
                _ => RowEventKind::Text,
            };
        }
    }
    let mut regions: Vec<FormatRegion> = Vec::new();
    let mut page_signals: BTreeMap<u32, (usize, usize, usize, usize)> = BTreeMap::new();
    for row in rows
        .iter()
        .filter(|r| r.disposition == RowDisposition::Dialogue)
    {
        let signal = page_signals.entry(row.physical_page).or_default();
        if let Some(line) = line_map.get(row.id.as_str()) {
            if line.starts_new_block {
                if line
                    .evidence
                    .items
                    .iter()
                    .any(|e| e.source == super::models::EvidenceSource::SpeakerLabel)
                {
                    signal.0 += 1;
                }
                if line.confidence == ClassificationConfidence::Explicit {
                    if line.kind == LineKind::Question {
                        signal.1 += 1;
                    }
                    if line.kind == LineKind::Answer {
                        signal.2 += 1;
                    }
                }
                if line.kind == LineKind::ExaminationHeading {
                    signal.3 += 1;
                }
            }
        }
    }
    for (page, (speakers, q, a, headings)) in page_signals {
        let grammar = if speakers > 0 && q + a > 0 {
            FormatGrammarKind::Hybrid
        } else if speakers > 0 {
            FormatGrammarKind::SpeakerPrefixed
        } else if q + a > 0 {
            FormatGrammarKind::QaMarkers
        } else {
            FormatGrammarKind::Unknown
        };
        if let Some(last) = regions
            .last_mut()
            .filter(|r| r.grammar == grammar && r.physical_page_end + 1 == page)
        {
            last.physical_page_end = page;
            last.speaker_anchor_observations += speakers;
            last.question_marker_observations += q;
            last.answer_marker_observations += a;
            last.examination_heading_observations += headings;
        } else {
            regions.push(FormatRegion {
                id: format!("format_region_{}", regions.len() + 1),
                grammar,
                physical_page_start: page,
                physical_page_end: page,
                speaker_anchor_observations: speakers,
                question_marker_observations: q,
                answer_marker_observations: a,
                examination_heading_observations: headings,
                confidence: if grammar == FormatGrammarKind::Unknown {
                    0.3
                } else {
                    0.9
                },
            });
        }
    }
    for row in rows.iter_mut() {
        if row.disposition == RowDisposition::Dialogue {
            row.format_region_id = regions
                .iter()
                .find(|r| {
                    r.physical_page_start <= row.physical_page
                        && row.physical_page <= r.physical_page_end
                })
                .map(|r| r.id.clone());
        }
    }
    let row_map: BTreeMap<_, _> = rows.iter().map(|r| (r.id.as_str(), r)).collect();
    let utterances: Vec<_> = blocks
        .iter()
        .map(|block| {
            let source_rows: Vec<_> = block
                .source_row_ids
                .iter()
                .filter_map(|id| row_map.get(id.as_str()).copied())
                .collect();
            let source_confidence = source_rows
                .iter()
                .map(|r| {
                    document
                        .pages
                        .iter()
                        .find(|p| p.physical_page == r.physical_page)
                        .map(|p| p.layout.reconstruction_confidence)
                        .unwrap_or(0.0)
                })
                .fold(1.0f32, f32::min);
            let procedural = matches!(
                block.kind,
                LineKind::Parenthetical
                    | LineKind::ExaminationHeading
                    | LineKind::Heading
                    | LineKind::ExhibitMarker
                    | LineKind::RedactionMarker
            );
            Utterance {
                id: format!("utterance_{}", block.id),
                participant_id: block.participant_id.clone(),
                speaker_label: block.speaker.as_ref().map(|s| s.label.clone()),
                discourse_role: match block.kind {
                    LineKind::Question => DiscourseRole::Question,
                    LineKind::Answer => DiscourseRole::Response,
                    LineKind::Objection => DiscourseRole::Objection,
                    LineKind::InterpreterStatement => DiscourseRole::Interpretation,
                    LineKind::Unknown => DiscourseRole::Unknown,
                    _ if procedural => DiscourseRole::Procedural,
                    _ => DiscourseRole::Statement,
                },
                text: block.text.clone(),
                source_spans: source_spans(&source_rows),
                confidence: UtteranceConfidence {
                    speaker: if block.participant_id.is_some() {
                        0.9
                    } else {
                        0.0
                    },
                    boundary: if block.confidence == ClassificationConfidence::Explicit {
                        0.95
                    } else {
                        0.7
                    },
                    discourse_role: if block.confidence == ClassificationConfidence::Explicit {
                        0.95
                    } else {
                        0.65
                    },
                    source: source_confidence,
                },
                verification: if block.speaker.as_ref().is_some_and(|s| s.redacted) {
                    VerificationStatus::Uncertain
                } else {
                    block.verification
                },
            }
        })
        .collect();
    // Participant entities are observed identities. General name/entity extraction belongs to the later AI layer.
    let entities: Vec<_> = context
        .participants
        .iter()
        .filter(|p| !p.redacted)
        .map(|p| Entity {
            id: format!("entity_{}", p.id),
            canonical_label: p
                .canonical_name
                .clone()
                .unwrap_or_else(|| p.display_label.clone()),
            kind: EntityKind::Person,
            participant_id: Some(p.id.clone()),
            observed_mentions: 0,
        })
        .collect();
    let graph = super::graph::build_transcript_graph(blocks);
    let conversation_edges = graph
        .edges
        .into_iter()
        .map(|edge| ConversationEdge {
            from_utterance_id: format!("utterance_{}", edge.from.trim_start_matches("node_")),
            to_utterance_id: format!("utterance_{}", edge.to.trim_start_matches("node_")),
            relation: match edge.kind {
                GraphEdgeKind::Follows => ConversationRelation::Follows,
                GraphEdgeKind::RespondsTo => ConversationRelation::RespondsTo,
                GraphEdgeKind::Interrupts => ConversationRelation::Interrupts,
                GraphEdgeKind::ResumesAfter => ConversationRelation::Resumes,
                GraphEdgeKind::Interprets => ConversationRelation::Clarifies,
            },
            confidence: if edge.kind == GraphEdgeKind::Follows {
                1.0
            } else {
                0.7
            },
        })
        .collect();
    let page_frames = document
        .pages
        .iter()
        .map(|page| {
            let page_rows: Vec<_> = rows
                .iter()
                .filter(|r| r.physical_page == page.physical_page)
                .collect();
            let crossing = |incoming: bool| {
                utterances
                    .iter()
                    .find(|u| {
                        u.source_spans
                            .iter()
                            .any(|s| s.physical_page_start == page.physical_page)
                            && u.source_spans.iter().any(|s| {
                                if incoming {
                                    s.physical_page_start < page.physical_page
                                } else {
                                    s.physical_page_start > page.physical_page
                                }
                            })
                    })
                    .map(|u| PageContinuity {
                        utterance_id: u.id.clone(),
                        participant_id: u.participant_id.clone(),
                    })
            };
            PageFrame {
                physical_page: page.physical_page,
                transcript_page: context.transcript_page_for(page.physical_page),
                section: page.section,
                row_ids: page_rows.iter().map(|r| r.id.clone()).collect(),
                format_region_ids: page_rows
                    .iter()
                    .filter_map(|r| r.format_region_id.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                continuity_in: crossing(true),
                continuity_out: crossing(false),
            }
        })
        .collect();
    let assigned: BTreeSet<_> = blocks
        .iter()
        .flat_map(|b| b.source_row_ids.iter().cloned())
        .collect();
    let dialogue: Vec<_> = rows
        .iter()
        .filter(|r| r.disposition == RowDisposition::Dialogue)
        .collect();
    let assigned_count = dialogue.iter().filter(|r| assigned.contains(&r.id)).count();
    let unresolved = lines
        .iter()
        .filter(|l| {
            l.speaker.is_none()
                && !matches!(
                    l.kind,
                    LineKind::ExaminationHeading
                        | LineKind::Parenthetical
                        | LineKind::ExhibitMarker
                        | LineKind::Heading
                        | LineKind::RedactionMarker
                )
        })
        .count();
    let coverage = if dialogue.is_empty() {
        0.0
    } else {
        assigned_count as f32 / dialogue.len() as f32
    };
    let confidence = if utterances.is_empty() {
        0.0
    } else {
        utterances
            .iter()
            .map(|u| {
                u.confidence.source.min(u.confidence.discourse_role).min(
                    if u.discourse_role == DiscourseRole::Procedural {
                        1.0
                    } else {
                        u.confidence.speaker
                    },
                )
            })
            .sum::<f32>()
            / utterances.len() as f32
    };
    TranscriptIr {
        document: TranscriptIrDocument {
            document_type: context.document_type,
            physical_page_count: document.pages.len(),
            transcript_page_count: dialogue
                .iter()
                .map(|r| (r.physical_page, r.panel))
                .collect::<BTreeSet<_>>()
                .len(),
            positioned_text_pages: context.layout_summary.positioned_text_pages,
            ocr_required_pages: context.layout_summary.ocr_required_pages,
        },
        diagnostics: OmniDiagnostics {
            canonical_rows: rows.len(),
            transcript_rows: dialogue.len(),
            rows_assigned_to_utterances: assigned_count,
            unresolved_speaker_rows: unresolved,
            format_regions: regions.len(),
            cross_page_utterances: utterances
                .iter()
                .filter(|u| {
                    u.source_spans
                        .iter()
                        .map(|s| s.physical_page_start)
                        .collect::<BTreeSet<_>>()
                        .len()
                        > 1
                })
                .count(),
            utterances: utterances.len(),
            entity_mentions: 0,
            source_row_coverage: coverage,
            conversation_confidence: confidence,
        },
        format_regions: regions,
        participants: context.participants.clone(),
        entities,
        entity_mentions: Vec::new(),
        page_frames,
        canonical_rows: rows.to_vec(),
        utterances,
        conversation_edges,
    }
}

fn source_spans(rows: &[&CanonicalRow]) -> Vec<SourceSpan> {
    let mut spans: Vec<SourceSpan> = Vec::new();
    for row in rows {
        if let Some(last) = spans.last_mut().filter(|s| {
            s.physical_page_end == row.physical_page && s.transcript_page_end == row.transcript_page
        }) {
            last.line_end = row.line_number;
            last.row_ids.push(row.id.clone());
        } else {
            spans.push(SourceSpan {
                physical_page_start: row.physical_page,
                physical_page_end: row.physical_page,
                transcript_page_start: row.transcript_page.clone(),
                transcript_page_end: row.transcript_page.clone(),
                line_start: row.line_number,
                line_end: row.line_number,
                row_ids: vec![row.id.clone()],
            });
        }
    }
    spans
}
