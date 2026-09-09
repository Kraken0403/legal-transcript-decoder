use std::collections::BTreeMap;

use lopdf::{Document, Encoding, Object, ObjectId, content::Content};

use super::models::{
    AnnotationSignal, ExtractedPage, FilledRectangleSignal, PageLayoutDiagnostics, PageSection,
    PositionedTextFragment, TextSource, VisualRow,
};

const MAX_PAGE_CONTENT_SIZE: usize = 64 * 1024 * 1024;
const DEFAULT_FONT_SIZE: f64 = 10.0;
const MIN_ROW_TOLERANCE: f64 = 0.65;
const MAX_ROW_TOLERANCE: f64 = 3.0;

#[derive(Debug, Clone, Copy)]
struct Matrix {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Matrix {
    const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    fn from_operands(operands: &[Object]) -> Option<Self> {
        if operands.len() < 6 {
            return None;
        }
        Some(Self {
            a: object_to_f64(&operands[0])?,
            b: object_to_f64(&operands[1])?,
            c: object_to_f64(&operands[2])?,
            d: object_to_f64(&operands[3])?,
            e: object_to_f64(&operands[4])?,
            f: object_to_f64(&operands[5])?,
        })
    }

    /// Compose this matrix with `other` using the PDF affine transform convention.
    fn concat(self, other: Self) -> Self {
        Self {
            a: self.a * other.a + self.c * other.b,
            b: self.b * other.a + self.d * other.b,
            c: self.a * other.c + self.c * other.d,
            d: self.b * other.c + self.d * other.d,
            e: self.a * other.e + self.c * other.f + self.e,
            f: self.b * other.e + self.d * other.f + self.f,
        }
    }

    fn translate_local(self, tx: f64, ty: f64) -> Self {
        Self {
            e: self.e + tx * self.a + ty * self.c,
            f: self.f + tx * self.b + ty * self.d,
            ..self
        }
    }

    fn point(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }
}

#[derive(Debug, Clone)]
struct TextState {
    text_matrix: Matrix,
    line_matrix: Matrix,
    ctm: Matrix,
    leading: f64,
    rise: f64,
    font_name: Option<Vec<u8>>,
    font_size: f64,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            text_matrix: Matrix::IDENTITY,
            line_matrix: Matrix::IDENTITY,
            ctm: Matrix::IDENTITY,
            leading: 0.0,
            rise: 0.0,
            font_name: None,
            font_size: DEFAULT_FONT_SIZE,
        }
    }
}

pub fn extract_positioned_fragments(
    document: &Document,
    page_id: ObjectId,
) -> Result<Vec<PositionedTextFragment>, String> {
    let fonts = document
        .get_page_fonts(page_id)
        .map_err(|error| format!("Could not read page fonts: {error}"))?;

    let encodings: BTreeMap<Vec<u8>, Encoding<'_>> = fonts
        .into_iter()
        .filter_map(|(name, font)| {
            font.get_font_encoding_with_limit(document, MAX_PAGE_CONTENT_SIZE)
                .ok()
                .map(|encoding| (name, encoding))
        })
        .collect();

    let content_data = document
        .get_page_content_with_limit(page_id, MAX_PAGE_CONTENT_SIZE)
        .map_err(|error| format!("Could not decode page content: {error}"))?;
    let content = Content::decode(&content_data)
        .map_err(|error| format!("Could not parse page content operations: {error}"))?;

    let mut state = TextState::default();
    let mut graphics_stack: Vec<Matrix> = Vec::new();
    let mut fragments = Vec::new();
    let mut sequence = 0usize;

    for operation in &content.operations {
        match operation.operator.as_str() {
            "q" => graphics_stack.push(state.ctm),
            "Q" => {
                if let Some(previous) = graphics_stack.pop() {
                    state.ctm = previous;
                }
            }
            "cm" => {
                if let Some(matrix) = Matrix::from_operands(&operation.operands) {
                    state.ctm = matrix.concat(state.ctm);
                }
            }
            "BT" => {
                state.text_matrix = Matrix::IDENTITY;
                state.line_matrix = Matrix::IDENTITY;
            }
            "Tf" => {
                state.font_name = operation
                    .operands
                    .first()
                    .and_then(|operand| operand.as_name().ok())
                    .map(|name| name.to_vec());
                if let Some(size) = operation.operands.get(1).and_then(object_to_f64) {
                    state.font_size = size.abs().max(0.1);
                }
            }
            "Tm" => {
                if let Some(matrix) = Matrix::from_operands(&operation.operands) {
                    state.text_matrix = matrix;
                    state.line_matrix = matrix;
                }
            }
            "Td" => {
                if operation.operands.len() >= 2 {
                    if let (Some(tx), Some(ty)) = (
                        object_to_f64(&operation.operands[0]),
                        object_to_f64(&operation.operands[1]),
                    ) {
                        state.line_matrix = state.line_matrix.translate_local(tx, ty);
                        state.text_matrix = state.line_matrix;
                    }
                }
            }
            "TD" => {
                if operation.operands.len() >= 2 {
                    if let (Some(tx), Some(ty)) = (
                        object_to_f64(&operation.operands[0]),
                        object_to_f64(&operation.operands[1]),
                    ) {
                        state.leading = -ty;
                        state.line_matrix = state.line_matrix.translate_local(tx, ty);
                        state.text_matrix = state.line_matrix;
                    }
                }
            }
            "TL" => {
                if let Some(leading) = operation.operands.first().and_then(object_to_f64) {
                    state.leading = leading;
                }
            }
            "Ts" => {
                if let Some(rise) = operation.operands.first().and_then(object_to_f64) {
                    state.rise = rise;
                }
            }
            "T*" => move_to_next_text_line(&mut state),
            "Tj" | "TJ" => {
                emit_text_fragment(
                    document,
                    &encodings,
                    &mut state,
                    &operation.operands,
                    &mut fragments,
                    &mut sequence,
                );
            }
            "'" => {
                move_to_next_text_line(&mut state);
                emit_text_fragment(
                    document,
                    &encodings,
                    &mut state,
                    &operation.operands,
                    &mut fragments,
                    &mut sequence,
                );
            }
            "\"" => {
                move_to_next_text_line(&mut state);
                let string_operands = operation.operands.get(2..).unwrap_or(&[]);
                emit_text_fragment(
                    document,
                    &encodings,
                    &mut state,
                    string_operands,
                    &mut fragments,
                    &mut sequence,
                );
            }
            _ => {}
        }
    }

    Ok(fragments)
}

fn move_to_next_text_line(state: &mut TextState) {
    let leading = if state.leading.abs() > f64::EPSILON {
        state.leading
    } else {
        state.font_size * 1.2
    };
    state.line_matrix = state.line_matrix.translate_local(0.0, -leading);
    state.text_matrix = state.line_matrix;
}

fn emit_text_fragment(
    document: &Document,
    encodings: &BTreeMap<Vec<u8>, Encoding<'_>>,
    state: &mut TextState,
    operands: &[Object],
    fragments: &mut Vec<PositionedTextFragment>,
    sequence: &mut usize,
) {
    let Some(font_name) = state.font_name.as_ref() else {
        return;
    };
    let Some(encoding) = encodings.get(font_name) else {
        return;
    };
    let Ok(text) = decode_text_operands(document, encoding, operands) else {
        return;
    };
    let text = normalize_fragment_text(&text);
    if text.is_empty() {
        return;
    }

    let local_y = state.text_matrix.f + state.rise;
    let (x, y) = state.ctm.point(state.text_matrix.e, local_y);
    let font_size = state.font_size.abs().max(0.1);
    let width = estimate_text_width(&text, font_size);
    let height = font_size;

    fragments.push(PositionedTextFragment {
        text: text.clone(),
        x,
        y,
        width,
        height,
        font_name: Some(String::from_utf8_lossy(font_name).to_string()),
        font_size: Some(font_size),
        source: TextSource::NativePdf,
        sequence: *sequence,
        geometry_estimated: true,
        redaction_masked: false,
    });
    *sequence += 1;

    // Exact glyph advances require complete font metrics. For layout ordering we only need a
    // conservative current-point estimate; explicit Tm/Td/TJ positioning still dominates.
    state.text_matrix = state.text_matrix.translate_local(width, 0.0);
}

fn decode_text_operands(
    document: &Document,
    encoding: &Encoding<'_>,
    operands: &[Object],
) -> Result<String, lopdf::Error> {
    let mut text = String::new();
    collect_text(document, encoding, operands, &mut text)?;
    Ok(text)
}

fn collect_text(
    document: &Document,
    encoding: &Encoding<'_>,
    operands: &[Object],
    output: &mut String,
) -> Result<(), lopdf::Error> {
    for operand in operands {
        match operand {
            Object::String(bytes, _) => {
                output.push_str(&Document::decode_text(encoding, bytes)?);
            }
            Object::Array(items) => {
                collect_text(document, encoding, items, output)?;
            }
            Object::Integer(value) if *value < -100 => output.push(' '),
            Object::Real(value) if *value < -100.0 => output.push(' '),
            _ => {}
        }
    }
    let _ = document;
    Ok(())
}

fn normalize_fragment_text(text: &str) -> String {
    text.replace('\0', "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn estimate_text_width(text: &str, font_size: f64) -> f64 {
    let units: f64 = text
        .chars()
        .map(|character| {
            if character.is_whitespace() {
                0.33
            } else if matches!(
                character,
                'i' | 'l' | 'I' | '.' | ',' | ':' | ';' | '!' | '|'
            ) {
                0.28
            } else if matches!(character, 'W' | 'M' | '@' | '%' | '&') {
                0.82
            } else {
                0.54
            }
        })
        .sum();
    (units * font_size).max(font_size * 0.2)
}

/// Masks text that is spatially covered by a strong redaction signal before any transcript
/// semantics are inferred. This prevents recoverable hidden PDF text from leaking downstream.
pub fn apply_visual_redaction_masks(
    fragments: &mut [PositionedTextFragment],
    annotations: &[AnnotationSignal],
    filled_rectangles: &[FilledRectangleSignal],
) -> usize {
    let annotation_rects: Vec<[f64; 4]> = annotations
        .iter()
        .filter(|annotation| {
            annotation.is_explicit_redaction || annotation.is_dark_overlay_candidate
        })
        .filter_map(|annotation| annotation.rect.map(normalize_corner_rect))
        .collect();
    let filled_rects: Vec<[f64; 4]> = filled_rectangles
        .iter()
        .filter(|rectangle| rectangle.possible_redaction)
        .map(|rectangle| normalize_xywh_rect(rectangle.rect))
        .collect();

    let mut masked = 0usize;
    for fragment in fragments {
        let center_x = fragment.x + fragment.width.max(0.0) * 0.5;
        // PDF text y is normally the baseline. Check the baseline and a point through the glyph body.
        let points = [
            (center_x, fragment.y),
            (center_x, fragment.y + fragment.height.max(0.0) * 0.35),
        ];
        let covered = annotation_rects
            .iter()
            .chain(filled_rects.iter())
            .any(|rect| {
                points
                    .iter()
                    .any(|(x, y)| point_in_rect(*x, *y, *rect, 1.5))
            });

        if covered {
            fragment.text = "[REDACTED]".to_string();
            fragment.redaction_masked = true;
            masked += 1;
        }
    }
    masked
}

fn normalize_corner_rect(rect: [f64; 4]) -> [f64; 4] {
    [
        rect[0].min(rect[2]),
        rect[1].min(rect[3]),
        rect[0].max(rect[2]),
        rect[1].max(rect[3]),
    ]
}

fn normalize_xywh_rect(rect: [f64; 4]) -> [f64; 4] {
    normalize_corner_rect([rect[0], rect[1], rect[0] + rect[2], rect[1] + rect[3]])
}

fn point_in_rect(x: f64, y: f64, rect: [f64; 4], margin: f64) -> bool {
    x >= rect[0] - margin && x <= rect[2] + margin && y >= rect[1] - margin && y <= rect[3] + margin
}

pub fn reconstruct_visual_rows(
    fragments: Vec<PositionedTextFragment>,
    fallback_text: &str,
) -> (Vec<VisualRow>, String, PageLayoutDiagnostics) {
    if fragments.is_empty() {
        return (
            Vec::new(),
            fallback_text.to_string(),
            PageLayoutDiagnostics {
                positioned_text_available: false,
                used_positioned_reconstruction: false,
                fallback_reason: Some("No decodable positioned PDF text operations.".to_string()),
                ..PageLayoutDiagnostics::default()
            },
        );
    }

    let row_tolerance = adaptive_row_tolerance(&fragments);
    let fragment_count = fragments.len();
    let mut sorted = fragments;
    sorted.sort_by(|left, right| {
        right
            .y
            .partial_cmp(&left.y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                left.x
                    .partial_cmp(&right.x)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| left.sequence.cmp(&right.sequence))
    });

    let mut row_groups: Vec<Vec<PositionedTextFragment>> = Vec::new();
    let mut row_y_values: Vec<f64> = Vec::new();

    for fragment in sorted {
        if let Some((index, _)) = row_y_values
            .iter()
            .enumerate()
            .filter_map(|(index, y)| {
                let delta = (fragment.y - *y).abs();
                (delta <= row_tolerance).then_some((index, delta))
            })
            .min_by(|(_, left), (_, right)| {
                left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
            })
        {
            row_groups[index].push(fragment);
            let sum: f64 = row_groups[index].iter().map(|item| item.y).sum();
            row_y_values[index] = sum / row_groups[index].len() as f64;
        } else {
            row_y_values.push(fragment.y);
            row_groups.push(vec![fragment]);
        }
    }

    let mut rows: Vec<VisualRow> = row_groups
        .into_iter()
        .zip(row_y_values)
        .map(|(mut fragments, y)| {
            fragments.sort_by(|left, right| {
                left.x
                    .partial_cmp(&right.x)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| left.sequence.cmp(&right.sequence))
            });
            let text = join_row_fragments(&fragments);
            let line_number = leading_transcript_line_number(&text);
            let left_x = fragments.first().map(|fragment| fragment.x);
            VisualRow {
                panel: 0,
                printed_page: None,
                bbox: None,
                ocr_confidence: None,
                y,
                left_x,
                line_number_x: line_number.and(left_x),
                line_number,
                text,
                fragments,
            }
        })
        .filter(|row| !row.text.trim().is_empty())
        .collect();

    rows.sort_by(|left, right| {
        right
            .y
            .partial_cmp(&left.y)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let detected_line_number_column_x = detect_and_normalize_line_number_column(&mut rows);

    let reconstructed = rows
        .iter()
        .map(|row| row.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    let numbered_row_count = rows.iter().filter(|row| row.line_number.is_some()).count();
    let visual_row_count = rows.len();
    let likely_line_number_column_x =
        detected_line_number_column_x.or_else(|| likely_line_number_column(&rows));
    let confidence = reconstruction_confidence(&rows, &reconstructed, fallback_text);
    let masked_redaction_fragments = rows
        .iter()
        .flat_map(|row| row.fragments.iter())
        .filter(|fragment| fragment.redaction_masked)
        .count();
    let contains_masked_redaction = masked_redaction_fragments > 0;
    let use_positioned =
        (confidence >= 0.42 || contains_masked_redaction) && !reconstructed.trim().is_empty();

    let final_text = if use_positioned {
        reconstructed
    } else {
        fallback_text.to_string()
    };

    let fallback_reason = (!use_positioned).then(|| {
        "Positioned reconstruction was too sparse/inconsistent; retained bounded lopdf plain-text extraction."
            .to_string()
    });

    (
        rows,
        final_text,
        PageLayoutDiagnostics {
            positioned_text_available: true,
            used_positioned_reconstruction: use_positioned,
            fragment_count,
            visual_row_count,
            numbered_row_count,
            masked_redaction_fragments,
            likely_line_number_column_x,
            reconstruction_confidence: confidence,
            fallback_reason,
            requires_ocr: false,
            ..PageLayoutDiagnostics::default()
        },
    )
}

fn adaptive_row_tolerance(fragments: &[PositionedTextFragment]) -> f64 {
    let mut sizes: Vec<f64> = fragments
        .iter()
        .filter_map(|fragment| fragment.font_size)
        .filter(|size| *size > 0.1 && size.is_finite())
        .collect();
    if sizes.is_empty() {
        return 1.5;
    }
    sizes.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let median = sizes[sizes.len() / 2];
    (median * 0.18).clamp(MIN_ROW_TOLERANCE, MAX_ROW_TOLERANCE)
}

fn join_row_fragments(fragments: &[PositionedTextFragment]) -> String {
    let mut output = String::new();
    let mut previous: Option<&PositionedTextFragment> = None;

    for fragment in fragments {
        let text = fragment.text.trim();
        if text.is_empty() {
            continue;
        }
        if fragment.redaction_masked && previous.is_some_and(|previous| previous.redaction_masked) {
            continue;
        }

        if let Some(previous) = previous {
            let previous_right = previous.x + previous.width;
            let gap = fragment.x - previous_right;
            let reference_size = previous.font_size.unwrap_or(DEFAULT_FONT_SIZE).max(1.0);
            let previous_is_word = previous
                .text
                .chars()
                .last()
                .is_some_and(|character| character.is_alphanumeric());
            let current_is_word = text
                .chars()
                .next()
                .is_some_and(|character| character.is_alphanumeric());
            let should_space = gap > reference_size * 0.10
                || (previous_is_word && current_is_word && gap > -reference_size * 0.30);

            if should_space
                && !output
                    .chars()
                    .last()
                    .is_some_and(|character| character.is_whitespace())
                && !starts_with_closing_punctuation(text)
            {
                output.push(' ');
            }
        }

        output.push_str(text);
        previous = Some(fragment);
    }

    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn starts_with_closing_punctuation(text: &str) -> bool {
    text.chars().next().is_some_and(|character| {
        matches!(
            character,
            '.' | ',' | ';' | ':' | '?' | '!' | ')' | ']' | '}'
        )
    })
}

fn leading_transcript_line_number(text: &str) -> Option<u16> {
    let first = text.split_whitespace().next()?;
    let number = first.trim_matches(|character: char| !character.is_ascii_digit());
    if number.len() != first.len() {
        return None;
    }
    let parsed = number.parse::<u16>().ok()?;
    (1..=50).contains(&parsed).then_some(parsed)
}

fn detect_and_normalize_line_number_column(rows: &mut [VisualRow]) -> Option<f64> {
    #[derive(Debug, Clone, Copy)]
    struct Candidate {
        row_index: usize,
        fragment_index: usize,
        number: u16,
        x: f64,
    }

    // First trust row-leading numbers reconstructed from the visual row itself. This is the
    // strongest signal because the number already occupies the row boundary rather than ordinary
    // testimony text. Do not let later body numbers overwrite a coherent row-level sequence.
    let mut leading: Vec<(usize, u16, f64)> = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            Some((index, row.line_number?, row.line_number_x.or(row.left_x)?))
        })
        .collect();
    leading.sort_by_key(|item| item.0);
    if leading.len() >= 5 {
        let numbers: Vec<u16> = leading.iter().map(|item| item.1).collect();
        let min = numbers.iter().copied().min().unwrap_or(0);
        let max = numbers.iter().copied().max().unwrap_or(0);
        let sequence = tolerant_line_sequence_score(&numbers);
        if min <= 6 && max.saturating_sub(min) >= 5 && sequence >= 0.62 {
            let mut x_values: Vec<f64> = leading.iter().map(|item| item.2).collect();
            x_values.sort_by(|left, right| {
                left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal)
            });
            return Some(x_values[x_values.len() / 2]);
        }
    }

    let mut candidates = Vec::new();
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    for (row_index, row) in rows.iter().enumerate() {
        for (fragment_index, fragment) in row.fragments.iter().enumerate() {
            min_x = min_x.min(fragment.x);
            max_x = max_x.max(fragment.x + fragment.width.max(0.0));
            let token = fragment.text.trim();
            if let Ok(number) = token.parse::<u16>()
                && (1..=50).contains(&number)
            {
                candidates.push(Candidate {
                    row_index,
                    fragment_index,
                    number,
                    x: fragment.x,
                });
            }
        }
    }

    if candidates.len() < 5 {
        return None;
    }

    let page_width = (max_x - min_x).abs().max(1.0);
    let mut best: Option<(f32, Vec<Candidate>)> = None;
    for seed in candidates.iter().map(|candidate| candidate.x) {
        let mut column: Vec<Candidate> = candidates
            .iter()
            .copied()
            .filter(|candidate| (candidate.x - seed).abs() <= 12.0)
            .collect();
        column.sort_by_key(|candidate| candidate.row_index);
        column.dedup_by_key(|candidate| candidate.row_index);
        if column.len() < 5 {
            continue;
        }

        let numbers: Vec<u16> = column.iter().map(|candidate| candidate.number).collect();
        let unique: std::collections::BTreeSet<u16> = numbers.iter().copied().collect();
        let min_number = numbers.iter().copied().min().unwrap_or(0);
        let max_number = numbers.iter().copied().max().unwrap_or(0);
        let monotonic = tolerant_line_sequence_score(&numbers);

        // A transcript gutter behaves like a sequence, not merely a numeric column. This rejects
        // body values such as 9/11, 35-year, times, exhibit numbers, and monetary figures.
        if unique.len() < 5
            || min_number > 6
            || max_number.saturating_sub(min_number) < 5
            || monotonic < 0.62
        {
            continue;
        }

        let coverage = (column.len() as f32 / rows.len().max(1) as f32).min(1.0);
        let mut x_values: Vec<f64> = column.iter().map(|candidate| candidate.x).collect();
        x_values
            .sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
        let median_x = x_values[x_values.len() / 2];
        let distance_to_edge = (median_x - min_x).abs().min((max_x - median_x).abs());
        let edge_score = (1.0 - (distance_to_edge / (page_width * 0.5))).clamp(0.0, 1.0) as f32;
        let range_score = ((max_number.saturating_sub(min_number)) as f32 / 20.0).min(1.0);
        let score = monotonic * 0.55 + coverage * 0.20 + edge_score * 0.15 + range_score * 0.10;

        if best
            .as_ref()
            .map_or(true, |(best_score, _)| score > *best_score)
        {
            best = Some((score, column));
        }
    }

    let (_, column) = best?;
    let mut x_values: Vec<f64> = column.iter().map(|candidate| candidate.x).collect();
    x_values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let column_x = x_values[x_values.len() / 2];

    // Normalize a separate visual gutter into the canonical leading-number representation. The
    // conversation parser therefore does not care whether line numbers are printed left or right.
    for candidate in column.into_iter().rev() {
        let row = &mut rows[candidate.row_index];
        row.line_number = Some(candidate.number);
        row.line_number_x = Some(candidate.x);

        if candidate.fragment_index < row.fragments.len()
            && row.fragments[candidate.fragment_index].text.trim() == candidate.number.to_string()
        {
            row.fragments.remove(candidate.fragment_index);
            let body = join_row_fragments(&row.fragments);
            row.text = if body.is_empty() {
                candidate.number.to_string()
            } else {
                format!("{} {}", candidate.number, body)
            };
            row.left_x = row.fragments.first().map(|fragment| fragment.x);
        }
    }

    Some(column_x)
}

fn tolerant_line_sequence_score(numbers: &[u16]) -> f32 {
    if numbers.len() < 2 {
        return 0.0;
    }
    let good = numbers
        .windows(2)
        .filter(|pair| pair[1] > pair[0] && pair[1] <= pair[0].saturating_add(4))
        .count();
    good as f32 / (numbers.len() - 1) as f32
}

fn likely_line_number_column(rows: &[VisualRow]) -> Option<f64> {
    let mut x_values: Vec<f64> = rows
        .iter()
        .filter(|row| row.line_number.is_some())
        .filter_map(|row| row.line_number_x.or(row.left_x))
        .collect();
    if x_values.len() < 4 {
        return None;
    }
    x_values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    Some(x_values[x_values.len() / 2])
}

fn reconstruction_confidence(rows: &[VisualRow], reconstructed: &str, fallback: &str) -> f32 {
    if rows.is_empty() || reconstructed.trim().is_empty() {
        return 0.0;
    }

    let reconstructed_chars = semantic_char_count(reconstructed);
    let fallback_chars = semantic_char_count(fallback);
    let coverage = if fallback_chars == 0 {
        1.0
    } else {
        (reconstructed_chars as f32 / fallback_chars as f32).min(1.0)
    };
    let row_signal = (rows.len() as f32 / 8.0).min(1.0);
    let nonempty_ratio = rows
        .iter()
        .filter(|row| !row.text.trim().is_empty())
        .count() as f32
        / rows.len().max(1) as f32;

    (coverage * 0.60 + row_signal * 0.20 + nonempty_ratio * 0.20).clamp(0.0, 1.0)
}

fn semantic_char_count(text: &str) -> usize {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .count()
}

pub fn classify_page_sections(pages: &mut [ExtractedPage]) {
    for page in pages.iter_mut() {
        let (section, confidence, reasons) = classify_page(page);
        page.section = section;
        page.section_confidence = confidence;
        page.section_reasons = reasons;
    }

    // Front-matter appearance lists frequently span several numbered pages. Continue an explicit
    // APPEARANCES section until a genuine dialogue anchor appears; line numbers alone must not
    // turn a continuation page into testimony.
    let mut appearances_open = false;
    for page in pages.iter_mut() {
        if page.section == PageSection::Appearances {
            appearances_open = true;
            continue;
        }
        if !appearances_open {
            continue;
        }
        if page.section.is_reference_only() {
            appearances_open = false;
            continue;
        }
        let speaker_prefixes = count_speaker_prefixes(&page.text);
        let dialogue_markers = count_dialogue_markers(&page.text);
        let upper = page.text.to_ascii_uppercase();
        let examination = upper.contains("EXAMINATION")
            || upper
                .lines()
                .any(|line| strip_leading_line_number(line.trim()).starts_with("BY "));
        if speaker_prefixes == 0 && dialogue_markers == 0 && !examination {
            page.section = PageSection::Appearances;
            page.section_confidence = 0.88;
            page.section_reasons.push(
                "Continues an established appearances/front-matter section without dialogue anchors."
                    .to_string(),
            );
        } else {
            appearances_open = false;
        }
    }

    // Preserve a strong word-index run through trailing index pages. Index pages can contain many
    // small integers that superficially resemble transcript line gutters, especially near the end
    // of an alphabetical index. Continuity is used only when the page remains reference-like and
    // has no strong dialogue signal.
    let mut consecutive_word_index = 0usize;
    for index in 0..pages.len() {
        if pages[index].section == PageSection::WordIndex {
            consecutive_word_index += 1;
            continue;
        }
        if consecutive_word_index >= 3
            && matches!(
                pages[index].section,
                PageSection::Unknown | PageSection::Transcript
            )
        {
            let citations = count_page_line_citations(&pages[index].text);
            let speaker_prefixes = count_speaker_prefixes(&pages[index].text);
            let dialogue_markers = count_dialogue_markers(&pages[index].text);
            if (citations >= 3 || looks_index_like(&pages[index].text))
                && speaker_prefixes < 2
                && dialogue_markers < 2
            {
                pages[index].section = PageSection::WordIndex;
                pages[index].section_confidence = 0.90;
                pages[index].section_reasons.push(
                    "Continues an established word-index run without dialogue evidence."
                        .to_string(),
                );
                consecutive_word_index += 1;
                continue;
            }
        }
        consecutive_word_index = if pages[index].section == PageSection::WordIndex {
            1
        } else {
            0
        };
    }

    // Smooth isolated unknown pages inside a strong transcript run. A reporter may have a page
    // with mostly a long parenthetical/redaction and too few numbered rows for local detection.
    if pages.len() >= 3 {
        for index in 1..pages.len() - 1 {
            if pages[index].section != PageSection::Unknown {
                continue;
            }
            if pages[index - 1].section == PageSection::Transcript
                && pages[index + 1].section == PageSection::Transcript
            {
                pages[index].section = PageSection::Transcript;
                pages[index].section_confidence = 0.72;
                pages[index]
                    .section_reasons
                    .push("Sandwiched between transcript pages.".to_string());
            }
        }
    }

    // Pull dialogue-like unknown pages into an established transcript run when either neighbor is
    // already strong transcript evidence. This handles parenthetical/redaction-heavy pages without
    // swallowing indexes or certificates, which have already been classified above.
    for _ in 0..2 {
        let snapshot: Vec<PageSection> = pages.iter().map(|page| page.section).collect();
        for index in 0..pages.len() {
            if pages[index].section != PageSection::Unknown
                || !looks_dialogue_like(&pages[index].text)
            {
                continue;
            }
            let previous_is_transcript =
                index > 0 && snapshot[index - 1] == PageSection::Transcript;
            let next_is_transcript =
                index + 1 < snapshot.len() && snapshot[index + 1] == PageSection::Transcript;
            if previous_is_transcript || next_is_transcript {
                pages[index].section = PageSection::Transcript;
                pages[index].section_confidence = 0.64;
                pages[index].section_reasons.push(
                    "Dialogue-like page adjacent to an established transcript run.".to_string(),
                );
            }
        }
    }

    // If a document has no explicit transcript pages at all, do not aggressively exclude unknown
    // pages. This preserves older/simple text transcripts and unusual reporter layouts.
    let transcript_count = pages
        .iter()
        .filter(|page| page.section == PageSection::Transcript)
        .count();
    if transcript_count == 0 {
        for page in pages.iter_mut() {
            if page.section == PageSection::Unknown && looks_dialogue_like(&page.text) {
                page.section = PageSection::Transcript;
                page.section_confidence = 0.52;
                page.section_reasons.push(
                    "No strong transcript run existed; dialogue-like structure used as conservative fallback."
                        .to_string(),
                );
            }
        }
    }
}

fn classify_page(page: &ExtractedPage) -> (PageSection, f32, Vec<String>) {
    let upper = page.text.to_ascii_uppercase();
    let numbered_rows = numbered_rows_from_page(page);
    let sequential_score = sequential_line_score(&numbered_rows);
    let citation_count = count_page_line_citations(&page.text);
    let speaker_prefix_count = count_speaker_prefixes(&page.text);
    let dialogue_marker_count = count_dialogue_markers(&page.text);
    let mut reasons = Vec::new();

    let heading_lines: Vec<String> = page
        .text
        .lines()
        .map(|l| {
            strip_leading_line_number(l.trim())
                .trim()
                .to_ascii_uppercase()
        })
        .filter(|l| !l.is_empty() && !l.chars().all(|c| c.is_ascii_digit()))
        .take(8)
        .collect();
    let has_heading = |prefix: &str| heading_lines.iter().any(|l| l.starts_with(prefix));
    if has_heading("CERTIFICATE OF")
        || has_heading("REPORTER'S CERTIFICATE")
        || has_heading("CERTIFICATION OF")
    {
        return (
            PageSection::Certificate,
            0.96,
            vec!["Certificate heading; blank numbered lines are not testimony.".to_string()],
        );
    }
    if has_heading("ERRATA") {
        return (
            PageSection::Errata,
            0.96,
            vec!["Errata heading.".to_string()],
        );
    }
    if has_heading("APPEARANCES") && speaker_prefix_count == 0 && dialogue_marker_count == 0 {
        return (
            PageSection::Appearances,
            0.97,
            vec!["Appearances heading.".to_string()],
        );
    }
    if page.physical_page <= 3
        && (has_heading("INTERVIEW OF")
            || has_heading("DEPOSITION OF")
            || has_heading("TRANSCRIPT OF"))
        && speaker_prefix_count == 0
        && dialogue_marker_count == 0
    {
        return (
            PageSection::Cover,
            0.96,
            vec!["Document title; numbering does not establish dialogue.".to_string()],
        );
    }
    if speaker_prefix_count >= 2 || dialogue_marker_count >= 2 {
        return (
            PageSection::Transcript,
            0.92,
            vec![
                "Explicit speaker/Q-A anchors establish dialogue, including unnumbered formats."
                    .to_string(),
            ],
        );
    }

    if upper.contains("WORD INDEX")
        || upper.contains("WORD/PHRASE INDEX")
        || (citation_count >= 10 && numbered_rows.len() <= 5 && looks_index_like(&page.text))
    {
        reasons.push(format!(
            "Index evidence: {citation_count} page:line citations and {} leading transcript-line rows.",
            numbered_rows.len()
        ));
        return (PageSection::WordIndex, 0.96, reasons);
    }

    if (upper.contains("EXHIBIT INDEX") || upper.contains("INDEX OF EXHIBITS"))
        || (upper.contains("EXHIBIT") && upper.contains("DESCRIPTION") && numbered_rows.len() < 8)
    {
        reasons.push("Exhibit-index heading/column structure detected.".to_string());
        return (PageSection::ExhibitIndex, 0.94, reasons);
    }

    if upper.contains("CERTIFICATE")
        && (upper.contains("CERTIFICATE OF TRANSCRIPTION")
            || upper.contains("I CERTIFY")
            || upper.contains("COURT REPORTER")
            || upper.contains("NOTARY"))
        && numbered_rows.len() < 8
    {
        reasons.push("Reporter/notary certificate language detected.".to_string());
        return (PageSection::Certificate, 0.92, reasons);
    }

    if (upper.contains("ERRATA SHEET")
        || (upper.contains("ERRATA") && upper.contains("PAGE") && upper.contains("LINE")))
        && numbered_rows.len() < 8
    {
        reasons.push("Errata/correction-sheet structure detected.".to_string());
        return (PageSection::Errata, 0.92, reasons);
    }

    if upper.contains("ATTACHMENT") && numbered_rows.len() < 8 {
        reasons.push("Attachment heading detected.".to_string());
        return (PageSection::Attachment, 0.84, reasons);
    }

    if upper.contains("APPENDIX") && numbered_rows.len() < 8 {
        reasons.push("Appendix heading detected.".to_string());
        return (PageSection::Appendix, 0.86, reasons);
    }

    if (upper.contains("CASE NO.") || upper.contains("CASE NUMBER"))
        && (upper.contains("COURT") || upper.contains("DISTRICT"))
        && numbered_rows.len() < 6
        && speaker_prefix_count == 0
        && dialogue_marker_count == 0
    {
        reasons.push("Court-caption structure detected.".to_string());
        return (PageSection::Caption, 0.82, reasons);
    }

    if upper.contains("APPEARANCES") && numbered_rows.len() < 8 {
        reasons.push("Appearances/front-matter heading detected.".to_string());
        return (PageSection::Appearances, 0.97, reasons);
    }

    if dialogue_marker_count >= 4 {
        reasons.push(format!(
            "Strong Q/A dialogue marker evidence: {dialogue_marker_count} Q/A-prefixed rows."
        ));
        return (PageSection::Transcript, 0.90, reasons);
    }

    if numbered_rows.len() >= 8 && sequential_score >= 0.62 {
        reasons.push(format!(
            "Strong transcript line-number column: {} rows, sequential score {:.2}.",
            numbered_rows.len(),
            sequential_score
        ));
        return (PageSection::Transcript, 0.97, reasons);
    }

    if numbered_rows.len() >= 5 && speaker_prefix_count >= 2 && sequential_score >= 0.45 {
        reasons.push(format!(
            "Transcript-like numbered dialogue: {} numbered rows and {speaker_prefix_count} speaker prefixes.",
            numbered_rows.len()
        ));
        return (PageSection::Transcript, 0.88, reasons);
    }

    if (upper.contains("DEPOSITION OF")
        || upper.contains("INTERVIEW OF")
        || upper.contains("TRANSCRIPT OF"))
        && numbered_rows.len() < 8
    {
        reasons.push("Transcript-family cover heading detected.".to_string());
        return (PageSection::Cover, 0.90, reasons);
    }

    if numbered_rows.len() >= 4 && sequential_score >= 0.50 {
        reasons.push(format!(
            "Moderate transcript line-number sequence: {} rows, score {:.2}.",
            numbered_rows.len(),
            sequential_score
        ));
        return (PageSection::Transcript, 0.70, reasons);
    }

    reasons.push(format!(
        "No decisive section signature: {} numbered rows, {citation_count} citations, {speaker_prefix_count} speaker prefixes, {dialogue_marker_count} Q/A markers.",
        numbered_rows.len()
    ));
    (PageSection::Unknown, 0.35, reasons)
}

fn numbered_rows_from_page(page: &ExtractedPage) -> Vec<u16> {
    if !page.visual_rows.is_empty() {
        return page
            .visual_rows
            .iter()
            .filter_map(|row| row.line_number)
            .collect();
    }

    page.text
        .lines()
        .filter_map(|line| leading_transcript_line_number(line.trim()))
        .collect()
}

fn sequential_line_score(numbers: &[u16]) -> f32 {
    if numbers.len() < 2 {
        return 0.0;
    }
    let transitions = numbers.len() - 1;
    let good = numbers
        .windows(2)
        .filter(|pair| pair[1] == pair[0].saturating_add(1))
        .count();
    good as f32 / transitions as f32
}

fn count_page_line_citations(text: &str) -> usize {
    text.split_whitespace()
        .filter(|token| looks_like_page_line_citation(token))
        .count()
}

fn looks_like_page_line_citation(token: &str) -> bool {
    let clean = token.trim_matches(|character: char| {
        matches!(character, ',' | ';' | '(' | ')' | '[' | ']' | '.' | ':')
    });
    let Some((page, line)) = clean.split_once(':') else {
        return false;
    };
    !page.is_empty()
        && !line.is_empty()
        && page.chars().all(|character| character.is_ascii_digit())
        && line.chars().all(|character| character.is_ascii_digit())
        && line
            .parse::<u16>()
            .ok()
            .is_some_and(|value| (1..=50).contains(&value))
}

fn looks_index_like(text: &str) -> bool {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return false;
    }
    let citation_lines = lines
        .iter()
        .filter(|line| line.split_whitespace().any(looks_like_page_line_citation))
        .count();
    citation_lines as f32 / lines.len() as f32 >= 0.20
}

fn count_speaker_prefixes(text: &str) -> usize {
    text.lines()
        .filter(|line| {
            let content = strip_leading_line_number(line.trim());
            crate::transcript::speaker::split_colon_label(content).is_some()
                || crate::transcript::speaker::split_titled_speaker_prefix(content).is_some()
        })
        .count()
}

fn looks_like_titled_speaker_row(text: &str) -> bool {
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
    ];
    let Some(prefix_len) = prefixes
        .iter()
        .find_map(|prefix| upper.starts_with(prefix).then_some(prefix.len()))
    else {
        return false;
    };
    let Some(relative_end) = trimmed.get(prefix_len..).and_then(|rest| rest.find('.')) else {
        return false;
    };
    let label_end = prefix_len + relative_end;
    let Some(label) = trimmed.get(..label_end) else {
        return false;
    };
    let Some(remainder) = trimmed.get(label_end + 1..) else {
        return false;
    };
    !remainder.trim().is_empty()
        && label.split_whitespace().count() <= 6
        && !label.chars().any(|character| character.is_ascii_digit())
}

fn count_dialogue_markers(text: &str) -> usize {
    text.lines()
        .filter(|line| {
            let content = strip_leading_line_number(line.trim()).trim_start();
            let upper = content.to_ascii_uppercase();
            upper.starts_with("Q.")
                || upper.starts_with("A.")
                || upper.starts_with("Q ")
                || upper.starts_with("A ")
                || upper.starts_with("Q:")
                || upper.starts_with("A:")
                || upper.starts_with("QUESTION:")
                || upper.starts_with("ANSWER:")
        })
        .count()
}

fn looks_dialogue_like(text: &str) -> bool {
    let speaker_prefixes = count_speaker_prefixes(text);
    let numbered = text
        .lines()
        .filter(|line| leading_transcript_line_number(line.trim()).is_some())
        .count();
    let dialogue_markers = count_dialogue_markers(text);
    speaker_prefixes >= 3 || numbered >= 5 || dialogue_markers >= 4
}

fn strip_leading_line_number(text: &str) -> &str {
    let digit_count = text
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .count();
    if digit_count == 0 {
        return text;
    }
    let (digits, rest) = text.split_at(digit_count);
    if digits
        .parse::<u16>()
        .ok()
        .is_some_and(|value| (1..=50).contains(&value))
    {
        rest.trim_start()
    } else {
        text
    }
}

fn looks_like_speaker_label(label: &str) -> bool {
    let label = label.trim();
    if label.is_empty() || label.len() > 80 {
        return false;
    }
    let letters: Vec<char> = label
        .chars()
        .filter(|character| character.is_alphabetic())
        .collect();
    if letters.len() < 2 {
        return false;
    }
    let uppercase = letters
        .iter()
        .filter(|character| character.is_uppercase())
        .count();
    uppercase as f32 / letters.len() as f32 >= 0.72
}

fn object_to_f64(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(*value as f64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citation_detector_does_not_treat_line_number_as_citation() {
        assert!(looks_like_page_line_citation("128:15"));
        assert!(!looks_like_page_line_citation("15"));
    }

    #[test]
    fn sequence_score_prefers_transcript_columns() {
        assert!(sequential_line_score(&[1, 2, 3, 4, 5, 6]) > 0.95);
        assert!(sequential_line_score(&[1, 8, 14, 22, 31]) < 0.10);
    }
}
