use super::{
    format::parse_numbered_line,
    markers::detect_marker_candidate,
    models::{
        ClassificationConfidence, LineKind, MarkerKind, SpeakerRole, TranscriptContext,
        TranscriptLine, VerificationIssue, VerificationStatus,
    },
    profile::TranscriptProfile,
    speaker::{contains_redaction_marker, is_pure_redaction_marker},
};

pub fn verify_transcript_lines(
    lines: &mut [TranscriptLine],
    context: &TranscriptContext,
    profile: &TranscriptProfile,
) -> Vec<VerificationIssue> {
    let mut issues = Vec::new();

    for line in lines.iter_mut() {
        if line.kind == LineKind::PageHeader {
            line.verification = VerificationStatus::Verified;
            continue;
        }

        if is_pure_redaction_marker(&line.text) || contains_redaction_marker(&line.raw_text) {
            line.verification = if line.kind == LineKind::RedactionMarker {
                VerificationStatus::Redacted
            } else {
                VerificationStatus::VerifiedWithInference
            };
        } else {
            line.verification = match line.confidence {
                ClassificationConfidence::Explicit => VerificationStatus::Verified,
                ClassificationConfidence::Inferred => VerificationStatus::VerifiedWithInference,
                ClassificationConfidence::Unknown => VerificationStatus::Uncertain,
            };
        }

        let raw = line.raw_text.trim();
        let raw_content = if line.line_number.is_some() {
            parse_numbered_line(raw, profile.max_line_number).1
        } else {
            raw
        };
        let anchored = super::speaker::split_colon_label(raw_content)
            .or_else(|| super::speaker::split_titled_speaker_prefix(raw_content));
        let raw_content = anchored.map(|(_, b)| b).unwrap_or(raw_content);

        if let Some((marker_kind, style, marker)) =
            detect_marker_candidate(raw_content).filter(|(_, style, _)| {
                *style != super::models::MarkerStyle::BareUppercase
                    || line
                        .evidence
                        .items
                        .iter()
                        .any(|e| e.source == super::models::EvidenceSource::ExplicitMarker)
            })
        {
            let expected = match marker_kind {
                MarkerKind::Question => LineKind::Question,
                MarkerKind::Answer => LineKind::Answer,
            };

            if line.kind != expected {
                line.verification = VerificationStatus::Conflict;
                issues.push(issue(
                    line,
                    "explicit_marker_conflict",
                    format!(
                        "Source contains explicit marker {marker}, but parser classified the line as {:?}.",
                        line.kind
                    ),
                ));
            }
        }

        if let Some(speaker) = line.speaker.as_ref() {
            if let Some(participant_id) = speaker.participant_id.as_deref()
                && let Some(participant) = context.participant_by_id(participant_id)
                && participant.role != SpeakerRole::Unknown
                && speaker.role != SpeakerRole::Unknown
                && participant.role != speaker.role
            {
                line.verification = VerificationStatus::Conflict;
                issues.push(issue(
                    line,
                    "participant_role_conflict",
                    format!(
                        "Speaker role {:?} disagrees with participant registry role {:?}.",
                        speaker.role, participant.role
                    ),
                ));
            }
        }

        if line.speaker.as_ref().is_some_and(|s| s.redacted) {
            line.verification = VerificationStatus::Uncertain;
        }
        if line.kind == LineKind::Unknown && line.verification != VerificationStatus::Conflict {
            line.verification = VerificationStatus::Uncertain;
        }
    }

    issues
}

fn strip_speaker_prefix(text: &str) -> &str {
    let Some(colon) = text.find(':') else {
        return text;
    };
    let prefix = text[..colon].trim();
    if prefix.len() > 80 || prefix.contains('?') || !looks_like_speaker_prefix(prefix) {
        return text;
    }
    text[colon + 1..].trim()
}

fn looks_like_speaker_prefix(prefix: &str) -> bool {
    let upper = prefix.to_ascii_uppercase();
    if upper.contains("REDACTED")
        || upper.starts_with("MR. ")
        || upper.starts_with("MR ")
        || upper.starts_with("MS. ")
        || upper.starts_with("MS ")
        || upper.starts_with("MRS. ")
        || upper.starts_with("THE ")
        || upper.starts_with("ATTORNEY ")
        || upper.starts_with("COUNSEL ")
    {
        return true;
    }

    let letters: Vec<char> = prefix.chars().filter(|c| c.is_alphabetic()).collect();
    !letters.is_empty()
        && letters.iter().filter(|c| c.is_uppercase()).count() as f32 / letters.len() as f32 >= 0.80
}

fn issue(line: &TranscriptLine, code: &str, message: String) -> VerificationIssue {
    VerificationIssue {
        physical_page: line.physical_page,
        transcript_page: line.transcript_page.clone(),
        line_number: line.line_number,
        code: code.to_string(),
        message,
    }
}
