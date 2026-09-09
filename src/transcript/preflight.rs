use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::document::{ExtractedDocument, PageSection};

use super::{
    format::{
        detect_transcript_page_label, is_page_header_content, parse_numbered_line,
        parse_source_line,
    },
    markers::{default_marker_rules, detect_marker_candidate},
    models::{
        DocumentLayoutSummary, DocumentType, FormatFingerprint, GapKind, GapReason, LanguageHint,
        MarkerKind, MarkerRule, MarkerStyle, PageContext, Participant, ParticipantFunction,
        RedactionCandidate, RedactionSignalKind, RedactionSummary, SignalConfidence, Speaker,
        SpeakerRole, TranscriptContext, TranscriptGap,
    },
    profile::TranscriptProfile,
    speaker::{
        contains_redaction_marker, is_redacted_label, looks_like_bare_attorney_heading,
        speaker_from_label, split_titled_speaker_prefix,
    },
};

#[derive(Default)]
struct ParticipantCounter {
    first_seen: usize,
    observed_labels: BTreeSet<String>,
    observations: usize,
    question_observations: usize,
    answer_observations: usize,
    objection_observations: usize,
    statement_observations: usize,
    attorney_votes: usize,
    witness_votes: usize,
    interpreter_votes: usize,
    reporter_votes: usize,
    judge_votes: usize,
    videographer_votes: usize,
}

pub fn preflight_transcript(
    document: &ExtractedDocument,
    profile: &TranscriptProfile,
) -> TranscriptContext {
    let page_contexts = build_page_contexts(document, profile);
    let layout_summary = summarize_layout(document);
    let mut marker_rules = build_marker_rules(document, profile);
    marker_rules.sort_by_key(|rule| (rule.priority, std::cmp::Reverse(rule.observations)));

    let mut participants = discover_participants(document, profile, &page_contexts);
    let language = detect_language(document);
    let document_type = detect_document_type(document, &marker_rules, &participants);
    assign_participant_functions(document, document_type, &mut participants);
    let primary_questioner = pick_primary_questioner(&participants);
    let primary_witness = pick_primary_witness(&participants, &marker_rules);
    let (typical_line_min, typical_line_max) = learn_line_range(document, profile, &page_contexts);
    let gaps = detect_gaps(
        document,
        profile,
        &page_contexts,
        typical_line_min,
        typical_line_max,
    );
    let redaction_summary = summarize_redaction_signals(document);
    let redaction_candidates = collect_redaction_candidates(document);
    let format_fingerprint = build_format_fingerprint(
        document,
        &page_contexts,
        &marker_rules,
        typical_line_min,
        typical_line_max,
    );

    let participant_confidence = participant_confidence(&participants, &marker_rules);
    let transcript_context_pages = page_contexts
        .iter()
        .filter(|page| page.section.is_dialogue())
        .count();
    let page_confidence = if transcript_context_pages == 0 {
        0.0
    } else {
        page_contexts
            .iter()
            .filter(|page| page.section.is_dialogue() && page.transcript_page.is_some())
            .count() as f32
            / transcript_context_pages as f32
    };
    let marker_confidence = marker_confidence(&marker_rules, &format_fingerprint);

    let format_confidence =
        (participant_confidence * 0.30 + page_confidence * 0.30 + marker_confidence * 0.40)
            .clamp(0.0, 1.0);
    let source_completeness = calculate_source_completeness(
        document,
        profile,
        &page_contexts,
        typical_line_min,
        typical_line_max,
    );

    let ai_router_recommended = format_confidence < 0.78
        || language.code == "und"
        || matches!(document_type, DocumentType::Unknown)
        || layout_summary.ocr_required_pages > 0
        || layout_summary.unknown_pages > 3
        || redaction_summary.requires_visual_confirmation;

    TranscriptContext {
        document_type,
        profile_name: profile.name.to_string(),
        metadata: document.metadata.clone(),
        language,
        participants,
        primary_questioner,
        primary_witness,
        marker_rules,
        format_fingerprint,
        layout_summary,
        typical_line_min,
        typical_line_max,
        page_contexts,
        gaps,
        redaction_summary,
        redaction_candidates,
        ai_router_recommended,
        format_confidence,
        source_completeness,
    }
}

fn build_page_contexts(
    document: &ExtractedDocument,
    profile: &TranscriptProfile,
) -> Vec<PageContext> {
    let mut previous_numeric_page = None;
    let mut contexts = Vec::with_capacity(document.pages.len());

    for page in &document.pages {
        let transcript_page = if page.section.is_reference_only() {
            None
        } else {
            detect_transcript_page_label(page, profile, previous_numeric_page)
        };

        if page.section.is_transcript_family() || page.section == PageSection::Unknown {
            if let Some(number) = transcript_page
                .as_deref()
                .and_then(|page| page.parse::<u32>().ok())
            {
                previous_numeric_page = Some(number);
            }
        }

        contexts.push(PageContext {
            physical_page: page.physical_page,
            transcript_page,
            section: page.section,
            section_confidence: page.section_confidence,
            layout_reconstruction_confidence: page.layout.reconstruction_confidence,
            requires_ocr: page.layout.requires_ocr,
        });
    }

    contexts
}

fn summarize_layout(document: &ExtractedDocument) -> DocumentLayoutSummary {
    let mut summary = DocumentLayoutSummary::default();
    let mut line_columns = Vec::new();

    for page in &document.pages {
        if page.layout.positioned_text_available {
            summary.positioned_text_pages += 1;
        }
        if page.layout.used_positioned_reconstruction {
            summary.positioned_reconstructed_pages += 1;
        }
        if page.layout.requires_ocr {
            summary.ocr_required_pages += 1;
        }
        summary.masked_redaction_fragments += page.layout.masked_redaction_fragments;
        if let Some(column) = page.layout.likely_line_number_column_x {
            if page.section.is_dialogue() {
                line_columns.push(column);
            }
        }

        match page.section {
            PageSection::Cover => summary.cover_pages += 1,
            PageSection::Caption => summary.caption_pages += 1,
            PageSection::Appearances => summary.appearances_pages += 1,
            PageSection::Transcript => summary.transcript_pages += 1,
            PageSection::WordIndex => summary.word_index_pages += 1,
            PageSection::ExhibitIndex => summary.exhibit_index_pages += 1,
            PageSection::Certificate => summary.certificate_pages += 1,
            PageSection::Errata => summary.errata_pages += 1,
            PageSection::Appendix => summary.appendix_pages += 1,
            PageSection::Attachment => summary.attachment_pages += 1,
            PageSection::Unknown => summary.unknown_pages += 1,
        }

        if page.section.is_reference_only() {
            summary.reference_only_pages += 1;
        }
    }

    if !line_columns.is_empty() {
        line_columns
            .sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
        summary.likely_line_number_column_x = Some(line_columns[line_columns.len() / 2]);
    }

    summary
}

fn assign_participant_functions(
    document: &ExtractedDocument,
    document_type: DocumentType,
    participants: &mut [Participant],
) {
    let subject = discover_named_subject(document);
    let front_matter = document
        .pages
        .iter()
        .filter(|page| page.section.is_transcript_family())
        .take(12)
        .map(|page| page.text.to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join("\n");

    let top_questioner_id = participants
        .iter()
        .max_by_key(|participant| participant.question_observations)
        .filter(|participant| participant.question_observations > 0)
        .map(|participant| participant.id.clone());
    let top_answerer_id = participants
        .iter()
        .max_by_key(|participant| participant.answer_observations)
        .filter(|participant| participant.answer_observations > 0)
        .map(|participant| participant.id.clone());

    for participant in participants {
        let label_upper = participant.display_label.to_ascii_uppercase();
        let is_subject = subject
            .as_ref()
            .is_some_and(|subject| labels_probably_match(subject, &label_upper));
        let front_mentions_counsel = front_matter_mentions_role(
            &front_matter,
            &label_upper,
            &["COUNSEL", "ATTORNEY", "ESQ"],
        );
        let front_mentions_law_enforcement = front_matter_mentions_role(
            &front_matter,
            &label_upper,
            &["FBI", "SPECIAL AGENT", "MARSHAL", "LAW ENFORCEMENT"],
        );
        let front_mentions_government_counsel = front_matter_mentions_role(
            &front_matter,
            &label_upper,
            &["ATTORNEY GENERAL", "DEPARTMENT OF JUSTICE", "U.S. ATTORNEY"],
        );

        if is_subject {
            participant.role = SpeakerRole::Witness;
            participant.role_confidence = 0.98;
        } else if participant.role == SpeakerRole::Unknown
            && (front_mentions_counsel || front_mentions_government_counsel)
        {
            participant.role = SpeakerRole::Attorney;
            participant.role_confidence = 0.95;
        }
        let (function, confidence) = if is_subject {
            match document_type {
                DocumentType::Interview => (ParticipantFunction::Interviewee, 1.0),
                DocumentType::Deposition | DocumentType::Examination => {
                    (ParticipantFunction::Witness, 1.0)
                }
                _ => (ParticipantFunction::Witness, 0.90),
            }
        } else if participant.role == SpeakerRole::Interpreter {
            (ParticipantFunction::Interpreter, 1.0)
        } else if participant.role == SpeakerRole::CourtReporter {
            (ParticipantFunction::CourtReporter, 1.0)
        } else if participant.role == SpeakerRole::Judge {
            (ParticipantFunction::Judge, 1.0)
        } else if participant.role == SpeakerRole::Videographer {
            (ParticipantFunction::Videographer, 1.0)
        } else if front_mentions_law_enforcement {
            (ParticipantFunction::LawEnforcement, 0.95)
        } else if top_questioner_id.as_deref() == Some(participant.id.as_str())
            && matches!(document_type, DocumentType::Interview)
        {
            (ParticipantFunction::Interviewer, 0.95)
        } else if front_mentions_government_counsel {
            (ParticipantFunction::GovernmentCounsel, 0.92)
        } else if front_mentions_counsel {
            if matches!(document_type, DocumentType::Interview)
                && participant.question_observations > participant.answer_observations
                && top_questioner_id.as_deref() == Some(participant.id.as_str())
            {
                (ParticipantFunction::Interviewer, 0.90)
            } else {
                (ParticipantFunction::Counsel, 0.92)
            }
        } else if participant.role == SpeakerRole::Attorney {
            match document_type {
                DocumentType::Interview
                    if top_questioner_id.as_deref() == Some(participant.id.as_str()) =>
                {
                    (ParticipantFunction::Interviewer, 0.88)
                }
                _ => (ParticipantFunction::Counsel, 0.78),
            }
        } else if participant.role == SpeakerRole::Witness
            || top_answerer_id.as_deref() == Some(participant.id.as_str())
        {
            match document_type {
                DocumentType::Interview => (ParticipantFunction::Interviewee, 0.88),
                _ => (ParticipantFunction::Witness, 0.88),
            }
        } else {
            (ParticipantFunction::Unknown, 0.30)
        };

        participant.function = function;
        participant.function_confidence = confidence;
        if !participant.redacted {
            if is_subject {
                participant.canonical_name = subject.clone();
            } else if let Some(front_name) =
                canonical_name_from_front_matter(document, &participant.display_label)
            {
                participant.canonical_name = Some(front_name);
            } else if participant.canonical_name.is_none()
                && function != ParticipantFunction::Unknown
            {
                participant.canonical_name = Some(participant.display_label.clone());
            }
        }
    }
}

fn canonical_name_from_front_matter(
    document: &ExtractedDocument,
    observed_label: &str,
) -> Option<String> {
    let normalized_label = observed_label
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .filter(|token| {
            !matches!(
                token.to_ascii_uppercase().as_str(),
                "MR" | "MS" | "MRS" | "MISS" | "DR" | "HON" | "CHAIRMAN"
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let last_name = normalized_label
        .split_whitespace()
        .last()?
        .to_ascii_uppercase();
    if last_name.len() < 3 {
        return None;
    }

    for page in document.pages.iter().take(16) {
        if !page.section.is_transcript_family() && page.section != PageSection::Unknown {
            continue;
        }
        for raw_line in page.text.lines() {
            let mut line = raw_line.trim();
            let digit_count = line.chars().take_while(|c| c.is_ascii_digit()).count();
            if digit_count > 0 {
                line = line.get(digit_count..).unwrap_or(line).trim_start();
            }
            if split_any_speaker_prefix(line).is_some() {
                continue;
            }
            let before_comma = line.split(',').next().unwrap_or(line).trim();
            if before_comma.chars().any(char::is_lowercase) {
                continue;
            }
            if !before_comma.to_ascii_uppercase().contains(&last_name) {
                continue;
            }
            let words = before_comma.split_whitespace().collect::<Vec<_>>();
            if (2..=5).contains(&words.len())
                && words.last().is_some_and(|word| {
                    word.trim_matches(|c: char| !c.is_alphanumeric())
                        .eq_ignore_ascii_case(&last_name)
                })
            {
                return Some(before_comma.to_string());
            }
        }
    }
    None
}

fn discover_named_subject(document: &ExtractedDocument) -> Option<String> {
    for page in document.pages.iter().take(12) {
        if !page.section.is_transcript_family() && page.section != PageSection::Unknown {
            continue;
        }
        for raw_line in page.text.lines() {
            let line = raw_line.trim();
            let upper = line.to_ascii_uppercase();
            for prefix in [
                "INTERVIEW OF:",
                "INTERVIEW OF",
                "DEPOSITION OF:",
                "DEPOSITION OF",
                "WITNESS:",
            ] {
                if let Some(index) = upper.find(prefix) {
                    let candidate = line[index + prefix.len()..].trim().trim_matches(':').trim();
                    if candidate.len() >= 2 {
                        return Some(canonicalize_label(candidate));
                    }
                }
            }
        }
    }
    None
}

fn labels_probably_match(left: &str, right: &str) -> bool {
    let normalize = |value: &str| {
        value
            .chars()
            .map(|character| {
                if character.is_alphanumeric() || character.is_whitespace() {
                    character
                } else {
                    ' '
                }
            })
            .collect::<String>()
            .split_whitespace()
            .filter(|token| {
                !matches!(
                    token.to_ascii_uppercase().as_str(),
                    "MR" | "MS" | "MRS" | "MISS" | "DR" | "HON" | "THE" | "WITNESS"
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
            .to_ascii_uppercase()
    };
    let left = normalize(left);
    let right = normalize(right);
    if left.is_empty() || right.is_empty() {
        return false;
    }
    left == right || left.contains(&right) || right.contains(&left)
}

fn front_matter_mentions_role(front_matter: &str, label: &str, role_terms: &[&str]) -> bool {
    let surname = label
        .split_whitespace()
        .last()
        .unwrap_or(label)
        .trim_matches('.');
    if surname.len() < 2 {
        return false;
    }
    // A nearby person's title is not evidence about this participant.
    front_matter.lines().any(|line| {
        let Some((name, role)) = line.split_once(',') else {
            return false;
        };
        name.split_whitespace()
            .last()
            .is_some_and(|word| word.trim_matches('.').eq_ignore_ascii_case(surname))
            && role_terms.iter().any(|term| role.contains(term))
    })
}

fn build_marker_rules(
    document: &ExtractedDocument,
    profile: &TranscriptProfile,
) -> Vec<MarkerRule> {
    let mut rules = default_marker_rules();

    for marker in profile.word_question_markers {
        ensure_rule(
            &mut rules,
            MarkerKind::Question,
            MarkerStyle::Word,
            marker,
            4,
        );
    }
    for marker in profile.word_answer_markers {
        ensure_rule(&mut rules, MarkerKind::Answer, MarkerStyle::Word, marker, 4);
    }

    for page in &document.pages {
        if !page.section.is_dialogue() {
            continue;
        }
        for raw_line in page.text.lines() {
            let (_, content) = parse_source_line(page, raw_line.trim(), profile.max_line_number);
            let content = content.trim();

            if split_any_speaker_prefix(content).is_some() {
                // A speaker-labelled utterance such as "DAVID MARKUS: A what?" must not teach
                // the document that bare A is an answer marker. Q/A conventions are learned only
                // when the marker occupies the structural row start.
                continue;
            }

            if let Some((kind, style, pattern)) = detect_marker_candidate(content) {
                increment_rule(&mut rules, kind, style, pattern);
            }
        }
    }

    rules
}

fn ensure_rule(
    rules: &mut Vec<MarkerRule>,
    kind: MarkerKind,
    style: MarkerStyle,
    pattern: &str,
    priority: u8,
) {
    if rules.iter().any(|rule| {
        rule.kind == kind && rule.style == style && rule.pattern.eq_ignore_ascii_case(pattern)
    }) {
        return;
    }

    rules.push(MarkerRule {
        kind,
        style,
        pattern: pattern.to_string(),
        priority,
        observations: 0,
    });
}

fn increment_rule(
    rules: &mut Vec<MarkerRule>,
    kind: MarkerKind,
    style: MarkerStyle,
    pattern: &str,
) {
    if let Some(rule) = rules.iter_mut().find(|rule| {
        rule.kind == kind && rule.style == style && rule.pattern.eq_ignore_ascii_case(pattern)
    }) {
        rule.observations += 1;
        return;
    }

    rules.push(MarkerRule {
        kind,
        style,
        pattern: pattern.to_string(),
        priority: priority_for_style(style),
        observations: 1,
    });
}

fn priority_for_style(style: MarkerStyle) -> u8 {
    match style {
        MarkerStyle::Dot => 1,
        MarkerStyle::BareUppercase => 2,
        MarkerStyle::Colon => 3,
        MarkerStyle::Word => 4,
        MarkerStyle::SpeakerPrefixed => 5,
    }
}

fn discover_participants(
    document: &ExtractedDocument,
    profile: &TranscriptProfile,
    page_contexts: &[PageContext],
) -> Vec<Participant> {
    let mut counters: BTreeMap<String, ParticipantCounter> = BTreeMap::new();
    let mut observation_index = 0usize;
    let mut last_unmarked_question_label: Option<String> = None;
    let allow_titled_speaker_style = titled_speaker_style_likely(document);

    for page in &document.pages {
        if !page.section.is_dialogue() {
            continue;
        }
        let transcript_page = page_contexts
            .iter()
            .find(|context| context.physical_page == page.physical_page)
            .and_then(|context| context.transcript_page.as_deref());

        for (logical_index, raw_line) in page.text.lines().enumerate() {
            let (_, content) = parse_source_line(page, raw_line.trim(), profile.max_line_number);
            let content = content.trim();

            if content.is_empty() || is_page_header_content(content, transcript_page, logical_index)
            {
                continue;
            }

            observation_index += 1;
            if let Some(label) = super::speaker::witness_introduction(content) {
                observe_participant(
                    &mut counters,
                    label,
                    SpeakerRole::Witness,
                    observation_index,
                    ObservationType::Statement,
                );
                continue;
            }

            if let Some(label) = by_heading_label(content) {
                if !is_redacted_label(label) {
                    observe_participant(
                        &mut counters,
                        label,
                        speaker_from_label(label)
                            .map(|s| s.role)
                            .unwrap_or(SpeakerRole::Unknown),
                        observation_index,
                        ObservationType::Question,
                    );
                }
                continue;
            }

            let speaker_prefix = if allow_titled_speaker_style {
                split_any_speaker_prefix(content)
            } else {
                split_colon_speaker_prefix(content)
            };
            let Some((label, remainder)) = speaker_prefix else {
                continue;
            };

            // A generic redaction token may stand for several different people. Do not collapse
            // all of them into one participant during document-wide preflight.
            if is_redacted_label(label) {
                continue;
            }

            let explicit_role = speaker_from_label(label).map(|speaker| speaker.role);
            let marker = detect_marker_candidate(remainder);
            let canonical_label = canonicalize_label(label);
            let punctuation_question = marker.is_none() && remainder.trim_end().ends_with('?');
            let follows_unmarked_question = marker.is_none()
                && explicit_role.is_none()
                && last_unmarked_question_label
                    .as_ref()
                    .is_some_and(|previous| previous != &canonical_label);

            let inferred_role = match marker.map(|(kind, _, _)| kind) {
                Some(MarkerKind::Question) => Some(SpeakerRole::Attorney),
                Some(MarkerKind::Answer) => Some(SpeakerRole::Witness),
                None if looks_like_objection(remainder) => Some(SpeakerRole::Attorney),

                None => explicit_role,
            };

            let role = inferred_role.unwrap_or(SpeakerRole::Unknown);
            let observation = match marker.map(|(kind, _, _)| kind) {
                Some(MarkerKind::Question) => ObservationType::Question,
                Some(MarkerKind::Answer) => ObservationType::Answer,
                None if looks_like_objection(remainder) => ObservationType::Objection,
                None if punctuation_question => ObservationType::Question,
                None if follows_unmarked_question => ObservationType::Answer,
                None => ObservationType::Statement,
            };

            observe_participant(&mut counters, label, role, observation_index, observation);

            if matches!(observation, ObservationType::Question) && marker.is_none() {
                last_unmarked_question_label = Some(canonical_label);
            } else if matches!(observation, ObservationType::Answer) {
                last_unmarked_question_label = None;
            }
        }
    }

    let mut raw: Vec<(String, ParticipantCounter)> = counters.into_iter().collect();
    raw.sort_by_key(|(_, counter)| counter.first_seen);

    let mut role_sequences: HashMap<SpeakerRole, usize> = HashMap::new();
    raw.into_iter()
        .map(|(canonical, counter)| {
            let (role, role_confidence) = decide_role(&counter);
            let sequence = role_sequences.entry(role).or_default();
            *sequence += 1;
            let id = participant_id(role, *sequence);
            let display_label = counter
                .observed_labels
                .iter()
                .next()
                .cloned()
                .unwrap_or_else(|| canonical.clone());

            Participant {
                id,
                display_label: display_label.clone(),
                canonical_name: canonical_name_for_label(&display_label, role),
                observed_labels: counter.observed_labels.into_iter().collect(),
                role,
                role_confidence,
                function: ParticipantFunction::Unknown,
                function_confidence: 0.0,
                redacted: false,
                observations: counter.observations,
                question_observations: counter.question_observations,
                answer_observations: counter.answer_observations,
                objection_observations: counter.objection_observations,
                statement_observations: counter.statement_observations,
            }
        })
        .collect()
}

#[derive(Clone, Copy)]
enum ObservationType {
    Question,
    Answer,
    Objection,
    Statement,
}

fn observe_participant(
    counters: &mut BTreeMap<String, ParticipantCounter>,
    label: &str,
    role: SpeakerRole,
    first_seen: usize,
    observation: ObservationType,
) {
    let canonical = canonicalize_label(label);
    if canonical.is_empty() {
        return;
    }

    let counter = counters
        .entry(canonical)
        .or_insert_with(|| ParticipantCounter {
            first_seen,
            ..ParticipantCounter::default()
        });
    counter.observed_labels.insert(label.trim().to_string());
    counter.observations += 1;

    match observation {
        ObservationType::Question => counter.question_observations += 1,
        ObservationType::Answer => counter.answer_observations += 1,
        ObservationType::Objection => counter.objection_observations += 1,
        ObservationType::Statement => counter.statement_observations += 1,
    }

    match role {
        SpeakerRole::Attorney => counter.attorney_votes += 1,
        SpeakerRole::Witness => counter.witness_votes += 1,
        SpeakerRole::Interpreter => counter.interpreter_votes += 1,
        SpeakerRole::CourtReporter => counter.reporter_votes += 1,
        SpeakerRole::Judge => counter.judge_votes += 1,
        SpeakerRole::Videographer => counter.videographer_votes += 1,
        SpeakerRole::Unknown => {}
    }
}

fn decide_role(counter: &ParticipantCounter) -> (SpeakerRole, f32) {
    let roles = [
        (SpeakerRole::Attorney, counter.attorney_votes),
        (SpeakerRole::Witness, counter.witness_votes),
        (SpeakerRole::Interpreter, counter.interpreter_votes),
        (SpeakerRole::CourtReporter, counter.reporter_votes),
        (SpeakerRole::Judge, counter.judge_votes),
        (SpeakerRole::Videographer, counter.videographer_votes),
    ];

    let total_votes: usize = roles.iter().map(|(_, votes)| *votes).sum();
    let (role, votes) = roles
        .into_iter()
        .max_by_key(|(_, votes)| *votes)
        .unwrap_or((SpeakerRole::Unknown, 0));

    if votes == 0 {
        (SpeakerRole::Unknown, 0.30)
    } else {
        (
            role,
            (votes as f32 / total_votes.max(1) as f32).clamp(0.0, 1.0),
        )
    }
}

fn participant_id(role: SpeakerRole, sequence: usize) -> String {
    let prefix = match role {
        SpeakerRole::Attorney => "attorney",
        SpeakerRole::Witness => "witness",
        SpeakerRole::Interpreter => "interpreter",
        SpeakerRole::CourtReporter => "reporter",
        SpeakerRole::Judge => "judge",
        SpeakerRole::Videographer => "videographer",
        SpeakerRole::Unknown => "speaker",
    };
    format!("{prefix}_{sequence}")
}

fn canonicalize_label(label: &str) -> String {
    label
        .trim()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_uppercase()
}

fn canonical_name_for_label(label: &str, role: SpeakerRole) -> Option<String> {
    if is_redacted_label(label)
        || matches!(
            label.trim().to_ascii_uppercase().as_str(),
            "THE WITNESS"
                | "WITNESS"
                | "THE DEPONENT"
                | "DEPONENT"
                | "THE REPORTER"
                | "REPORTER"
                | "THE INTERPRETER"
                | "INTERPRETER"
                | "THE COURT"
        )
    {
        return None;
    }

    if role == SpeakerRole::Unknown {
        None
    } else {
        Some(label.trim().to_string())
    }
}

fn pick_primary_questioner(participants: &[Participant]) -> Option<Speaker> {
    let participant = participants
        .iter()
        .filter(|participant| participant.role == SpeakerRole::Attorney)
        .max_by_key(|participant| (participant.question_observations, participant.observations))?;

    if participant.question_observations == 0
        && participants
            .iter()
            .filter(|person| person.role == SpeakerRole::Attorney)
            .count()
            > 1
    {
        return None;
    }

    Some(speaker_from_participant(participant))
}

fn pick_primary_witness(
    participants: &[Participant],
    marker_rules: &[MarkerRule],
) -> Option<Speaker> {
    if let Some(participant) = participants
        .iter()
        .filter(|participant| {
            participant.role == SpeakerRole::Witness
                || matches!(
                    participant.function,
                    ParticipantFunction::Witness | ParticipantFunction::Interviewee
                )
        })
        .max_by_key(|participant| (participant.answer_observations, participant.observations))
    {
        let mut speaker = speaker_from_participant(participant);
        speaker.role = SpeakerRole::Witness;
        return Some(speaker);
    }

    let answer_count: usize = marker_rules
        .iter()
        .filter(|rule| rule.kind == MarkerKind::Answer)
        .map(|rule| rule.observations)
        .sum();

    if answer_count > 0 {
        Some(Speaker {
            participant_id: Some("witness_1".to_string()),
            label: "THE WITNESS".to_string(),
            role: SpeakerRole::Witness,
            redacted: false,
        })
    } else {
        None
    }
}

fn speaker_from_participant(participant: &Participant) -> Speaker {
    Speaker {
        participant_id: Some(participant.id.clone()),
        label: participant.display_label.clone(),
        role: participant.role,
        redacted: participant.redacted,
    }
}

fn learn_line_range(
    document: &ExtractedDocument,
    profile: &TranscriptProfile,
    page_contexts: &[PageContext],
) -> (Option<u16>, Option<u16>) {
    let mut min_frequency: HashMap<u16, usize> = HashMap::new();
    let mut max_frequency: HashMap<u16, usize> = HashMap::new();

    for page in &document.pages {
        if !page.section.is_dialogue() {
            continue;
        }
        let transcript_page = page_contexts
            .iter()
            .find(|context| context.physical_page == page.physical_page)
            .and_then(|context| context.transcript_page.as_deref());
        let mut page_numbers = Vec::new();

        for (logical_index, raw_line) in page.text.lines().enumerate() {
            let (number, content) =
                parse_source_line(page, raw_line.trim(), profile.max_line_number);
            if is_page_header_content(content.trim(), transcript_page, logical_index) {
                continue;
            }
            if let Some(number) = number {
                page_numbers.push(number);
            }
        }

        if let Some(minimum) = page_numbers.iter().copied().min() {
            *min_frequency.entry(minimum).or_default() += 1;
        }
        if let Some(maximum) = page_numbers.iter().copied().max() {
            *max_frequency.entry(maximum).or_default() += 1;
        }
    }

    let typical_min = min_frequency
        .into_iter()
        .max_by_key(|(minimum, frequency)| (*frequency, std::cmp::Reverse(*minimum)))
        .map(|(minimum, _)| minimum);
    let typical_max = max_frequency
        .into_iter()
        .max_by_key(|(maximum, frequency)| (*frequency, *maximum))
        .map(|(maximum, _)| maximum);

    (typical_min, typical_max)
}

fn detect_gaps(
    document: &ExtractedDocument,
    profile: &TranscriptProfile,
    page_contexts: &[PageContext],
    typical_line_min: Option<u16>,
    typical_line_max: Option<u16>,
) -> Vec<TranscriptGap> {
    let mut gaps = Vec::new();

    let numeric_pages: Vec<(u32, u32)> = page_contexts
        .iter()
        .filter(|page| page.section.is_dialogue())
        .filter_map(|page| {
            page.transcript_page
                .as_deref()
                .and_then(|value| value.parse::<u32>().ok())
                .map(|transcript_page| (page.physical_page, transcript_page))
        })
        .collect();

    for pair in numeric_pages.windows(2) {
        let (_, previous) = pair[0];
        let (_, current) = pair[1];
        if current > previous + 1 {
            gaps.push(TranscriptGap {
                kind: GapKind::MissingTranscriptPages,
                physical_page: Some(pair[1].0),
                transcript_page: Some(current.to_string()),
                missing_line_start: None,
                missing_line_end: None,
                missing_transcript_page_start: Some(previous + 1),
                missing_transcript_page_end: Some(current - 1),
                reason: GapReason::UnobservedInExtractedText,
            });
        }
    }

    for page in &document.pages {
        if !page.section.is_dialogue() {
            continue;
        }
        let transcript_page = page_contexts
            .iter()
            .find(|context| context.physical_page == page.physical_page)
            .and_then(|context| context.transcript_page.clone());
        let transcript_page_ref = transcript_page.as_deref();
        let mut numbers = Vec::new();

        for (logical_index, raw_line) in page.text.lines().enumerate() {
            let (number, content) =
                parse_source_line(page, raw_line.trim(), profile.max_line_number);
            if is_page_header_content(content.trim(), transcript_page_ref, logical_index) {
                continue;
            }
            if let Some(number) = number {
                numbers.push(number);
            }
        }

        numbers.sort_unstable();
        numbers.dedup();
        if numbers.is_empty() {
            continue;
        }

        for pair in numbers.windows(2) {
            if pair[1] > pair[0] + 1 {
                gaps.push(TranscriptGap {
                    kind: GapKind::MissingLines,
                    physical_page: Some(page.physical_page),
                    transcript_page: transcript_page.clone(),
                    missing_line_start: Some(pair[0] + 1),
                    missing_line_end: Some(pair[1] - 1),
                    missing_transcript_page_start: None,
                    missing_transcript_page_end: None,
                    reason: GapReason::UnobservedInExtractedText,
                });
            }
        }
    }

    gaps
}

fn collect_redaction_candidates(document: &ExtractedDocument) -> Vec<RedactionCandidate> {
    const MAX_CANDIDATES: usize = 750;
    let mut candidates = Vec::new();

    for page in &document.pages {
        for annotation in &page.annotations {
            if annotation.is_explicit_redaction {
                candidates.push(RedactionCandidate {
                    physical_page: page.physical_page,
                    rect: annotation.rect,
                    kind: RedactionSignalKind::PdfRedactAnnotation,
                    confidence: SignalConfidence::High,
                    note: "PDF annotation subtype is /Redact.".to_string(),
                });
            } else if annotation.is_dark_overlay_candidate {
                candidates.push(RedactionCandidate {
                    physical_page: page.physical_page,
                    rect: annotation.rect,
                    kind: RedactionSignalKind::DarkAnnotationCandidate,
                    confidence: SignalConfidence::Medium,
                    note: "Dark overlay-like annotation; visual confirmation required.".to_string(),
                });
            }
            if candidates.len() >= MAX_CANDIDATES {
                return candidates;
            }
        }

        for rectangle in &page.filled_rectangles {
            if rectangle.possible_redaction {
                candidates.push(RedactionCandidate {
                    physical_page: page.physical_page,
                    rect: Some(rectangle.rect),
                    kind: RedactionSignalKind::DarkFilledRectangleCandidate,
                    confidence: SignalConfidence::Low,
                    note: "Dark filled vector rectangle; may be a redaction or ordinary design element."
                        .to_string(),
                });
            }
            if candidates.len() >= MAX_CANDIDATES {
                return candidates;
            }
        }

        let mut seen_textual_markers = BTreeSet::new();
        for raw_line in page.text.lines().chain(page.raw_text.lines()) {
            if contains_redaction_marker(raw_line) {
                let key = raw_line.trim().to_ascii_uppercase();
                if seen_textual_markers.insert(key) {
                    candidates.push(RedactionCandidate {
                        physical_page: page.physical_page,
                        rect: None,
                        kind: RedactionSignalKind::TextualRedactionMarker,
                        confidence: SignalConfidence::High,
                        note: format!(
                            "Text layer contains a redaction/sealed marker: {}",
                            truncate(raw_line.trim(), 140)
                        ),
                    });
                }
            }
            if candidates.len() >= MAX_CANDIDATES {
                return candidates;
            }
        }
    }

    candidates
}

fn summarize_redaction_signals(document: &ExtractedDocument) -> RedactionSummary {
    let explicit_redaction_annotations = document
        .pages
        .iter()
        .flat_map(|page| &page.annotations)
        .filter(|annotation| annotation.is_explicit_redaction)
        .count();
    let dark_annotation_candidates = document
        .pages
        .iter()
        .flat_map(|page| &page.annotations)
        .filter(|annotation| annotation.is_dark_overlay_candidate)
        .count();
    let dark_filled_rectangle_candidates = document
        .pages
        .iter()
        .flat_map(|page| &page.filled_rectangles)
        .filter(|rectangle| rectangle.possible_redaction)
        .count();
    let textual_redaction_markers = document
        .pages
        .iter()
        .map(|page| {
            let mut markers = BTreeSet::new();
            for line in page.text.lines().chain(page.raw_text.lines()) {
                if contains_redaction_marker(line) {
                    markers.insert(line.trim().to_ascii_uppercase());
                }
            }
            markers.len()
        })
        .sum();
    let pages_with_images = document
        .pages
        .iter()
        .filter(|page| page.image_count > 0)
        .count();

    RedactionSummary {
        explicit_redaction_annotations,
        dark_annotation_candidates,
        dark_filled_rectangle_candidates,
        textual_redaction_markers,
        pages_with_images,
        requires_visual_confirmation: dark_annotation_candidates > 0
            || dark_filled_rectangle_candidates > 0
            || pages_with_images > 0,
    }
}

fn build_format_fingerprint(
    document: &ExtractedDocument,
    page_contexts: &[PageContext],
    marker_rules: &[MarkerRule],
    typical_line_min: Option<u16>,
    typical_line_max: Option<u16>,
) -> FormatFingerprint {
    let question_markers: Vec<MarkerRule> = marker_rules
        .iter()
        .filter(|rule| rule.kind == MarkerKind::Question)
        .cloned()
        .collect();
    let answer_markers: Vec<MarkerRule> = marker_rules
        .iter()
        .filter(|rule| rule.kind == MarkerKind::Answer)
        .cloned()
        .collect();
    let question_observations: usize = question_markers.iter().map(|rule| rule.observations).sum();
    let answer_observations: usize = answer_markers.iter().map(|rule| rule.observations).sum();
    let paired_marker_convention = question_observations >= 1 && answer_observations >= 1;
    let dominant_question_marker = paired_marker_convention
        .then(|| dominant_marker(&question_markers))
        .flatten();
    let dominant_answer_marker = paired_marker_convention
        .then(|| dominant_marker(&answer_markers))
        .flatten();

    let mut colon_speaker_observations = 0usize;
    let mut titled_speaker_observations = 0usize;
    let mut examination_heading_style_detected = false;
    let mut redacted_speaker_labels_detected = false;

    for page in &document.pages {
        if !page.section.is_dialogue() {
            continue;
        }
        for line in page.text.lines() {
            let (_, trimmed) = parse_source_line(page, line.trim(), 100);
            if let Some((label, remainder)) = split_colon_speaker_prefix(trimmed) {
                if !remainder.is_empty() {
                    colon_speaker_observations += 1;
                }
                if is_redacted_label(label) {
                    redacted_speaker_labels_detected = true;
                }
            }
            if split_titled_speaker_prefix(trimmed).is_some() {
                titled_speaker_observations += 1;
            }
            if by_heading_label(trimmed).is_some() {
                examination_heading_style_detected = true;
            }
        }
    }

    let colon_speaker_prefixed_dialogue = colon_speaker_observations >= 2;
    let titled_speaker_prefixed_dialogue = titled_speaker_observations >= 1;
    let speaker_prefixed_dialogue =
        colon_speaker_prefixed_dialogue || titled_speaker_prefixed_dialogue;

    let mut notes = Vec::new();
    if redacted_speaker_labels_detected {
        notes.push(
            "Redacted speaker labels detected; participant identity must not be inferred from the redaction token alone."
                .to_string(),
        );
    }
    if dominant_question_marker.is_none() || dominant_answer_marker.is_none() {
        notes.push(
            "No dominant Q/A marker pair; speaker labels and conversational structure require greater weight."
                .to_string(),
        );
    }

    FormatFingerprint {
        question_markers,
        answer_markers,
        dominant_question_marker,
        dominant_answer_marker,
        speaker_prefixed_dialogue,
        colon_speaker_prefixed_dialogue,
        titled_speaker_prefixed_dialogue,
        examination_heading_style_detected,
        redacted_speaker_labels_detected,
        typical_line_min,
        typical_line_max,
        transcript_page_labels_detected: page_contexts
            .iter()
            .filter(|page| page.section.is_dialogue() && page.transcript_page.is_some())
            .count(),
        notes,
    }
}

fn dominant_marker(rules: &[MarkerRule]) -> Option<String> {
    rules
        .iter()
        .filter(|rule| rule.observations > 0)
        .max_by_key(|rule| (rule.observations, std::cmp::Reverse(rule.priority)))
        .map(|rule| rule.pattern.clone())
}

fn participant_confidence(participants: &[Participant], marker_rules: &[MarkerRule]) -> f32 {
    if participants.is_empty() {
        let marker_count: usize = marker_rules.iter().map(|rule| rule.observations).sum();
        return if marker_count >= 20 { 0.60 } else { 0.30 };
    }

    let role_known = participants
        .iter()
        .filter(|participant| participant.role != SpeakerRole::Unknown)
        .count();
    role_known as f32 / participants.len() as f32
}

fn marker_confidence(marker_rules: &[MarkerRule], fingerprint: &FormatFingerprint) -> f32 {
    let question_count: usize = marker_rules
        .iter()
        .filter(|rule| rule.kind == MarkerKind::Question)
        .map(|rule| rule.observations)
        .sum();
    let answer_count: usize = marker_rules
        .iter()
        .filter(|rule| rule.kind == MarkerKind::Answer)
        .map(|rule| rule.observations)
        .sum();
    let total = question_count + answer_count;

    if total >= 40 && question_count > 0 && answer_count > 0 {
        1.0
    } else if total >= 8 {
        0.82
    } else if total > 0 {
        0.60
    } else if fingerprint.speaker_prefixed_dialogue {
        // Speaker-labelled interviews/hearings can be structurally strong without Q./A. markers.
        0.82
    } else {
        0.35
    }
}

fn calculate_source_completeness(
    document: &ExtractedDocument,
    profile: &TranscriptProfile,
    page_contexts: &[PageContext],
    typical_min: Option<u16>,
    typical_max: Option<u16>,
) -> f32 {
    let (Some(min_line), Some(max_line)) = (typical_min, typical_max) else {
        return 0.5;
    };
    if max_line < min_line {
        return 0.5;
    }

    let numeric_pages: Vec<u32> = page_contexts
        .iter()
        .filter(|page| page.section.is_dialogue())
        .filter_map(|page| page.transcript_page.as_deref()?.parse::<u32>().ok())
        .collect();
    let (Some(min_page), Some(max_page)) = (
        numeric_pages.iter().copied().min(),
        numeric_pages.iter().copied().max(),
    ) else {
        return 0.5;
    };

    let lines_per_page = (max_line - min_line + 1) as usize;
    let expected_pages = (max_page - min_page + 1) as usize;
    let expected_lines = expected_pages.saturating_mul(lines_per_page);
    if expected_lines == 0 {
        return 0.5;
    }

    let mut observed = 0usize;
    for page in &document.pages {
        if !page.section.is_dialogue() {
            continue;
        }
        let transcript_page = page_contexts
            .iter()
            .find(|context| context.physical_page == page.physical_page)
            .and_then(|context| context.transcript_page.as_deref());
        let mut unique = BTreeSet::new();
        for (logical_index, raw_line) in page.text.lines().enumerate() {
            let (number, content) =
                parse_source_line(page, raw_line.trim(), profile.max_line_number);
            if is_page_header_content(content.trim(), transcript_page, logical_index) {
                continue;
            }
            if let Some(number) = number {
                unique.insert(number);
            }
        }
        observed += unique.len();
    }

    (observed as f32 / expected_lines as f32).clamp(0.0, 1.0)
}

fn detect_language(document: &ExtractedDocument) -> LanguageHint {
    let sample: String = document
        .pages
        .iter()
        .take(20)
        .map(|page| page.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .take(80_000)
        .collect();

    let mut latin = 0usize;
    let mut devanagari = 0usize;
    let mut arabic = 0usize;
    let mut cjk = 0usize;
    let mut letters = 0usize;

    for character in sample.chars() {
        let code = character as u32;
        if character.is_alphabetic() {
            letters += 1;
        }
        if character.is_ascii_alphabetic() || (0x00C0..=0x024F).contains(&code) {
            latin += 1;
        } else if (0x0900..=0x097F).contains(&code) {
            devanagari += 1;
        } else if (0x0600..=0x06FF).contains(&code) {
            arabic += 1;
        } else if (0x4E00..=0x9FFF).contains(&code)
            || (0x3040..=0x30FF).contains(&code)
            || (0xAC00..=0xD7AF).contains(&code)
        {
            cjk += 1;
        }
    }

    if letters == 0 {
        return LanguageHint {
            code: "und".to_string(),
            name: "Unknown".to_string(),
            confidence: 0.1,
            dominant_script: "unknown".to_string(),
        };
    }

    let lower = sample.to_ascii_lowercase();
    let english_hits = [" the ", " and ", " you ", " of ", " to ", " that "]
        .iter()
        .filter(|needle| lower.contains(*needle))
        .count();

    if latin * 100 / letters.max(1) >= 80 && english_hits >= 4 {
        return LanguageHint {
            code: "en".to_string(),
            name: "English".to_string(),
            confidence: 0.92,
            dominant_script: "latin".to_string(),
        };
    }

    let (script, count) = [
        ("latin", latin),
        ("devanagari", devanagari),
        ("arabic", arabic),
        ("cjk", cjk),
    ]
    .into_iter()
    .max_by_key(|(_, count)| *count)
    .unwrap_or(("unknown", 0));

    LanguageHint {
        code: "und".to_string(),
        name: "Undetermined".to_string(),
        confidence: if count > 0 { 0.65 } else { 0.1 },
        dominant_script: script.to_string(),
    }
}

fn detect_document_type(
    document: &ExtractedDocument,
    marker_rules: &[MarkerRule],
    participants: &[Participant],
) -> DocumentType {
    let sample = document
        .pages
        .iter()
        .take(16)
        .map(|page| page.text.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("\n");
    let marker_count: usize = marker_rules.iter().map(|rule| rule.observations).sum();
    let dialogue_signals = marker_count
        + participants
            .iter()
            .map(|participant| participant.observations)
            .sum::<usize>();

    let headings = document
        .pages
        .iter()
        .take(4)
        .flat_map(|p| p.text.lines())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("\n");
    if headings.contains("interview of") || headings.contains("transcribed interview") {
        return DocumentType::Interview;
    }
    if headings.contains("deposition of") {
        return DocumentType::Deposition;
    }
    if sample.contains("deposition") || sample.contains("deponent") {
        DocumentType::Deposition
    } else if sample.contains("arbitration") || sample.contains("arbitrator") {
        DocumentType::Arbitration
    } else if sample.contains("hearing") {
        DocumentType::Hearing
    } else if sample.contains("trial transcript")
        || sample.contains("trial proceedings")
        || (sample.contains("the court") && dialogue_signals >= 4)
    {
        DocumentType::TrialTranscript
    } else if sample.contains("interview") {
        DocumentType::Interview
    } else if sample.contains("examination") && dialogue_signals >= 4 {
        DocumentType::Examination
    } else if dialogue_signals >= 8 {
        DocumentType::OtherTranscript
    } else {
        DocumentType::Unknown
    }
}

fn titled_speaker_style_likely(document: &ExtractedDocument) -> bool {
    let mut colon = 0usize;
    let mut titled = 0usize;
    for page in &document.pages {
        if !page.section.is_dialogue() {
            continue;
        }
        for line in page.text.lines() {
            let (_, trimmed) = parse_source_line(page, line.trim(), 100);
            if split_colon_speaker_prefix(trimmed).is_some() {
                colon += 1;
            }
            if split_titled_speaker_prefix(trimmed).is_some() {
                titled += 1;
            }
        }
    }
    let _ = colon;
    titled >= 1
}

fn split_colon_speaker_prefix(text: &str) -> Option<(&str, &str)> {
    super::speaker::split_colon_label(text)
}

fn split_any_speaker_prefix(text: &str) -> Option<(&str, &str)> {
    split_colon_speaker_prefix(text).or_else(|| split_titled_speaker_prefix(text))
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
        && trimmed.chars().all(|c| {
            c.is_alphanumeric()
                || c.is_whitespace()
                || matches!(c, '.' | '-' | '\'' | '[' | ']' | '_' | '#')
        })
}

fn by_heading_label(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    let remainder = strip_prefix_ascii_case(trimmed, "BY ")?;
    if !remainder.ends_with(':') {
        return None;
    }
    let label = remainder.trim_end_matches(':').trim();
    if speaker_from_label(label).is_some()
        || looks_like_bare_attorney_heading(label)
        || is_redacted_label(label)
        || looks_like_generic_speaker_label(label)
    {
        Some(label)
    } else {
        None
    }
}

fn strip_prefix_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let candidate = text.get(..prefix.len())?;
    if candidate.eq_ignore_ascii_case(prefix) {
        text.get(prefix.len()..)
    } else {
        None
    }
}

fn looks_like_objection(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    lower.starts_with("objection")
        || lower.starts_with("object ")
        || lower.starts_with("i object")
        || lower.contains(" object to ")
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut result = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        result.push_str("...");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{
        DocumentKind, DocumentMetadata, ExtractedPage, PageLayoutDiagnostics, PageSection,
    };

    fn doc(pages: Vec<&str>) -> ExtractedDocument {
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

    #[test]
    fn learns_participants_markers_and_gap() {
        let document = doc(vec![
            "1 00048\n1 BY MR. SCOTT:\n2 Q. What happened?\n3 A. Nothing.\n4 A     Still nothing.\n6 Q. Why?",
        ]);
        let context = preflight_transcript(&document, &TranscriptProfile::us_english());

        assert!(
            context
                .participants
                .iter()
                .any(|person| person.display_label.eq_ignore_ascii_case("MR. SCOTT"))
        );
        assert!(
            context
                .marker_rules
                .iter()
                .any(|rule| rule.pattern == "Q." && rule.observations > 0)
        );
        assert!(
            context
                .marker_rules
                .iter()
                .any(|rule| rule.pattern == "A" && rule.observations > 0)
        );
        assert!(
            context.gaps.iter().any(|gap| {
                gap.kind == GapKind::MissingLines && gap.missing_line_start == Some(5)
            })
        );
    }

    #[test]
    fn generic_speaker_labels_are_learned_from_behavior() {
        let document = doc(vec![
            "1 PAGE 1\n1 JOHN DOE: Q. Where were you?\n2 JANE DOE: A. At home.",
        ]);
        let context = preflight_transcript(&document, &TranscriptProfile::us_english());
        assert!(context.participants.iter().any(|participant| {
            participant.display_label == "JOHN DOE" && participant.role == SpeakerRole::Attorney
        }));
        assert!(context.participants.iter().any(|participant| {
            participant.display_label == "JANE DOE" && participant.role == SpeakerRole::Witness
        }));
    }
}
