use crate::document::ExtractedPage;

use super::profile::TranscriptProfile;

pub fn parse_numbered_line(line: &str, max_line_number: u16) -> (Option<u16>, &str) {
    let digit_count = line
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .count();

    if digit_count == 0 {
        return (None, line);
    }

    let (number_text, remainder) = line.split_at(digit_count);
    let Ok(number) = number_text.parse::<u16>() else {
        return (None, line);
    };

    if number == 0 || number > max_line_number {
        return (None, line);
    }

    if !remainder.is_empty()
        && !remainder.starts_with(char::is_whitespace)
        && !remainder.starts_with(':')
        && !["Q.", "A.", "Q:", "A:"]
            .iter()
            .any(|m| remainder.starts_with(m))
    {
        return (None, line);
    }
    let remainder = remainder.trim_start_matches(':').trim_start();

    (Some(number), remainder)
}

pub fn detect_transcript_page_label(
    page: &ExtractedPage,
    profile: &TranscriptProfile,
    previous_page: Option<u32>,
) -> Option<String> {
    if page.visual_rows.iter().any(|r| r.panel > 0) {
        return None;
    }
    if let Some(label) = page.visual_rows.iter().find_map(|r| r.printed_page.clone()) {
        return Some(label);
    }
    // A standalone number outside a proven gutter is a printed page label, even below 50.
    if page.layout.used_positioned_reconstruction && page.layout.numbered_row_count >= 4 {
        for row in page
            .visual_rows
            .iter()
            .take(3)
            .chain(page.visual_rows.iter().rev().take(3))
        {
            if row.line_number.is_none() {
                if let Some(n) =
                    explicit_page_marker(&row.text).or_else(|| pure_numeric_page(&row.text))
                {
                    return Some(n.to_string());
                }
            }
        }
    }
    let mut best: Option<(i32, u32)> = None;

    for (index, raw_line) in page
        .text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(10)
        .enumerate()
    {
        let (line_number, content) =
            parse_source_line(page, raw_line.trim(), profile.max_line_number);
        let content = content.trim();

        if let Some(number) = explicit_page_marker(content) {
            return Some(number.to_string());
        }

        let Some(number) = pure_numeric_page(content) else {
            continue;
        };

        let mut score = 0i32;
        if content.len() >= 3 && content.starts_with('0') {
            score += 100;
        }
        if line_number.is_none() && index < 5 && number > profile.max_line_number as u32 {
            score += 80;
        }
        if let Some(previous) = previous_page
            && number == previous + 1
            && index < 5
        {
            score += 80;
        }
        if index == 0
            && line_number.is_none()
            && has_line_number_convention(page, profile.max_line_number)
        {
            score += 80;
        }
        if index < 5 {
            score += 10;
        }

        match best {
            Some((best_score, _)) if best_score >= score => {}
            _ => best = Some((score, number)),
        }
    }

    match best {
        Some((score, number)) if score >= 80 => Some(number.to_string()),
        _ => None,
    }
}

pub fn is_page_header_content(
    content: &str,
    transcript_page: Option<&str>,
    logical_index: usize,
) -> bool {
    if logical_index >= 8 {
        return false;
    }

    let Some(transcript_page) = transcript_page else {
        return false;
    };
    let Ok(expected) = transcript_page.parse::<u32>() else {
        return false;
    };

    if let Some(page) = explicit_page_marker(content) {
        return page == expected;
    }

    pure_numeric_page(content) == Some(expected)
}

pub fn explicit_page_marker(text: &str) -> Option<u32> {
    let upper = text.trim().to_ascii_uppercase();
    upper.strip_prefix("PAGE ")?.trim().parse::<u32>().ok()
}

pub fn pure_numeric_page(text: &str) -> Option<u32> {
    let trimmed = text.trim();
    if trimmed.is_empty()
        || trimmed.len() > 7
        || !trimmed.chars().all(|character| character.is_ascii_digit())
    {
        return None;
    }

    trimmed.parse::<u32>().ok()
}

/// Strip a printed gutter once, only when the page supplies evidence of one.
pub fn parse_source_line<'a>(
    page: &ExtractedPage,
    raw: &'a str,
    max: u16,
) -> (Option<u16>, &'a str) {
    if page.layout.used_positioned_reconstruction {
        if let Some(row) = page
            .visual_rows
            .iter()
            .find(|r| r.text.trim() == raw.trim())
        {
            return match row.line_number {
                Some(number) => {
                    let (parsed, body) = parse_numbered_line(raw.trim(), max.max(number));
                    if parsed == Some(number) {
                        (Some(number), body)
                    } else {
                        (Some(number), raw)
                    }
                }
                None => (None, raw),
            };
        }
    }
    if has_line_number_convention(page, max) {
        parse_numbered_line(raw.trim(), max)
    } else {
        (None, raw)
    }
}

pub fn has_line_number_convention(page: &ExtractedPage, max: u16) -> bool {
    if page.layout.used_positioned_reconstruction {
        return page.layout.numbered_row_count > 0;
    }
    let candidates: Vec<_> = page
        .text
        .lines()
        .filter_map(|line| {
            let (n, b) = parse_numbered_line(line.trim(), max);
            n.map(|n| (n, b))
        })
        .collect();
    let explicit = candidates
        .iter()
        .any(|(_, b)| ["Q.", "A.", "Q:", "A:"].iter().any(|m| b.starts_with(m)));
    explicit
        || (candidates.len() >= 3
            && candidates.windows(2).filter(|w| w[1].0 > w[0].0).count() * 2
                >= candidates.len() - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numbers_in_prose_are_not_attached_gutters() {
        assert_eq!(
            parse_numbered_line("12-year-old", 100),
            (None, "12-year-old")
        );
    }
    #[test]
    fn blank_printed_number_is_preserved() {
        assert_eq!(parse_numbered_line("12", 100), (Some(12), ""));
    }
    #[test]
    fn glued_explicit_marker_can_be_recovered() {
        assert_eq!(
            parse_numbered_line("12Q. When?", 100),
            (Some(12), "Q. When?")
        );
    }
}
