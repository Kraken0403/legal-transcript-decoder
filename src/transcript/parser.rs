use std::collections::{BTreeMap, BTreeSet};

use crate::document::ExtractedDocument;

use super::{
    error::TranscriptParseError,
    format::{is_page_header_content, parse_numbered_line},
    graph::build_transcript_graph,
    markers::{MarkerMatch, match_marker},
    models::{
        ClassificationConfidence, ClassificationEvidence, EvidenceItem, EvidenceSource, LineKind,
        MarkerKind, ParseDiagnostics, ParsedTranscript, Participant, ParticipantFunction,
        SourceContinuity, Speaker, SpeakerRole, TestimonyExchange, TranscriptBlock,
        TranscriptContext, TranscriptLine, TranscriptLocation, VerificationStatus,
    },
    omni::build_transcript_ir,
    profile::TranscriptProfile,
    speaker::{
        anonymous_speaker, is_pure_redaction_marker, is_redacted_label,
        looks_like_bare_attorney_heading, resolve_known_speaker, speaker_from_label,
        split_titled_speaker_prefix, unknown_attorney, witness_speaker,
    },
    verifier::verify_transcript_lines,
};

#[derive(Debug, Clone)]
struct ContinuationContext {
    kind: LineKind,
    speaker: Option<Speaker>,
}

#[derive(Debug, Default)]
struct ParserState {
    current_questioner: Option<Speaker>,
    current_witness: Option<Speaker>,
    continuation: Option<ContinuationContext>,
    last_physical_page: Option<u32>,
    last_transcript_page: Option<u32>,
    last_line_number: Option<u16>,
    anonymous_attorney_sequence: usize,
    anonymous_witness_sequence: usize,
    anonymous_unknown_sequence: usize,
    redacted_witness: Option<Speaker>,
    redacted_objector: Option<Speaker>,
}

impl ParserState {
    fn new_redacted_attorney(&mut self) -> Speaker {
        self.anonymous_attorney_sequence += 1;
        anonymous_speaker(
            SpeakerRole::Attorney,
            self.anonymous_attorney_sequence,
            true,
        )
    }

    fn redacted_witness(&mut self) -> Speaker {
        if let Some(speaker) = self.redacted_witness.clone() {
            return speaker;
        }
        self.anonymous_witness_sequence += 1;
        let speaker =
            anonymous_speaker(SpeakerRole::Witness, self.anonymous_witness_sequence, true);
        self.redacted_witness = Some(speaker.clone());
        speaker
    }

    fn redacted_objector(&mut self) -> Speaker {
        if let Some(speaker) = self.redacted_objector.clone() {
            return speaker;
        }
        let speaker = self.new_redacted_attorney();
        self.redacted_objector = Some(speaker.clone());
        speaker
    }

    fn redacted_unknown(&mut self) -> Speaker {
        self.anonymous_unknown_sequence += 1;
        anonymous_speaker(SpeakerRole::Unknown, self.anonymous_unknown_sequence, true)
    }
}

#[derive(Debug)]
struct ClassifiedLine {
    kind: LineKind,
    speaker: Option<Speaker>,
    confidence: ClassificationConfidence,
    starts_new_block: bool,
    text: String,
    evidence: ClassificationEvidence,
}

pub fn parse_transcript(
    filename: String,
    document: &ExtractedDocument,
    context: &TranscriptContext,
    profile: &TranscriptProfile,
) -> Result<ParsedTranscript, TranscriptParseError> {
    let mut lines = Vec::new();
    let mut state = ParserState {
        current_questioner: None,
        current_witness: if context
            .participants
            .iter()
            .filter(|p| p.role == SpeakerRole::Witness)
            .count()
            <= 1
        {
            context.primary_witness.clone()
        } else {
            None
        },
        ..ParserState::default()
    };
    let mut detected_pages = BTreeSet::new();

    let mut source_rows = super::source::collect_rows(document, context, profile);
    for row in &source_rows {
        use super::omni_models::RowDisposition;
        if row.disposition == RowDisposition::BlankNumberedLine && row.section.is_dialogue() {
            update_parser_position(
                &mut state,
                row.physical_page,
                row.transcript_page.as_deref(),
                row.line_number,
            );
            continue;
        }
        if row.disposition != RowDisposition::Dialogue {
            if row.disposition != RowDisposition::PageFurniture {
                state.continuation = None;
            }
            continue;
        }
        let physical_page = row.physical_page;
        let transcript_page = row.transcript_page.clone();
        let line_number = row.line_number;
        detected_pages.insert((physical_page, row.panel));
        let source_continuity = determine_source_continuity(
            &state,
            physical_page,
            transcript_page.as_deref(),
            line_number,
        );
        if continuation_boundary_is_suspicious(
            &state,
            physical_page,
            transcript_page.as_deref(),
            line_number,
        ) {
            state.continuation = None;
        }
        let allow_bare_marker = document
            .pages
            .iter()
            .find(|p| p.physical_page == physical_page)
            .and_then(|p| p.visual_rows.get(row.row_index))
            .map_or(true, |v| {
                if !v
                    .fragments
                    .first()
                    .is_some_and(|f| matches!(f.text.trim(), "Q" | "A"))
                {
                    return true;
                }
                if v.fragments.len() < 2 {
                    return true;
                }
                let marker = &v.fragments[0];
                let next = &v.fragments[1];
                // PDF gutter geometry distinguishes structural "A     Yes" from prose "A product".
                next.x - (marker.x + marker.width) > marker.font_size.unwrap_or(10.0) * 0.65
            });
        let classified = classify_line(
            &row.text,
            line_number,
            &mut state,
            context,
            allow_bare_marker,
        );
        let participant_id = classified
            .speaker
            .as_ref()
            .and_then(|speaker| speaker.participant_id.clone());
        lines.push(TranscriptLine {
            source_row_id: row.id.clone(),
            physical_page,
            transcript_page: transcript_page.clone(),
            line_number,
            kind: classified.kind,
            speaker: classified.speaker,
            participant_id,
            confidence: classified.confidence,
            verification: VerificationStatus::Uncertain,
            source_continuity,
            evidence: classified.evidence,
            starts_new_block: classified.starts_new_block,
            text: classified.text,
            raw_text: row.raw_text.clone(),
        });
        update_parser_position(
            &mut state,
            physical_page,
            transcript_page.as_deref(),
            line_number,
        );
    }
    let physical_pages_without_label = source_rows
        .iter()
        .filter(|r| {
            r.disposition == super::omni_models::RowDisposition::Dialogue
                && r.transcript_page.is_none()
        })
        .map(|r| r.physical_page)
        .collect::<BTreeSet<_>>()
        .len();
    // Unknown text stays unknown. Context-only recovery used to invent missing speakers/answers.
    complete_speaker_turns(&mut lines);

    let mut resolved_context = context.clone();
    reconcile_dynamic_participants(&mut resolved_context, &lines);
    let verification_issues = verify_transcript_lines(&mut lines, &resolved_context, profile);

    let blocks = build_blocks(&lines);
    let (exchanges, unpaired_answers) = build_exchanges(&blocks);
    let graph = build_transcript_graph(&blocks);
    let omni = build_transcript_ir(
        document,
        &resolved_context,
        &mut source_rows,
        &lines,
        &blocks,
    );
    let quality = super::quality::assess(
        document,
        &resolved_context,
        &omni,
        &lines,
        &blocks,
        &verification_issues,
    );

    let inferred_lines = lines
        .iter()
        .filter(|line| line.confidence == ClassificationConfidence::Inferred)
        .count();
    let unknown_lines = lines
        .iter()
        .filter(|line| line.kind == LineKind::Unknown)
        .count();
    let verified_lines = lines
        .iter()
        .filter(|line| line.verification == VerificationStatus::Verified)
        .count();
    let verified_with_inference_lines = lines
        .iter()
        .filter(|line| line.verification == VerificationStatus::VerifiedWithInference)
        .count();
    let conflict_lines = lines
        .iter()
        .filter(|line| line.verification == VerificationStatus::Conflict)
        .count();
    let uncertain_lines = lines
        .iter()
        .filter(|line| line.verification == VerificationStatus::Uncertain)
        .count();
    let unanswered_questions = exchanges
        .iter()
        .filter(|exchange| {
            !exchange
                .response_sequence
                .iter()
                .any(|block| block.kind == LineKind::Answer)
        })
        .count();
    let transcript_line_count = lines
        .iter()
        .filter(|line| line.kind != LineKind::PageHeader)
        .count();
    let legacy_parse_confidence = calculate_parse_confidence(&lines);
    let coverage_factor = 0.45 + 0.55 * omni.diagnostics.source_row_coverage;
    let parse_confidence = ((legacy_parse_confidence * 0.35
        + omni.diagnostics.conversation_confidence * 0.65)
        * coverage_factor)
        .clamp(0.0, 1.0)
        .min(quality.confidence_ceiling);

    let dark_redaction_candidates = resolved_context
        .redaction_summary
        .dark_annotation_candidates
        + resolved_context
            .redaction_summary
            .dark_filled_rectangle_candidates;
    let anonymous_participants = resolved_context
        .participants
        .iter()
        .filter(|participant| participant.redacted || participant.canonical_name.is_none())
        .count();

    Ok(ParsedTranscript {
        schema_version: "2.0".to_string(),
        source_sha256: None,
        quality,
        source_pages: document.pages.clone(),
        filename,
        profile: resolved_context.profile_name.clone(),
        physical_page_count: document.source_page_count(),
        transcript_page_count: detected_pages.len(),
        line_count: transcript_line_count,
        context: resolved_context.clone(),
        lines,
        blocks,
        exchanges,
        graph,
        omni,
        diagnostics: ParseDiagnostics {
            inferred_lines,
            unknown_lines,
            verified_lines,
            verified_with_inference_lines,
            conflict_lines,
            uncertain_lines,
            detected_transcript_pages: detected_pages.len(),
            physical_pages_without_transcript_page: physical_pages_without_label,
            unanswered_questions,
            unpaired_answers: unpaired_answers.len(),
            detected_gaps: resolved_context.gaps.len(),
            anonymous_participants,
            explicit_redaction_annotations: resolved_context
                .redaction_summary
                .explicit_redaction_annotations,
            dark_redaction_candidates,
            textual_redaction_markers: resolved_context.redaction_summary.textual_redaction_markers,
            parse_confidence,
        },
        unpaired_answers,
        verification_issues,
    })
}

fn complete_speaker_turns(lines: &mut [TranscriptLine]) {
    let mut start = 0;
    let mut previous_question_speaker: Option<String> = None;
    while start < lines.len() {
        let mut end = start + 1;
        while end < lines.len()
            && !lines[end].starts_new_block
            && lines[end].speaker == lines[start].speaker
        {
            end += 1;
        }
        let labelled = lines[start]
            .evidence
            .items
            .iter()
            .any(|e| e.source == EvidenceSource::SpeakerLabel);
        if labelled
            && !matches!(
                lines[start].kind,
                LineKind::Question | LineKind::Answer | LineKind::Objection
            )
        {
            let question = lines[end - 1].text.trim_end().ends_with('?');
            let responding_witness = lines[start]
                .speaker
                .as_ref()
                .is_some_and(|s| s.role == SpeakerRole::Witness)
                && previous_question_speaker.is_some()
                && previous_question_speaker != lines[start].participant_id;
            let kind = if question {
                Some(LineKind::Question)
            } else if responding_witness {
                Some(LineKind::Answer)
            } else {
                None
            };
            if let Some(kind) = kind {
                for line in &mut lines[start..end] {
                    line.kind = kind;
                    line.confidence = ClassificationConfidence::Inferred;
                    add_evidence(
                        &mut line.evidence,
                        EvidenceSource::RoleBehavior,
                        0.7,
                        "Discourse inferred from the complete labelled turn; participant role is unchanged.",
                    );
                }
            }
        }
        match lines[start].kind {
            LineKind::Question => previous_question_speaker = lines[start].participant_id.clone(),
            LineKind::ExaminationHeading | LineKind::Heading | LineKind::RedactionMarker => {
                previous_question_speaker = None
            }
            _ => {}
        }
        start = end;
    }
}

fn classify_line(
    content: &str,
    line_number: Option<u16>,
    state: &mut ParserState,
    context: &TranscriptContext,
    allow_bare_marker: bool,
) -> ClassifiedLine {
    let content = content.trim();
    if let Some(label) = super::speaker::witness_introduction(content) {
        let mut speaker = resolve_known_speaker(label, context).unwrap_or_else(|| Speaker {
            participant_id: Some(super::speaker::label_id(label)),
            label: label.to_string(),
            role: SpeakerRole::Witness,
            redacted: false,
        });
        speaker.role = SpeakerRole::Witness;
        state.current_witness = Some(speaker.clone());
        state.current_questioner = None;
        state.continuation = None;
        return classified(
            LineKind::Heading,
            Some(speaker),
            ClassificationConfidence::Explicit,
            true,
            content,
            explicit_structural_evidence(
                "Explicit witness introduction starts a new testimony scope.",
            ),
        );
    }

    if let Some(attorney) = parse_by_heading(content, state, context) {
        state.current_questioner = Some(attorney.clone());
        state.continuation = None;
        let mut evidence = explicit_structural_evidence("Examination heading identifies examiner.");
        add_evidence(
            &mut evidence,
            EvidenceSource::CurrentExaminer,
            1.0,
            "Current examiner established by BY heading.",
        );
        return classified(
            LineKind::ExaminationHeading,
            Some(attorney),
            ClassificationConfidence::Explicit,
            true,
            content,
            evidence,
        );
    }

    if is_examination_heading(content) {
        state.continuation = None;
        return classified(
            LineKind::ExaminationHeading,
            None,
            ClassificationConfidence::Explicit,
            true,
            content,
            explicit_structural_evidence("Recognized examination heading."),
        );
    }

    if let Some((label, remainder)) = split_speaker_prefix(content, context) {
        let speaker = resolve_prefixed_speaker(label, remainder, state, context);
        return classify_prefixed_speaker(speaker, remainder, state, context);
    }

    if is_parenthetical_start(content) {
        let kind = if is_exhibit_marker(content) {
            LineKind::ExhibitMarker
        } else {
            LineKind::Parenthetical
        };

        if parenthetical_is_closed(content) {
            state.continuation = None;
        } else {
            state.continuation = Some(ContinuationContext {
                kind: kind.clone(),
                speaker: None,
            });
        }

        return classified(
            kind,
            None,
            ClassificationConfidence::Explicit,
            true,
            content,
            explicit_structural_evidence("Recognized parenthetical/exhibit event."),
        );
    }

    if let Some(marker) = match_marker(content, context)
        .filter(|m| m.style != super::models::MarkerStyle::BareUppercase || allow_bare_marker)
    {
        return classify_explicit_marker(marker, state, context);
    }

    if is_pure_redaction_marker(content) {
        state.continuation = None;
        let mut evidence = ClassificationEvidence::default();
        add_evidence(
            &mut evidence,
            EvidenceSource::Redaction,
            1.0,
            "Source line consists of a redaction/sealed marker.",
        );
        return classified(
            LineKind::RedactionMarker,
            None,
            ClassificationConfidence::Explicit,
            true,
            content,
            evidence,
        );
    }

    if looks_like_objection(content) {
        state.continuation = Some(ContinuationContext {
            kind: LineKind::Objection,
            speaker: None,
        });
        let mut evidence = ClassificationEvidence::default();
        evidence.objection_score = 1.0;
        add_evidence(
            &mut evidence,
            EvidenceSource::RoleBehavior,
            1.0,
            "Text explicitly contains objection language.",
        );
        return classified(
            LineKind::Objection,
            None,
            ClassificationConfidence::Explicit,
            true,
            content,
            evidence,
        );
    }

    if is_general_heading(content) {
        state.continuation = None;
        return classified(
            LineKind::Heading,
            None,
            ClassificationConfidence::Explicit,
            true,
            content,
            explicit_structural_evidence("Recognized transcript heading."),
        );
    }

    if let Some(active) = state.continuation.clone() {
        let closes_parenthetical = matches!(
            active.kind,
            LineKind::Parenthetical | LineKind::ExhibitMarker
        ) && parenthetical_is_closed(content);

        if closes_parenthetical {
            state.continuation = None;
        }

        let mut evidence = ClassificationEvidence::default();
        score_kind(&mut evidence, &active.kind, 0.72);
        add_evidence(
            &mut evidence,
            EvidenceSource::PreviousLine,
            0.72,
            "Unmarked row continues the active speaker turn; source boundaries are retained.",
        );
        return classified(
            active.kind,
            active.speaker,
            ClassificationConfidence::Inferred,
            false,
            content,
            evidence,
        );
    }

    classified(
        LineKind::Unknown,
        None,
        ClassificationConfidence::Unknown,
        true,
        content,
        ClassificationEvidence::default(),
    )
}

fn classify_explicit_marker(
    marker: MarkerMatch<'_>,
    state: &mut ParserState,
    context: &TranscriptContext,
) -> ClassifiedLine {
    let mut evidence = ClassificationEvidence::default();
    let marker_weight = if marker.priority == 1 { 1.0 } else { 0.94 };
    add_evidence(
        &mut evidence,
        EvidenceSource::ExplicitMarker,
        marker_weight,
        &format!(
            "Matched transcript marker {} with priority {} ({:?}).",
            marker.pattern, marker.priority, marker.style
        ),
    );
    add_evidence(
        &mut evidence,
        EvidenceSource::FormatFingerprint,
        0.40,
        "Marker is part of this document's learned format fingerprint.",
    );

    match marker.kind {
        MarkerKind::Question => {
            evidence.question_score = marker_weight;
            let (speaker, question_text) = parse_question_speaker(marker.remainder, state, context);
            let speaker = speaker
                .or_else(|| state.current_questioner.clone())
                .unwrap_or_else(unknown_attorney);

            if state.current_questioner.is_some() {
                add_evidence(
                    &mut evidence,
                    EvidenceSource::CurrentExaminer,
                    0.60,
                    "Question attributed using active examiner state.",
                );
            }

            state.current_questioner = Some(speaker.clone());
            state.continuation = Some(ContinuationContext {
                kind: LineKind::Question,
                speaker: Some(speaker.clone()),
            });

            classified(
                LineKind::Question,
                Some(speaker),
                ClassificationConfidence::Explicit,
                true,
                question_text,
                evidence,
            )
        }
        MarkerKind::Answer => {
            evidence.answer_score = marker_weight;
            let witness = state.current_witness.clone().unwrap_or_else(|| Speaker {
                participant_id: Some("witness_unresolved".to_string()),
                label: "UNRESOLVED WITNESS".to_string(),
                role: SpeakerRole::Witness,
                redacted: false,
            });
            state.current_witness = Some(witness.clone());
            add_evidence(
                &mut evidence,
                EvidenceSource::KnownWitness,
                0.70,
                "Answer attributed to primary/known witness for this transcript.",
            );
            let answer = strip_witness_prefix(marker.remainder);
            state.continuation = Some(ContinuationContext {
                kind: LineKind::Answer,
                speaker: Some(witness.clone()),
            });

            classified(
                LineKind::Answer,
                Some(witness),
                ClassificationConfidence::Explicit,
                true,
                answer,
                evidence,
            )
        }
    }
}

fn classify_prefixed_speaker(
    mut speaker: Speaker,
    remainder: &str,
    state: &mut ParserState,
    context: &TranscriptContext,
) -> ClassifiedLine {
    // Speaker anchors and Q/A markers are separate structural channels. A labelled utterance
    // such as "DAVID MARKUS: A what?" must remain David Markus speaking ordinary text, not
    // become an A-marker answer merely because another region of the document uses Q/A.

    // In a document/region with an established Q/A convention, a validated speaker
    // anchor may itself prefix a real marker, e.g. "[REDACTED]: Q. Who was there?".
    // Keep this separate from speaker-only transcripts such as Maxwell, where ordinary
    // utterances like "DAVID MARKUS: A what?" must remain plain speaker dialogue.
    if let Some(marker) = match_marker(remainder, context)
        .filter(|m| m.style != super::models::MarkerStyle::BareUppercase)
    {
        let mut evidence = marker_evidence(
            &marker,
            match marker.kind {
                MarkerKind::Question => LineKind::Question,
                MarkerKind::Answer => LineKind::Answer,
            },
        );
        add_evidence(
            &mut evidence,
            EvidenceSource::SpeakerLabel,
            0.90,
            "Validated speaker anchor prefixes an established Q/A marker.",
        );

        return match marker.kind {
            MarkerKind::Question => {
                state.current_questioner = Some(speaker.clone());
                state.continuation = Some(ContinuationContext {
                    kind: LineKind::Question,
                    speaker: Some(speaker.clone()),
                });
                classified(
                    LineKind::Question,
                    Some(speaker),
                    ClassificationConfidence::Explicit,
                    true,
                    marker.remainder,
                    evidence,
                )
            }
            MarkerKind::Answer => {
                state.current_witness = Some(speaker.clone());
                state.continuation = Some(ContinuationContext {
                    kind: LineKind::Answer,
                    speaker: Some(speaker.clone()),
                });
                classified(
                    LineKind::Answer,
                    Some(speaker),
                    ClassificationConfidence::Explicit,
                    true,
                    marker.remainder,
                    evidence,
                )
            }
        };
    }

    match speaker.role {
        SpeakerRole::Attorney => {
            if remainder.trim_end().ends_with('?') {
                state.current_questioner = Some(speaker.clone());
                state.continuation = Some(ContinuationContext {
                    kind: LineKind::Question,
                    speaker: Some(speaker.clone()),
                });
                let mut evidence = ClassificationEvidence::default();
                evidence.question_score = 0.86;
                add_evidence(
                    &mut evidence,
                    EvidenceSource::SpeakerLabel,
                    0.80,
                    "Speaker-labelled transcript: known attorney label.",
                );
                add_evidence(
                    &mut evidence,
                    EvidenceSource::QuestionPunctuation,
                    0.75,
                    "Speaker-labelled utterance ends with question punctuation.",
                );
                return classified(
                    LineKind::Question,
                    Some(speaker),
                    ClassificationConfidence::Inferred,
                    true,
                    remainder,
                    evidence,
                );
            }

            let kind = if looks_like_objection(remainder) {
                LineKind::Objection
            } else {
                LineKind::AttorneyStatement
            };
            if kind == LineKind::Objection {
                state.redacted_objector = if speaker.redacted {
                    Some(speaker.clone())
                } else {
                    state.redacted_objector.clone()
                };
            }
            state.continuation = Some(ContinuationContext {
                kind: kind.clone(),
                speaker: Some(speaker.clone()),
            });
            let mut evidence = ClassificationEvidence::default();
            score_kind(&mut evidence, &kind, 0.92);
            add_evidence(
                &mut evidence,
                EvidenceSource::SpeakerLabel,
                0.92,
                "Known/inferred attorney speaker label.",
            );
            if kind == LineKind::Objection {
                add_evidence(
                    &mut evidence,
                    EvidenceSource::RoleBehavior,
                    1.0,
                    "Objection language strongly supports attorney role.",
                );
            }
            classified(
                kind,
                Some(speaker),
                ClassificationConfidence::Explicit,
                true,
                remainder,
                evidence,
            )
        }
        SpeakerRole::Witness => {
            state.current_witness = Some(speaker.clone());
            if state
                .continuation
                .as_ref()
                .is_some_and(|active| active.kind == LineKind::Question)
            {
                state.continuation = Some(ContinuationContext {
                    kind: LineKind::Answer,
                    speaker: Some(speaker.clone()),
                });
                let mut evidence = ClassificationEvidence::default();
                evidence.answer_score = 0.84;
                add_evidence(
                    &mut evidence,
                    EvidenceSource::SpeakerLabel,
                    0.80,
                    "Speaker-labelled transcript: known witness label.",
                );
                add_evidence(
                    &mut evidence,
                    EvidenceSource::PreviousLine,
                    0.70,
                    "Witness utterance immediately follows active question.",
                );
                return classified(
                    LineKind::Answer,
                    Some(speaker),
                    ClassificationConfidence::Inferred,
                    true,
                    remainder,
                    evidence,
                );
            }
            set_explicit_statement(
                speaker,
                remainder,
                LineKind::WitnessStatement,
                state,
                "Known/inferred witness speaker label.",
            )
        }
        SpeakerRole::Interpreter => set_explicit_statement(
            speaker,
            remainder,
            LineKind::InterpreterStatement,
            state,
            "Interpreter speaker label.",
        ),
        SpeakerRole::CourtReporter => set_explicit_statement(
            speaker,
            remainder,
            LineKind::ReporterStatement,
            state,
            "Court reporter speaker label.",
        ),
        SpeakerRole::Judge => set_explicit_statement(
            speaker,
            remainder,
            LineKind::JudgeStatement,
            state,
            "Judge/court speaker label.",
        ),
        SpeakerRole::Videographer => set_explicit_statement(
            speaker,
            remainder,
            LineKind::VideographerStatement,
            state,
            "Videographer speaker label.",
        ),
        SpeakerRole::Unknown => {
            if remainder.trim_end().ends_with('?') {
                state.current_questioner = Some(speaker.clone());
                state.continuation = Some(ContinuationContext {
                    kind: LineKind::Question,
                    speaker: Some(speaker.clone()),
                });
                let mut evidence = ClassificationEvidence::default();
                evidence.question_score = 0.78;
                add_evidence(
                    &mut evidence,
                    EvidenceSource::QuestionPunctuation,
                    0.72,
                    "Unmarked speaker-labelled utterance ends in a question.",
                );
                add_evidence(
                    &mut evidence,
                    EvidenceSource::RoleBehavior,
                    0.65,
                    "Question behavior supports examiner/attorney role.",
                );
                return classified(
                    LineKind::Question,
                    Some(speaker),
                    ClassificationConfidence::Inferred,
                    true,
                    remainder,
                    evidence,
                );
            }

            state.continuation = Some(ContinuationContext {
                kind: LineKind::SpeakerStatement,
                speaker: Some(speaker.clone()),
            });
            let mut evidence = ClassificationEvidence::default();
            evidence.statement_score = 0.45;
            add_evidence(
                &mut evidence,
                EvidenceSource::SpeakerLabel,
                0.45,
                "Speaker label detected, but role is unresolved.",
            );
            classified(
                LineKind::SpeakerStatement,
                Some(speaker),
                ClassificationConfidence::Explicit,
                true,
                remainder,
                evidence,
            )
        }
    }
}

fn set_explicit_statement(
    speaker: Speaker,
    text: &str,
    kind: LineKind,
    state: &mut ParserState,
    note: &str,
) -> ClassifiedLine {
    state.continuation = Some(ContinuationContext {
        kind: kind.clone(),
        speaker: Some(speaker.clone()),
    });
    let mut evidence = ClassificationEvidence::default();
    evidence.statement_score = 0.92;
    add_evidence(&mut evidence, EvidenceSource::SpeakerLabel, 0.92, note);
    classified(
        kind,
        Some(speaker),
        ClassificationConfidence::Explicit,
        true,
        text,
        evidence,
    )
}

fn classified(
    kind: LineKind,
    speaker: Option<Speaker>,
    confidence: ClassificationConfidence,
    starts_new_block: bool,
    text: &str,
    evidence: ClassificationEvidence,
) -> ClassifiedLine {
    ClassifiedLine {
        kind,
        speaker,
        confidence,
        starts_new_block,
        text: text.to_string(),
        evidence,
    }
}

fn continuation_boundary_is_suspicious(
    state: &ParserState,
    physical_page: u32,
    transcript_page: Option<&str>,
    line_number: Option<u16>,
) -> bool {
    if state.continuation.is_none() {
        return false;
    }

    if state
        .last_physical_page
        .is_some_and(|p| physical_page > p + 1)
    {
        return true;
    }
    let Some(current_line) = line_number else {
        return false;
    };
    let Some(previous_physical_page) = state.last_physical_page else {
        return false;
    };

    if previous_physical_page == physical_page {
        let Some(previous_line) = state.last_line_number else {
            return false;
        };
        return current_line != previous_line.saturating_add(1);
    }

    let current_transcript_page = transcript_page.and_then(|page| page.parse::<u32>().ok());
    match (state.last_transcript_page, current_transcript_page) {
        (Some(previous_page), Some(current_page)) => {
            current_page != previous_page + 1 || current_line != 1
        }
        _ => false,
    }
}

fn determine_source_continuity(
    state: &ParserState,
    physical_page: u32,
    transcript_page: Option<&str>,
    line_number: Option<u16>,
) -> SourceContinuity {
    if let Some(previous) = state.last_physical_page {
        if physical_page > previous + 1 {
            return SourceContinuity::GapBefore;
        }
        if line_number.is_none() && state.last_line_number.is_none() {
            return if previous == physical_page {
                SourceContinuity::Continuous
            } else {
                SourceContinuity::CrossPageContinuous
            };
        }
    }
    let (Some(previous_physical), Some(previous_line), Some(current_line)) = (
        state.last_physical_page,
        state.last_line_number,
        line_number,
    ) else {
        return SourceContinuity::FirstObserved;
    };

    if previous_physical == physical_page {
        return if current_line == previous_line.saturating_add(1) {
            SourceContinuity::Continuous
        } else {
            SourceContinuity::GapBefore
        };
    }

    let current_page = transcript_page.and_then(|page| page.parse::<u32>().ok());
    match (state.last_transcript_page, current_page) {
        (Some(previous_page), Some(current_page))
            if current_page == previous_page + 1 && current_line == 1 =>
        {
            SourceContinuity::CrossPageContinuous
        }
        (Some(_), Some(_)) => SourceContinuity::GapBefore,
        _ => SourceContinuity::Unknown,
    }
}

fn update_parser_position(
    state: &mut ParserState,
    physical_page: u32,
    transcript_page: Option<&str>,
    line_number: Option<u16>,
) {
    state.last_physical_page = Some(physical_page);
    state.last_transcript_page = transcript_page.and_then(|page| page.parse::<u32>().ok());
    state.last_line_number = line_number;
}

fn build_blocks(lines: &[TranscriptLine]) -> Vec<TranscriptBlock> {
    let mut blocks: Vec<TranscriptBlock> = Vec::new();

    for line in lines {
        if line.kind == LineKind::PageHeader {
            continue;
        }

        if let Some(last) = blocks.last_mut() {
            let can_merge = !line.starts_new_block
                && line.source_continuity != SourceContinuity::GapBefore
                && last.kind == line.kind
                && last.speaker == line.speaker;

            if can_merge {
                if !line.text.is_empty() {
                    if !last.text.is_empty() {
                        last.text.push(' ');
                    }
                    last.text.push_str(&line.text);
                }
                last.source_row_ids.push(line.source_row_id.clone());
                last.end = location_from_line(line);
                last.confidence = combine_confidence(last.confidence, line.confidence);
                last.verification = combine_verification(last.verification, line.verification);
                continue;
            }
        }

        let block_number = blocks.len() + 1;
        blocks.push(TranscriptBlock {
            id: format!("block_{block_number:06}"),
            source_row_ids: vec![line.source_row_id.clone()],
            kind: line.kind.clone(),
            speaker: line.speaker.clone(),
            participant_id: line.participant_id.clone(),
            confidence: line.confidence,
            verification: line.verification,
            source_continuity: line.source_continuity,
            start: location_from_line(line),
            end: location_from_line(line),
            text: line.text.clone(),
        });
    }

    blocks
}

fn build_exchanges(blocks: &[TranscriptBlock]) -> (Vec<TestimonyExchange>, Vec<TranscriptBlock>) {
    let mut exchanges = Vec::new();
    let mut unpaired_answers = Vec::new();
    let mut current: Option<TestimonyExchange> = None;

    for block in blocks {
        if block.source_continuity == SourceContinuity::GapBefore
            || matches!(
                block.kind,
                LineKind::ExaminationHeading | LineKind::Heading | LineKind::RedactionMarker
            )
            || (block.kind == LineKind::Parenthetical
                && ["off the record", "concluded", "adjourned", "recess"]
                    .iter()
                    .any(|v| block.text.to_ascii_lowercase().contains(v)))
        {
            if let Some(exchange) = current.take() {
                exchanges.push(exchange);
            }
        }
        if block.kind == LineKind::Question {
            if let Some(exchange) = current.take() {
                exchanges.push(exchange);
            }
            current = Some(TestimonyExchange {
                question: block.clone(),
                response_sequence: Vec::new(),
            });
            continue;
        }

        if let Some(exchange) = current.as_mut() {
            exchange.response_sequence.push(block.clone());
        } else if block.kind == LineKind::Answer {
            unpaired_answers.push(block.clone());
        }
    }

    if let Some(exchange) = current {
        exchanges.push(exchange);
    }

    (exchanges, unpaired_answers)
}

fn location_from_line(line: &TranscriptLine) -> TranscriptLocation {
    TranscriptLocation {
        physical_page: line.physical_page,
        transcript_page: line.transcript_page.clone(),
        line_number: line.line_number,
    }
}

fn combine_confidence(
    left: ClassificationConfidence,
    right: ClassificationConfidence,
) -> ClassificationConfidence {
    if left == ClassificationConfidence::Unknown || right == ClassificationConfidence::Unknown {
        ClassificationConfidence::Unknown
    } else if left == ClassificationConfidence::Inferred
        || right == ClassificationConfidence::Inferred
    {
        ClassificationConfidence::Inferred
    } else {
        ClassificationConfidence::Explicit
    }
}

fn combine_verification(left: VerificationStatus, right: VerificationStatus) -> VerificationStatus {
    use VerificationStatus::*;
    if left == Conflict || right == Conflict {
        Conflict
    } else if left == Uncertain || right == Uncertain {
        Uncertain
    } else if left == SourceMissing || right == SourceMissing {
        SourceMissing
    } else if left == Redacted || right == Redacted {
        Redacted
    } else if left == VerifiedWithInference || right == VerifiedWithInference {
        VerifiedWithInference
    } else {
        Verified
    }
}

fn split_speaker_prefix<'a>(
    text: &'a str,
    _context: &TranscriptContext,
) -> Option<(&'a str, &'a str)> {
    super::speaker::split_colon_label(text).or_else(|| split_titled_speaker_prefix(text))
}

fn resolve_prefixed_speaker(
    label: &str,
    remainder: &str,
    state: &mut ParserState,
    context: &TranscriptContext,
) -> Speaker {
    if matches!(
        label.trim().to_ascii_uppercase().as_str(),
        "THE WITNESS" | "WITNESS" | "DEPONENT" | "THE DEPONENT"
    ) {
        if let Some(speaker) = state.current_witness.clone() {
            return speaker;
        }
    }
    if !is_redacted_label(label)
        && let Some(speaker) = resolve_known_speaker(label, context)
    {
        return speaker;
    }

    if is_redacted_label(label) {
        if let Some(marker) = match_marker(remainder, context) {
            return match marker.kind {
                MarkerKind::Question => {
                    if let Some(current) = state.current_questioner.clone()
                        && current.redacted
                        && current.role == SpeakerRole::Attorney
                    {
                        current
                    } else {
                        let speaker = state.new_redacted_attorney();
                        state.current_questioner = Some(speaker.clone());
                        speaker
                    }
                }
                MarkerKind::Answer => {
                    let speaker = state.redacted_witness();
                    state.current_witness = Some(speaker.clone());
                    speaker
                }
            };
        }

        if looks_like_objection(remainder) {
            return state.redacted_objector();
        }

        if let Some(active) = state.continuation.as_ref()
            && let Some(speaker) = active.speaker.as_ref()
            && speaker.redacted
        {
            return speaker.clone();
        }

        return state.redacted_unknown();
    }

    speaker_from_label(label).unwrap_or_else(|| Speaker {
        participant_id: Some(super::speaker::label_id(label)),
        label: label.trim().to_string(),
        role: infer_role_from_remainder(remainder, context),
        redacted: false,
    })
}

fn infer_role_from_remainder(remainder: &str, context: &TranscriptContext) -> SpeakerRole {
    if let Some(marker) = match_marker(remainder, context) {
        return match marker.kind {
            MarkerKind::Question => SpeakerRole::Attorney,
            MarkerKind::Answer => SpeakerRole::Witness,
        };
    }
    if looks_like_objection(remainder) {
        SpeakerRole::Attorney
    } else {
        SpeakerRole::Unknown
    }
}

fn parse_by_heading(
    text: &str,
    state: &mut ParserState,
    context: &TranscriptContext,
) -> Option<Speaker> {
    let trimmed = text.trim();
    let remainder = strip_prefix_ascii_case(trimmed, "BY ")?;
    if !remainder.ends_with(':') {
        return None;
    }

    let label = remainder.trim_end_matches(':').trim();
    if label.is_empty() {
        return None;
    }

    if is_redacted_label(label) {
        return Some(state.new_redacted_attorney());
    }

    if let Some(speaker) = resolve_known_speaker(label, context) {
        return Some(speaker);
    }

    if looks_like_bare_attorney_heading(label) || looks_like_generic_speaker_label(label) {
        return Some(Speaker {
            participant_id: Some(super::speaker::label_id(label)),
            label: label.to_string(),
            role: SpeakerRole::Unknown,
            redacted: false,
        });
    }

    None
}

fn parse_question_speaker<'a>(
    question: &'a str,
    state: &mut ParserState,
    context: &TranscriptContext,
) -> (Option<Speaker>, &'a str) {
    let trimmed = question.trim();
    let Some(remainder) = strip_prefix_ascii_case(trimmed, "BY ") else {
        return (None, trimmed);
    };
    let Some(colon) = remainder.find(':') else {
        return (None, trimmed);
    };

    let label = remainder[..colon].trim();
    if label.is_empty() {
        return (None, trimmed);
    }

    let speaker = if is_redacted_label(label) {
        state.new_redacted_attorney()
    } else {
        resolve_known_speaker(label, context).unwrap_or_else(|| Speaker {
            participant_id: Some(super::speaker::label_id(label)),
            label: label.to_string(),
            role: SpeakerRole::Unknown,
            redacted: false,
        })
    };
    state.current_questioner = Some(speaker.clone());
    (Some(speaker), remainder[colon + 1..].trim())
}

fn strip_prefix_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let candidate = text.get(..prefix.len())?;
    if candidate.eq_ignore_ascii_case(prefix) {
        text.get(prefix.len()..)
    } else {
        None
    }
}

fn strip_witness_prefix(text: &str) -> &str {
    let text = text.trim();
    for prefix in ["THE WITNESS:", "BY THE WITNESS:"] {
        if let Some(candidate) = text.get(..prefix.len())
            && candidate.eq_ignore_ascii_case(prefix)
        {
            return text.get(prefix.len()..).unwrap_or("").trim();
        }
    }
    text
}

fn looks_like_objection(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    lower.starts_with("objection")
        || lower.starts_with("object ")
        || lower.starts_with("i object")
        || lower.contains(" object to ")
}

fn is_parenthetical_start(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with('(')
        || (trimmed.starts_with('[') && !is_pure_redaction_marker(trimmed))
        || trimmed.starts_with("---")
}

fn parenthetical_is_closed(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.starts_with("---") || (trimmed.ends_with(')') || trimmed.ends_with(']'))
}

fn is_exhibit_marker(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("exhibit")
        && (lower.contains("marked")
            || lower.contains("received")
            || lower.contains("identified")
            || lower.contains("admitted"))
}

fn is_examination_heading(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_uppercase().as_str(),
        "EXAMINATION"
            | "DIRECT EXAMINATION"
            | "CROSS-EXAMINATION"
            | "CROSS EXAMINATION"
            | "REDIRECT EXAMINATION"
            | "RE-DIRECT EXAMINATION"
            | "RECROSS EXAMINATION"
            | "RE-CROSS EXAMINATION"
            | "FURTHER EXAMINATION"
    )
}

fn is_general_heading(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_uppercase().as_str(),
        "APPEARANCES"
            | "INDEX"
            | "PROCEEDINGS"
            | "CERTIFICATE"
            | "CERTIFICATION"
            | "ERRATA"
            | "SIGNATURE"
            | "STIPULATIONS"
            | "CONTENTS"
    )
}

fn looks_like_generic_speaker_label(label: &str) -> bool {
    let trimmed = label.trim();
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if words.is_empty() || words.len() > 7 {
        return false;
    }
    if matches!(trimmed, "Q" | "A" | "QUESTION" | "ANSWER") {
        return false;
    }

    let letters: Vec<char> = trimmed.chars().filter(|c| c.is_alphabetic()).collect();
    if letters.len() < 2 {
        return false;
    }
    let uppercase_ratio =
        letters.iter().filter(|c| c.is_uppercase()).count() as f32 / letters.len() as f32;
    uppercase_ratio >= 0.80
}

fn speaker_only_dialogue_mode(context: &TranscriptContext) -> bool {
    let marker_observations: usize = context
        .marker_rules
        .iter()
        .map(|rule| rule.observations)
        .sum();
    context.format_fingerprint.speaker_prefixed_dialogue && marker_observations < 4
}

fn marker_evidence(marker: &MarkerMatch<'_>, kind: LineKind) -> ClassificationEvidence {
    let mut evidence = ClassificationEvidence::default();
    let weight = if marker.priority == 1 { 1.0 } else { 0.94 };
    score_kind(&mut evidence, &kind, weight);
    add_evidence(
        &mut evidence,
        EvidenceSource::ExplicitMarker,
        weight,
        &format!("Explicit marker {} detected.", marker.pattern),
    );
    evidence
}

fn explicit_structural_evidence(note: &str) -> ClassificationEvidence {
    let mut evidence = ClassificationEvidence::default();
    evidence.statement_score = 1.0;
    add_evidence(&mut evidence, EvidenceSource::FormatFingerprint, 1.0, note);
    evidence
}

fn score_kind(evidence: &mut ClassificationEvidence, kind: &LineKind, score: f32) {
    match kind {
        LineKind::Question => evidence.question_score = evidence.question_score.max(score),
        LineKind::Answer => evidence.answer_score = evidence.answer_score.max(score),
        LineKind::Objection => evidence.objection_score = evidence.objection_score.max(score),
        _ => evidence.statement_score = evidence.statement_score.max(score),
    }
}

fn add_evidence(
    evidence: &mut ClassificationEvidence,
    source: EvidenceSource,
    weight: f32,
    note: &str,
) {
    evidence.items.push(EvidenceItem {
        source,
        weight,
        note: note.to_string(),
    });
}

fn reconcile_dynamic_participants(context: &mut TranscriptContext, lines: &[TranscriptLine]) {
    #[derive(Default)]
    struct Counts {
        speaker: Option<Speaker>,
        observations: usize,
        questions: usize,
        answers: usize,
        objections: usize,
        statements: usize,
    }

    let existing_ids: BTreeSet<String> = context
        .participants
        .iter()
        .map(|participant| participant.id.clone())
        .collect();
    let mut dynamic: BTreeMap<String, Counts> = BTreeMap::new();

    for line in lines {
        let Some(speaker) = line.speaker.as_ref() else {
            continue;
        };
        let Some(id) = speaker.participant_id.as_ref() else {
            continue;
        };
        if existing_ids.contains(id) {
            continue;
        }

        let counts = dynamic.entry(id.clone()).or_default();
        counts.speaker.get_or_insert_with(|| speaker.clone());
        counts.observations += 1;
        match line.kind {
            LineKind::Question => counts.questions += 1,
            LineKind::Answer => counts.answers += 1,
            LineKind::Objection => counts.objections += 1,
            _ => counts.statements += 1,
        }
    }

    let document_type = context.document_type;

    for (id, counts) in dynamic {
        let Some(speaker) = counts.speaker else {
            continue;
        };
        context.participants.push(Participant {
            id,
            display_label: speaker.label.clone(),
            canonical_name: if speaker.redacted {
                None
            } else if speaker.label.starts_with("THE ") || speaker.label.starts_with("ANONYMOUS ") {
                None
            } else {
                Some(speaker.label.clone())
            },
            observed_labels: vec![speaker.label.clone()],
            role: speaker.role,
            role_confidence: if speaker.role == SpeakerRole::Unknown {
                0.40
            } else {
                0.90
            },
            function: match speaker.role {
                SpeakerRole::Witness => match document_type {
                    super::models::DocumentType::Interview => ParticipantFunction::Interviewee,
                    _ => ParticipantFunction::Witness,
                },
                SpeakerRole::Attorney => match document_type {
                    super::models::DocumentType::Interview if counts.questions > 0 => {
                        ParticipantFunction::Interviewer
                    }
                    _ => ParticipantFunction::Counsel,
                },
                SpeakerRole::Interpreter => ParticipantFunction::Interpreter,
                SpeakerRole::CourtReporter => ParticipantFunction::CourtReporter,
                SpeakerRole::Judge => ParticipantFunction::Judge,
                SpeakerRole::Videographer => ParticipantFunction::Videographer,
                SpeakerRole::Unknown => ParticipantFunction::Unknown,
            },
            function_confidence: if speaker.role == SpeakerRole::Unknown {
                0.30
            } else {
                0.78
            },
            redacted: speaker.redacted,
            observations: counts.observations,
            question_observations: counts.questions,
            answer_observations: counts.answers,
            objection_observations: counts.objections,
            statement_observations: counts.statements,
        });
    }

    for participant in &mut context.participants {
        let observations: Vec<_> = lines
            .iter()
            .filter(|l| {
                l.starts_new_block && l.participant_id.as_deref() == Some(participant.id.as_str())
            })
            .collect();
        participant.observations = observations.len();
        participant.question_observations = observations
            .iter()
            .filter(|l| l.kind == LineKind::Question)
            .count();
        participant.answer_observations = observations
            .iter()
            .filter(|l| l.kind == LineKind::Answer)
            .count();
        participant.objection_observations = observations
            .iter()
            .filter(|l| l.kind == LineKind::Objection)
            .count();
        participant.statement_observations = participant.observations
            - participant.question_observations
            - participant.answer_observations
            - participant.objection_observations;
    }

    if context.primary_questioner.is_none() {
        context.primary_questioner = context
            .participants
            .iter()
            .filter(|participant| participant.role == SpeakerRole::Attorney)
            .max_by_key(|participant| participant.question_observations)
            .map(participant_to_speaker);
    }
    if context.primary_witness.is_none() {
        context.primary_witness = context
            .participants
            .iter()
            .filter(|participant| participant.role == SpeakerRole::Witness)
            .max_by_key(|participant| participant.answer_observations)
            .map(participant_to_speaker);
    }
}

fn participant_to_speaker(participant: &Participant) -> Speaker {
    Speaker {
        participant_id: Some(participant.id.clone()),
        label: participant.display_label.clone(),
        role: participant.role,
        redacted: participant.redacted,
    }
}

fn calculate_parse_confidence(lines: &[TranscriptLine]) -> f32 {
    let testimony: Vec<&TranscriptLine> = lines
        .iter()
        .filter(|line| line.kind != LineKind::PageHeader)
        .collect();
    if testimony.is_empty() {
        return 0.0;
    }

    let score: f32 = testimony
        .iter()
        .map(|line| match line.verification {
            VerificationStatus::Verified => 1.0,
            VerificationStatus::VerifiedWithInference => 0.88,
            VerificationStatus::Redacted => 0.80,
            VerificationStatus::SourceMissing => 0.45,
            VerificationStatus::Uncertain => 0.25,
            VerificationStatus::Conflict => 0.0,
        })
        .sum();

    (score / testimony.len() as f32).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        document::{
            DocumentKind, DocumentMetadata, ExtractedDocument, ExtractedPage,
            PageLayoutDiagnostics, PageSection,
        },
        transcript::preflight::preflight_transcript,
    };

    fn document(pages: Vec<&str>) -> ExtractedDocument {
        ExtractedDocument {
            kind: DocumentKind::Pdf,
            metadata: DocumentMetadata::default(),
            pages: pages
                .into_iter()
                .enumerate()
                .map(|(index, text)| ExtractedPage {
                    physical_page: (index + 1) as u32,
                    text: text.to_string(),
                    raw_text: text.to_string(),
                    section: PageSection::Transcript,
                    section_confidence: 1.0,
                    section_reasons: vec!["Test fixture".to_string()],
                    visual_rows: Vec::new(),
                    layout: PageLayoutDiagnostics {
                        reconstruction_confidence: 1.0,
                        ..PageLayoutDiagnostics::default()
                    },
                    annotations: Vec::new(),
                    filled_rectangles: Vec::new(),
                    image_count: 0,
                })
                .collect(),
        }
    }

    fn parse(doc: &ExtractedDocument) -> ParsedTranscript {
        let profile = TranscriptProfile::us_english();
        let context = preflight_transcript(doc, &profile);
        parse_transcript("sample.pdf".to_string(), doc, &context, &profile).unwrap()
    }

    #[test]
    fn preserves_cross_page_question() {
        let doc = document(vec![
            "1 00012\n21 A. Yes.\n22 Q. What is the purpose of the",
            "2 00013\n1 executive committee?\n2 A. Strategy.",
        ]);
        let result = parse(&doc);
        let question = result
            .blocks
            .iter()
            .find(|block| block.kind == LineKind::Question && block.text.contains("purpose"))
            .unwrap();
        assert_eq!(
            question.text,
            "What is the purpose of the executive committee?"
        );
    }

    #[test]
    fn bare_a_is_uppercase_only() {
        let doc = document(vec![
            "1 00048\n10 Q. Does the competitor have\n11 a product that works?\n12 A     Yes.",
        ]);
        let result = parse(&doc);
        assert_eq!(
            result.exchanges[0].question.text,
            "Does the competitor have a product that works?"
        );
    }

    #[test]
    fn retains_unknown_text_after_source_gap() {
        let doc = document(vec![
            "1 00048\n5 A. Prior answer.\n6 Still answer.\n8 But in these circumstances, does the competitor\n9 have a product that can meet the customer's needs?\n10 A. That's one factor.",
        ]);
        let result = parse(&doc);
        assert!(
            result
                .lines
                .iter()
                .any(|l| l.kind == LineKind::Unknown && l.text.starts_with("But in"))
        );
        assert!(!result.quality.ready_for_ai);
    }

    #[test]
    fn recovers_cross_page_question_tail_without_marker() {
        let doc = document(vec![
            "1 00103\n21 Q. If somebody called and\n22 wanted to buy it,",
            "2 00104\n1 you're not going to turn away money.\n2 A. Absolutely not.",
        ]);
        let result = parse(&doc);
        assert!(result.blocks.iter().any(|block| {
            block.kind == LineKind::Question && block.text.contains("turn away money")
        }));
    }

    #[test]
    fn redacted_examiner_gets_stable_anonymous_id() {
        let doc = document(vec![
            "1 00010\n1 BY [REDACTED]:\n2 Q. Where were you?\n3 A. At home.\n4 [REDACTED]: Q. Who was there?\n5 A. Nobody.",
        ]);
        let result = parse(&doc);
        let questions: Vec<&TranscriptBlock> = result
            .blocks
            .iter()
            .filter(|block| block.kind == LineKind::Question)
            .collect();
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0].participant_id, questions[1].participant_id);
    }
    #[test]
    fn mixed_period_labels_and_qa_share_participant_identity() {
        let result = parse(&document(vec![
            "PAGE 1\n1 Mr. Smith. Opening statement.\n2 Ms. Jones. I understand.\n3 EXAMINATION\n4 BY MR. SMITH:\n5 Q. Did it happen?\n6 A. Yes.\n7 Mr. Smith. Thank you.",
        ]));
        assert!(
            result
                .context
                .format_fingerprint
                .titled_speaker_prefixed_dialogue
        );
        let statement = result
            .blocks
            .iter()
            .find(|b| b.text == "Opening statement.")
            .unwrap();
        let question = result
            .blocks
            .iter()
            .find(|b| b.kind == LineKind::Question)
            .unwrap();
        assert_eq!(statement.participant_id, question.participant_id);
        assert_eq!(result.omni.utterances.len(), result.blocks.len());
        for (u, b) in result.omni.utterances.iter().zip(&result.blocks) {
            assert_eq!(u.text, b.text);
            assert_eq!(u.participant_id, b.participant_id);
        }
    }

    #[test]
    fn retains_unnumbered_wrapped_text_and_body_numbers() {
        let result = parse(&document(vec![
            "ALICE SMITH: I counted\n12 jurors in the room.\nBOB JONES: I saw\nall of them.",
        ]));
        assert_eq!(result.blocks.len(), 2);
        assert_eq!(result.blocks[0].text, "I counted 12 jurors in the room.");
        assert_eq!(result.blocks[1].text, "I saw all of them.");
        assert_eq!(
            result.quality.source_rows,
            result.quality.accounted_source_rows
        );
    }

    #[test]
    fn blank_printed_lines_are_not_missing_testimony() {
        let result = parse(&document(vec![
            "PAGE 3\n1 BY MR. SMITH:\n2 Q. Are you sure?\n3\n4 A. Yes.",
        ]));
        assert!(
            !result
                .context
                .gaps
                .iter()
                .any(|g| g.kind == super::super::models::GapKind::MissingLines)
        );
        assert!(
            result
                .omni
                .canonical_rows
                .iter()
                .any(|r| r.disposition
                    == super::super::omni_models::RowDisposition::BlankNumberedLine)
        );
        assert_eq!(result.line_count, 3);
    }

    #[test]
    fn source_gap_breaks_question_answer_link() {
        let result = parse(&document(vec![
            "PAGE 1\n1 BY MR. SMITH:\n2 Q. Where?\n4 A. At home.",
        ]));
        assert_eq!(result.unpaired_answers.len(), 1);
        assert!(result.exchanges[0].response_sequence.is_empty());
        assert!(!result.quality.ready_for_ai);
    }

    #[test]
    fn repeated_testimony_is_never_deleted_as_boilerplate() {
        let pages = vec![
            "PAGE 1\n1 Q. What?\n2 A. I don't recall.",
            "PAGE 2\n1 Q. What?\n2 A. I don't recall.",
            "PAGE 3\n1 Q. What?\n2 A. I don't recall.",
            "PAGE 4\n1 Q. What?\n2 A. I don't recall.",
            "PAGE 5\n1 Q. What?\n2 A. I don't recall.",
        ];
        let result = parse(&document(pages));
        assert_eq!(
            result
                .blocks
                .iter()
                .filter(|b| b.text == "I don't recall.")
                .count(),
            5
        );
        assert_eq!(
            result
                .omni
                .utterances
                .iter()
                .filter(|u| u.text == "I don't recall.")
                .count(),
            5
        );
    }

    #[test]
    fn explicit_witness_changes_do_not_reuse_previous_witness() {
        let result = parse(&document(vec![
            "PAGE 1\n1 WITNESS: JANE DOE\n2 BY MR. SMITH:\n3 Q. Name?\n4 A. Jane.\n5 WITNESS: SAM RAY\n6 BY MR. SMITH:\n7 Q. Name?\n8 A. Sam.",
        ]));
        let answers: Vec<_> = result
            .blocks
            .iter()
            .filter(|b| b.kind == LineKind::Answer)
            .collect();
        assert_eq!(answers.len(), 2);
        assert_ne!(answers[0].participant_id, answers[1].participant_id);
        assert_eq!(answers[0].speaker.as_ref().unwrap().label, "JANE DOE");
        assert_eq!(answers[1].speaker.as_ref().unwrap().label, "SAM RAY");
    }

    #[test]
    fn number_is_stripped_only_once() {
        let result = parse(&document(vec![
            "PAGE 1\n1 Q. How many?\n2 A. I saw\n3 12 jurors and 25 witnesses.",
        ]));
        let answer = result
            .blocks
            .iter()
            .find(|b| b.kind == LineKind::Answer)
            .unwrap();
        assert_eq!(answer.text, "I saw 12 jurors and 25 witnesses.");
        assert!(
            result
                .omni
                .canonical_rows
                .iter()
                .any(|r| r.text == "12 jurors and 25 witnesses.")
        );
    }

    #[test]
    fn redacted_words_inside_testimony_do_not_erase_the_turn() {
        let result = parse(&document(vec![
            "PAGE 1\n1 Q. Who?\n2 A. I met [REDACTED] in June.\n3 We spoke for an hour.",
        ]));
        let answer = result
            .blocks
            .iter()
            .find(|b| b.kind == LineKind::Answer)
            .unwrap();
        assert!(answer.text.contains("[REDACTED] in June. We spoke"));
    }

    #[test]
    fn reference_pages_remain_in_canonical_inventory() {
        let mut doc = document(vec![
            "PAGE 1\n1 Q. Which exhibit?\n2 A. Seven.",
            "INDEX OF EXHIBITS\nExhibit 7 1:2",
        ]);
        doc.pages[1].section = PageSection::ExhibitIndex;
        let result = parse(&doc);
        assert_eq!(result.source_pages.len(), 2);
        assert!(
            result
                .omni
                .canonical_rows
                .iter()
                .any(|r| r.physical_page == 2
                    && r.disposition == super::super::omni_models::RowDisposition::Reference)
        );
        assert!(!result.lines.iter().any(|r| r.physical_page == 2));
    }
}
