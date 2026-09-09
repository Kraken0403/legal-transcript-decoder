use super::models::{Participant, ParticipantFunction, Speaker, SpeakerRole, TranscriptContext};

pub fn is_redacted_label(label: &str) -> bool {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return false;
    }

    let upper = trimmed.to_ascii_uppercase();
    upper.contains("[REDACTED]")
        || upper.contains("NAME REDACTED")
        || upper.contains("REDACTED NAME")
        || upper.contains("NAME WITHHELD")
        || upper.contains("[SEALED]")
        || upper == "REDACTED"
        || upper == "SEALED"
        || trimmed
            .chars()
            .filter(|c| *c == '█' || *c == '■' || *c == '▇')
            .count()
            >= 3
        || (trimmed.len() >= 4 && trimmed.chars().all(|c| matches!(c, 'X' | 'x' | '*' | '_')))
}

pub fn contains_redaction_marker(text: &str) -> bool {
    let upper = text.to_ascii_uppercase();
    upper.contains("[REDACTED]")
        || upper.contains("[NAME REDACTED]")
        || upper.contains("[REDACTED NAME]")
        || upper.contains("NAME REDACTED")
        || upper.contains("NAME WITHHELD")
        || upper.contains("DOJ REDACTION")
        || upper.contains("[SEALED]")
        || text
            .chars()
            .filter(|c| *c == '█' || *c == '■' || *c == '▇')
            .count()
            >= 3
}

pub fn is_pure_redaction_marker(text: &str) -> bool {
    let trimmed = text.trim().trim_matches(':').trim();
    let upper = trimmed.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "[REDACTED]"
            | "[NAME REDACTED]"
            | "[REDACTED NAME]"
            | "[NAME WITHHELD]"
            | "[SEALED]"
            | "REDACTED"
            | "SEALED"
            | "NAME WITHHELD"
    ) || (trimmed.chars().count() >= 3
        && trimmed
            .chars()
            .all(|c| matches!(c, '█' | '■' | '▇' | 'X' | 'x' | '*' | '_')))
}

pub fn speaker_from_label(label: &str) -> Option<Speaker> {
    let trimmed = label.trim();
    let upper = trimmed.to_ascii_uppercase();

    if is_redacted_label(trimmed) {
        return Some(Speaker {
            participant_id: None,
            label: trimmed.to_string(),
            role: SpeakerRole::Unknown,
            redacted: true,
        });
    }

    if matches!(
        upper.as_str(),
        "THE WITNESS" | "WITNESS" | "THE DEPONENT" | "DEPONENT"
    ) {
        return Some(speaker(trimmed, SpeakerRole::Witness, false));
    }

    if upper.contains("INTERPRETER") {
        return Some(speaker(trimmed, SpeakerRole::Interpreter, false));
    }

    if upper == "THE REPORTER"
        || upper == "REPORTER"
        || upper == "COURT REPORTER"
        || upper.contains("COURT REPORTER")
        || upper.contains("STENOGRAPHER")
    {
        return Some(speaker(trimmed, SpeakerRole::CourtReporter, false));
    }

    if upper.contains("VIDEOGRAPHER") || upper == "VIDEO OPERATOR" {
        return Some(speaker(trimmed, SpeakerRole::Videographer, false));
    }

    if upper == "THE COURT"
        || upper.starts_with("JUDGE ")
        || upper.starts_with("HON. ")
        || upper.starts_with("HONORABLE ")
    {
        return Some(speaker(trimmed, SpeakerRole::Judge, false));
    }

    if looks_like_honorific_label(&upper) || upper.starts_with("DR. ") || upper.starts_with("DR ") {
        return Some(speaker(trimmed, SpeakerRole::Unknown, false));
    }

    if looks_like_attorney_label(&upper) {
        return Some(speaker(trimmed, SpeakerRole::Attorney, false));
    }

    None
}

pub fn resolve_known_speaker(label: &str, context: &TranscriptContext) -> Option<Speaker> {
    let trimmed = label.trim();

    if let Some(participant) = context.participants.iter().find(|participant| {
        participant
            .observed_labels
            .iter()
            .any(|observed| observed.eq_ignore_ascii_case(trimmed))
            || participant.display_label.eq_ignore_ascii_case(trimmed)
    }) {
        return Some(speaker_from_participant(participant));
    }

    speaker_from_label(trimmed).map(|mut s| {
        if !s.redacted {
            s.participant_id = Some(label_id(trimmed));
        }
        s
    })
}

pub fn speaker_from_participant(participant: &Participant) -> Speaker {
    let role = if participant.role != SpeakerRole::Unknown {
        participant.role
    } else {
        match participant.function {
            ParticipantFunction::Witness | ParticipantFunction::Interviewee => SpeakerRole::Witness,
            ParticipantFunction::Counsel | ParticipantFunction::GovernmentCounsel => {
                SpeakerRole::Attorney
            }
            ParticipantFunction::Interviewer
            | ParticipantFunction::LawEnforcement
            | ParticipantFunction::Other
            | ParticipantFunction::Unknown => SpeakerRole::Unknown,
            ParticipantFunction::Interpreter => SpeakerRole::Interpreter,
            ParticipantFunction::CourtReporter => SpeakerRole::CourtReporter,
            ParticipantFunction::Judge => SpeakerRole::Judge,
            ParticipantFunction::Videographer => SpeakerRole::Videographer,
        }
    };

    Speaker {
        participant_id: Some(participant.id.clone()),
        label: participant.display_label.clone(),
        role,
        redacted: participant.redacted,
    }
}

pub fn looks_like_honorific_label(upper: &str) -> bool {
    upper.starts_with("MR. ")
        || upper.starts_with("MR ")
        || upper.starts_with("MS. ")
        || upper.starts_with("MS ")
        || upper.starts_with("MRS. ")
        || upper.starts_with("MRS ")
        || upper.starts_with("MISS ")
        || upper.starts_with("CHAIRMAN ")
        || upper.starts_with("CHAIRWOMAN ")
        || upper.starts_with("CONGRESSMAN ")
        || upper.starts_with("CONGRESSWOMAN ")
        || upper.starts_with("REPRESENTATIVE ")
        || upper.starts_with("SENATOR ")
        || upper.starts_with("GOVERNOR ")
}

pub fn split_titled_speaker_prefix(text: &str) -> Option<(&str, &str)> {
    let trimmed = text.trim();
    let upper = trimmed.to_ascii_uppercase();
    let prefixes = [
        "MR. ",
        "MR ",
        "MS. ",
        "MS ",
        "MRS. ",
        "MRS ",
        "MISS ",
        "CHAIRMAN ",
        "CHAIRWOMAN ",
        "CONGRESSMAN ",
        "CONGRESSWOMAN ",
        "REPRESENTATIVE ",
        "SENATOR ",
        "GOVERNOR ",
        "DR. ",
        "DR ",
        "JUDGE ",
        "JUSTICE ",
        "THE WITNESS ",
    ];

    let prefix_len = prefixes
        .iter()
        .find_map(|prefix| upper.starts_with(prefix).then_some(prefix.len()))?;
    // Skip initials; the terminal full stop must follow a name token.
    let relative_end = trimmed
        .get(prefix_len..)?
        .char_indices()
        .find_map(|(i, c)| {
            if c != '.' {
                return None;
            }
            let before = &trimmed[prefix_len..prefix_len + i];
            let token = before.split_whitespace().last().unwrap_or("");
            (token.chars().count() > 1).then_some(i)
        })?;
    let label_end = prefix_len + relative_end;
    let label = trimmed.get(..label_end)?.trim();
    let remainder = trimmed.get(label_end + 1..)?.trim();

    if label.is_empty() || label.len() > 80 {
        return None;
    }
    if !label
        .chars()
        .all(|c| c.is_alphabetic() || c.is_whitespace() || matches!(c, '.' | '\'' | '’' | '-'))
    {
        return None;
    }
    if label.split_whitespace().skip(1).any(|w| {
        !w.chars().next().is_some_and(|c| c.is_uppercase())
            && !matches!(w, "de" | "van" | "von" | "der" | "den" | "la" | "del")
    }) {
        return None;
    }
    let words = label.split_whitespace().count();
    if words < 2 || words > 6 || label.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }

    Some((label, remainder))
}
pub fn looks_like_attorney_label(upper: &str) -> bool {
    upper.starts_with("MR. ")
        || upper.starts_with("MR ")
        || upper.starts_with("MS. ")
        || upper.starts_with("MS ")
        || upper.starts_with("MRS. ")
        || upper.starts_with("MRS ")
        || upper.starts_with("MISS ")
        || upper.starts_with("ATTORNEY ")
        || upper.starts_with("ATTY. ")
        || upper.starts_with("ATTY ")
        || upper.starts_with("COUNSEL ")
        || upper.contains("COUNSEL FOR ")
        || upper.starts_with("ESQ. ")
}

pub fn looks_like_bare_attorney_heading(label: &str) -> bool {
    if is_redacted_label(label) {
        return true;
    }

    let words: Vec<&str> = label.split_whitespace().collect();
    if words.is_empty() || words.len() > 5 {
        return false;
    }

    let has_letter = label
        .chars()
        .any(|character| character.is_ascii_alphabetic());
    if !has_letter
        || label
            .chars()
            .any(|character| character.is_ascii_lowercase())
    {
        return false;
    }

    label.chars().all(|character| {
        character.is_ascii_alphanumeric()
            || character.is_whitespace()
            || matches!(character, '.' | '\'' | '-')
    })
}

pub fn witness_speaker(context: &TranscriptContext) -> Speaker {
    context.primary_witness.clone().unwrap_or_else(|| Speaker {
        participant_id: Some("witness_1".to_string()),
        label: "THE WITNESS".to_string(),
        role: SpeakerRole::Witness,
        redacted: false,
    })
}

pub fn unknown_attorney() -> Speaker {
    Speaker {
        participant_id: None,
        label: "UNKNOWN ATTORNEY".to_string(),
        role: SpeakerRole::Attorney,
        redacted: false,
    }
}

pub fn anonymous_speaker(role: SpeakerRole, sequence: usize, redacted: bool) -> Speaker {
    let role_name = match role {
        SpeakerRole::Witness => "witness",
        SpeakerRole::Attorney => "attorney",
        SpeakerRole::Interpreter => "interpreter",
        SpeakerRole::CourtReporter => "reporter",
        SpeakerRole::Judge => "judge",
        SpeakerRole::Videographer => "videographer",
        SpeakerRole::Unknown => "speaker",
    };

    let id = if redacted {
        format!("{role_name}_redacted_{sequence}")
    } else {
        format!("{role_name}_anonymous_{sequence}")
    };

    Speaker {
        participant_id: Some(id),
        label: if redacted {
            "[REDACTED]".to_string()
        } else {
            format!("ANONYMOUS {} {sequence}", role_name.to_ascii_uppercase())
        },
        role,
        redacted,
    }
}

fn speaker(label: &str, role: SpeakerRole, redacted: bool) -> Speaker {
    Speaker {
        participant_id: None,
        label: label.to_string(),
        role,
        redacted,
    }
}

pub fn is_structural_label(label: &str) -> bool {
    let u = label.trim().to_ascii_uppercase();
    matches!(
        u.as_str(),
        "Q" | "A"
            | "QUESTION"
            | "ANSWER"
            | "APPEARANCES"
            | "PRESENT"
            | "INTERVIEW OF"
            | "DEPOSITION OF"
            | "CASE NO"
            | "CASE NO."
            | "DATE"
            | "TIME"
            | "NOTE"
            | "NOTES"
            | "EXHIBITS"
            | "INDEX"
    ) || u.starts_with("FOR ")
        || u.starts_with("BY ")
        || u.starts_with("CERTIFICATE")
        || u.starts_with("EXAMINATION")
        || u.starts_with("IN THE ")
}

pub fn split_colon_label(text: &str) -> Option<(&str, &str)> {
    let (label, body) = text.trim().split_once(':')?;
    let label = label.trim();
    if is_structural_label(label) || label.is_empty() || label.len() > 80 {
        return None;
    }
    let words = label.split_whitespace().count();
    let known = speaker_from_label(label).is_some();
    let generic = (1..=6).contains(&words)
        && label.chars().any(char::is_alphabetic)
        && label
            .chars()
            .all(|c| c.is_alphabetic() || c.is_whitespace() || matches!(c, '.' | '-' | '\'' | '’'))
        && label
            .chars()
            .filter(|c| c.is_alphabetic())
            .all(|c| c.is_uppercase());
    (known || generic || is_redacted_label(label)).then_some((label, body.trim()))
}

/// Explicit introductions establish a witness; a name mentioned in testimony does not.
pub fn witness_introduction(text: &str) -> Option<&str> {
    let t = text.trim();
    let upper = t.to_ascii_uppercase();
    let candidate = if upper.starts_with("WITNESS: ") {
        t.get(9..)?
    } else if upper.starts_with("DEPONENT: ") {
        t.get(10..)?
    } else if let Some((name, description)) = t.split_once(',') {
        let d = description.trim().to_ascii_uppercase();
        if !(d.starts_with("HAVING BEEN")
            || d.starts_with("BEING FIRST")
            || d.starts_with("CALLED AS A WITNESS"))
        {
            return None;
        }
        name
    } else {
        return None;
    };
    let candidate = candidate.trim();
    let words: Vec<_> = candidate.split_whitespace().collect();
    if !(2..=6).contains(&words.len())
        || !candidate
            .chars()
            .all(|c| c.is_alphabetic() || c.is_whitespace() || matches!(c, '.' | '\'' | '’' | '-'))
    {
        return None;
    }
    Some(candidate)
}

pub fn label_id(label: &str) -> String {
    format!(
        "label_{}",
        label
            .trim()
            .to_ascii_uppercase()
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}
