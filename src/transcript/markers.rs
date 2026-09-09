use super::models::{MarkerKind, MarkerRule, MarkerStyle, TranscriptContext};

#[derive(Debug, Clone)]
pub struct MarkerMatch<'a> {
    pub kind: MarkerKind,
    pub style: MarkerStyle,
    pub pattern: String,
    pub priority: u8,
    pub remainder: &'a str,
}

pub fn default_marker_rules() -> Vec<MarkerRule> {
    vec![
        rule(MarkerKind::Question, MarkerStyle::Dot, "Q.", 1),
        rule(MarkerKind::Answer, MarkerStyle::Dot, "A.", 1),
        rule(MarkerKind::Question, MarkerStyle::BareUppercase, "Q", 2),
        rule(MarkerKind::Answer, MarkerStyle::BareUppercase, "A", 2),
        rule(MarkerKind::Question, MarkerStyle::Colon, "Q:", 3),
        rule(MarkerKind::Answer, MarkerStyle::Colon, "A:", 3),
        rule(MarkerKind::Question, MarkerStyle::Word, "QUESTION:", 4),
        rule(MarkerKind::Answer, MarkerStyle::Word, "ANSWER:", 4),
    ]
}

fn rule(kind: MarkerKind, style: MarkerStyle, pattern: &str, priority: u8) -> MarkerRule {
    MarkerRule {
        kind,
        style,
        pattern: pattern.to_string(),
        priority,
        observations: 0,
    }
}

pub fn match_marker<'a>(text: &'a str, context: &TranscriptContext) -> Option<MarkerMatch<'a>> {
    // Explicit punctuation conventions can change within a document. Bare Q/A requires
    // an observed pair; a single article "A ..." is not a format declaration.
    let paired = context
        .marker_rules
        .iter()
        .any(|r| r.kind == MarkerKind::Question && r.observations > 0)
        && context
            .marker_rules
            .iter()
            .any(|r| r.kind == MarkerKind::Answer && r.observations > 0);
    let mut rules = context
        .marker_rules
        .iter()
        .filter(|rule| rule.style != MarkerStyle::BareUppercase || paired)
        .collect::<Vec<_>>();
    rules.sort_by_key(|rule| (rule.priority, std::cmp::Reverse(rule.observations)));

    for rule in rules {
        if let Some(remainder) = match_rule(text, rule) {
            return Some(MarkerMatch {
                kind: rule.kind,
                style: rule.style,
                pattern: rule.pattern.clone(),
                priority: rule.priority,
                remainder,
            });
        }
    }

    None
}

pub fn detect_marker_candidate(text: &str) -> Option<(MarkerKind, MarkerStyle, &'static str)> {
    if strip_single_letter_punctuation(text, 'Q', '.').is_some() {
        return Some((MarkerKind::Question, MarkerStyle::Dot, "Q."));
    }
    if strip_single_letter_punctuation(text, 'A', '.').is_some() {
        return Some((MarkerKind::Answer, MarkerStyle::Dot, "A."));
    }
    if strip_bare_uppercase(text, 'Q').is_some() {
        return Some((MarkerKind::Question, MarkerStyle::BareUppercase, "Q"));
    }
    if strip_bare_uppercase(text, 'A').is_some() {
        return Some((MarkerKind::Answer, MarkerStyle::BareUppercase, "A"));
    }
    if strip_single_letter_punctuation(text, 'Q', ':').is_some() {
        return Some((MarkerKind::Question, MarkerStyle::Colon, "Q:"));
    }
    if strip_single_letter_punctuation(text, 'A', ':').is_some() {
        return Some((MarkerKind::Answer, MarkerStyle::Colon, "A:"));
    }
    if strip_case_insensitive_prefix(text, "QUESTION:").is_some() {
        return Some((MarkerKind::Question, MarkerStyle::Word, "QUESTION:"));
    }
    if strip_case_insensitive_prefix(text, "ANSWER:").is_some() {
        return Some((MarkerKind::Answer, MarkerStyle::Word, "ANSWER:"));
    }
    if strip_case_insensitive_prefix(text, "QUESTION.").is_some() {
        return Some((MarkerKind::Question, MarkerStyle::Word, "QUESTION."));
    }
    if strip_case_insensitive_prefix(text, "ANSWER.").is_some() {
        return Some((MarkerKind::Answer, MarkerStyle::Word, "ANSWER."));
    }

    None
}

fn match_rule<'a>(text: &'a str, rule: &MarkerRule) -> Option<&'a str> {
    match rule.style {
        MarkerStyle::Dot => {
            let marker = rule.pattern.chars().next()?;
            strip_single_letter_punctuation(text, marker, '.')
        }
        MarkerStyle::Colon => {
            let marker = rule.pattern.chars().next()?;
            strip_single_letter_punctuation(text, marker, ':')
        }
        MarkerStyle::Word => strip_case_insensitive_prefix(text, &rule.pattern),
        MarkerStyle::BareUppercase => {
            let marker = rule.pattern.chars().next()?;
            strip_bare_uppercase(text, marker)
        }
        MarkerStyle::SpeakerPrefixed => None,
    }
}

fn strip_single_letter_punctuation(text: &str, marker: char, punctuation: char) -> Option<&str> {
    let trimmed = text.trim_start();
    let first = trimmed.chars().next()?;

    // Never case-fold Q/A. Lowercase "a product..." is ordinary testimony text.
    if first != marker {
        return None;
    }

    let after_marker = &trimmed[first.len_utf8()..];
    let after_punctuation = after_marker.strip_prefix(punctuation)?;
    if after_punctuation
        .chars()
        .next()
        .is_some_and(|c| !c.is_whitespace())
    {
        return None;
    }
    Some(after_punctuation.trim_start())
}

fn strip_case_insensitive_prefix<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let trimmed = text.trim_start();
    let candidate = trimmed.get(..prefix.len())?;
    if candidate.eq_ignore_ascii_case(prefix) {
        Some(trimmed.get(prefix.len()..)?.trim_start())
    } else {
        None
    }
}

fn strip_bare_uppercase(text: &str, marker: char) -> Option<&str> {
    let trimmed = text.trim_start();
    let first = trimmed.chars().next()?;

    if first != marker {
        return None;
    }

    let remainder = &trimmed[first.len_utf8()..];
    if remainder.is_empty() {
        return Some(remainder);
    }
    if remainder
        .chars()
        .next()
        .is_some_and(|character| character.is_whitespace())
    {
        Some(remainder.trim_start())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_answer_is_uppercase_only() {
        assert!(strip_bare_uppercase("A     Yes.", 'A').is_some());
        assert!(strip_bare_uppercase("a product that works", 'A').is_none());
    }
}
