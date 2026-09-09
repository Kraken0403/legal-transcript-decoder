use lopdf::{Document, LoadOptions, Object};

use super::{
    error::DocumentError,
    layout::{
        apply_visual_redaction_masks, classify_page_sections, extract_positioned_fragments,
        reconstruct_visual_rows,
    },
    models::{
        AnnotationSignal, DocumentKind, DocumentMetadata, ExtractedDocument, ExtractedPage,
        FilledRectangleSignal, PageLayoutDiagnostics, PageSection, VisualRow,
    },
};

const MAX_PDF_STREAM_SIZE: usize = 64 * 1024 * 1024;
const MAX_PAGE_TEXT_SIZE: usize = 16 * 1024 * 1024;
const MAX_RECTANGLE_SIGNALS_PER_PAGE: usize = 250;

#[derive(Debug, Clone)]
enum FillColor {
    Gray(f64),
    Rgb(f64, f64, f64),
    Cmyk(f64, f64, f64, f64),
    Unknown,
}

impl FillColor {
    fn components(&self) -> Vec<f64> {
        match self {
            Self::Gray(g) => vec![*g],
            Self::Rgb(r, g, b) => vec![*r, *g, *b],
            Self::Cmyk(c, m, y, k) => vec![*c, *m, *y, *k],
            Self::Unknown => Vec::new(),
        }
    }

    fn color_space(&self) -> &'static str {
        match self {
            Self::Gray(_) => "gray",
            Self::Rgb(_, _, _) => "rgb",
            Self::Cmyk(_, _, _, _) => "cmyk",
            Self::Unknown => "unknown",
        }
    }

    fn is_dark(&self) -> bool {
        match self {
            Self::Gray(g) => *g <= 0.12,
            Self::Rgb(r, g, b) => *r <= 0.12 && *g <= 0.12 && *b <= 0.12,
            Self::Cmyk(c, m, y, k) => *k >= 0.75 && *c <= 0.35 && *m <= 0.35 && *y <= 0.35,
            Self::Unknown => false,
        }
    }
}

pub fn extract_document(filename: &str, bytes: &[u8]) -> Result<ExtractedDocument, DocumentError> {
    let lowercase_filename = filename.to_ascii_lowercase();

    if lowercase_filename.ends_with(".txt") {
        return extract_text_document(bytes);
    }

    if lowercase_filename.ends_with(".pdf") {
        return extract_pdf_document(bytes);
    }

    Err(DocumentError::UnsupportedFileType)
}

fn extract_text_document(bytes: &[u8]) -> Result<ExtractedDocument, DocumentError> {
    let decoded;
    let text = if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        let little = bytes[0] == 0xff;
        if (bytes.len() - 2) % 2 != 0 {
            return Err(DocumentError::InvalidUtf8);
        }
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|b| {
                if little {
                    u16::from_le_bytes([b[0], b[1]])
                } else {
                    u16::from_be_bytes([b[0], b[1]])
                }
            })
            .collect();
        decoded = String::from_utf16(&units).map_err(|_| DocumentError::InvalidUtf8)?;
        decoded.as_str()
    } else {
        std::str::from_utf8(bytes)
            .map_err(|_| DocumentError::InvalidUtf8)?
            .trim_start_matches('\u{feff}')
    };
    let pages = split_text_pages(text);

    let mut pages = pages;
    classify_page_sections(&mut pages);

    Ok(ExtractedDocument {
        kind: DocumentKind::Text,
        metadata: DocumentMetadata::default(),
        pages,
    })
}

fn extract_pdf_document(bytes: &[u8]) -> Result<ExtractedDocument, DocumentError> {
    if !bytes.starts_with(b"%PDF-") {
        return Err(DocumentError::InvalidPdfHeader);
    }

    let metadata = load_pdf_metadata(bytes);
    let adapter_error = match super::coordinate::extract(bytes) {
        Ok(mut pages) => {
            if metadata
                .declared_page_count
                .is_some_and(|count| count as usize != pages.len())
            {
                return Err(DocumentError::PdfLoad(
                    "Coordinate adapter page count disagrees with PDF page tree".to_string(),
                ));
            }
            classify_page_sections(&mut pages);
            return Ok(ExtractedDocument {
                kind: DocumentKind::Pdf,
                metadata,
                pages,
            });
        }
        Err(error) => error,
    };

    let options = LoadOptions::with_max_decompressed_size(MAX_PDF_STREAM_SIZE);
    let document = Document::load_mem_with_options(bytes, options)
        .map_err(|error| DocumentError::PdfLoad(error.to_string()))?;

    let pdf_pages = document.get_pages();
    if pdf_pages.is_empty() {
        return Err(DocumentError::NoExtractableText);
    }

    let mut pages = Vec::with_capacity(pdf_pages.len());

    for (page_number, page_id) in pdf_pages {
        let (page_text, plain_text_error) =
            match document.extract_text_with_limit(&[page_number], MAX_PAGE_TEXT_SIZE) {
                Ok(text) => (text, None),
                Err(error) => (String::new(), Some(error.to_string())),
            };

        let annotations = inspect_annotations(&document, page_id, page_number);
        let filled_rectangles = inspect_filled_rectangles(&document, page_id, page_number);
        let image_count = document
            .get_page_images(page_id)
            .map(|images| images.len())
            .unwrap_or(0);

        let fallback_text = normalize_pdf_page(&page_text);
        let (mut positioned_fragments, positioned_text_error) =
            match extract_positioned_fragments(&document, page_id) {
                Ok(fragments) => (fragments, None),
                Err(error) => (Vec::new(), Some(error)),
            };
        let masked_redaction_fragments = apply_visual_redaction_masks(
            &mut positioned_fragments,
            &annotations,
            &filled_rectangles,
        );
        let (visual_rows, reconstructed_text, mut layout) =
            reconstruct_visual_rows(positioned_fragments, &fallback_text);
        layout.masked_redaction_fragments = masked_redaction_fragments;
        layout.extraction_engine = "lopdf_fallback".to_string();
        layout.warnings.push(adapter_error.clone());
        layout.warnings.push("Fallback geometry is estimated and may omit nested Form XObjects, rotated text, or image-only content. Review required.".to_string());
        layout.reconstruction_confidence = layout.reconstruction_confidence.min(0.65);

        let mut extraction_warnings = Vec::new();
        if let Some(error) = plain_text_error {
            extraction_warnings.push(format!("Plain text extraction failed: {error}"));
        }
        if let Some(error) = positioned_text_error {
            extraction_warnings.push(format!("Positioned text extraction failed: {error}"));
        }
        if !extraction_warnings.is_empty() {
            let warning = extraction_warnings.join(" | ");
            layout.fallback_reason = Some(match layout.fallback_reason.take() {
                Some(existing) => format!("{existing} {warning}"),
                None => warning,
            });
        }

        if reconstructed_text.trim().is_empty() && image_count > 0 {
            layout.requires_ocr = true;
            layout.fallback_reason = Some(
                "Page contains images but no reliable native text. Install the local OCR dependencies or supply a reviewed text transcription."
                    .to_string(),
            );
        }

        pages.push(ExtractedPage {
            physical_page: page_number,
            text: reconstructed_text,
            raw_text: if masked_redaction_fragments > 0 {
                "[RAW TEXT WITHHELD: visual redaction masks present]".to_string()
            } else {
                fallback_text
            },
            section: PageSection::Unknown,
            section_confidence: 0.0,
            section_reasons: Vec::new(),
            visual_rows,
            layout,
            annotations,
            filled_rectangles,
            image_count,
        });
    }

    classify_page_sections(&mut pages);

    Ok(ExtractedDocument {
        kind: DocumentKind::Pdf,
        metadata,
        pages,
    })
}

fn load_pdf_metadata(bytes: &[u8]) -> DocumentMetadata {
    match Document::load_metadata_mem(bytes) {
        Ok(metadata) => DocumentMetadata {
            title: sanitize_metadata_text(metadata.title),
            author: sanitize_metadata_text(metadata.author),
            subject: sanitize_metadata_text(metadata.subject),
            keywords: sanitize_metadata_text(metadata.keywords),
            creator: sanitize_metadata_text(metadata.creator),
            producer: sanitize_metadata_text(metadata.producer),
            creation_date: sanitize_metadata_text(metadata.creation_date),
            modification_date: sanitize_metadata_text(metadata.modification_date),
            pdf_version: sanitize_metadata_text(Some(metadata.version)),
            encrypted: metadata.encrypted,
            declared_page_count: Some(metadata.page_count),
        },
        Err(_) => DocumentMetadata::default(),
    }
}

fn sanitize_metadata_text(value: Option<String>) -> Option<String> {
    let cleaned = value?
        .chars()
        .filter(|character| *character != '\0')
        .collect::<String>()
        .trim()
        .to_string();

    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

fn inspect_annotations(
    document: &Document,
    page_id: (u32, u16),
    physical_page: u32,
) -> Vec<AnnotationSignal> {
    let Ok(annotations) = document.get_page_annotations(page_id) else {
        return Vec::new();
    };

    annotations
        .into_iter()
        .map(|annotation| {
            let subtype = annotation
                .get(b"Subtype")
                .ok()
                .and_then(|value| value.as_name().ok())
                .map(|name| String::from_utf8_lossy(name).to_string())
                .unwrap_or_else(|| "Unknown".to_string());

            let rect = annotation.get(b"Rect").ok().and_then(object_to_rect);
            let color_components = annotation
                .get(b"IC")
                .ok()
                .or_else(|| annotation.get(b"C").ok())
                .map(object_to_number_array)
                .unwrap_or_default();

            let is_explicit_redaction = subtype.eq_ignore_ascii_case("Redact");
            let dark_color = color_is_dark(&color_components);
            let overlay_like_subtype = matches!(
                subtype.to_ascii_lowercase().as_str(),
                "square" | "highlight" | "polygon" | "redact"
            );

            AnnotationSignal {
                physical_page,
                subtype,
                rect,
                color_components,
                is_explicit_redaction,
                is_dark_overlay_candidate: overlay_like_subtype && dark_color,
            }
        })
        .collect()
}

fn inspect_filled_rectangles(
    document: &Document,
    page_id: (u32, u16),
    physical_page: u32,
) -> Vec<FilledRectangleSignal> {
    let Ok(content) = document.get_and_decode_page_content(page_id) else {
        return Vec::new();
    };

    let mut current_fill = FillColor::Unknown;
    let mut pending_rect: Option<[f64; 4]> = None;
    let mut signals = Vec::new();

    for operation in content.operations {
        match operation.operator.as_str() {
            "g" => {
                if let Some(g) = operation.operands.first().and_then(object_to_f64) {
                    current_fill = FillColor::Gray(g);
                }
            }
            "rg" => {
                if operation.operands.len() >= 3 {
                    if let (Some(r), Some(g), Some(b)) = (
                        object_to_f64(&operation.operands[0]),
                        object_to_f64(&operation.operands[1]),
                        object_to_f64(&operation.operands[2]),
                    ) {
                        current_fill = FillColor::Rgb(r, g, b);
                    }
                }
            }
            "k" => {
                if operation.operands.len() >= 4 {
                    if let (Some(c), Some(m), Some(y), Some(k)) = (
                        object_to_f64(&operation.operands[0]),
                        object_to_f64(&operation.operands[1]),
                        object_to_f64(&operation.operands[2]),
                        object_to_f64(&operation.operands[3]),
                    ) {
                        current_fill = FillColor::Cmyk(c, m, y, k);
                    }
                }
            }
            "re" => {
                if operation.operands.len() >= 4 {
                    let rect = [
                        object_to_f64(&operation.operands[0]),
                        object_to_f64(&operation.operands[1]),
                        object_to_f64(&operation.operands[2]),
                        object_to_f64(&operation.operands[3]),
                    ];

                    if let [Some(x), Some(y), Some(width), Some(height)] = rect {
                        pending_rect = Some([x, y, width, height]);
                    }
                }
            }
            "f" | "f*" | "B" | "B*" => {
                if let Some(rect) = pending_rect.take() {
                    let width = rect[2].abs();
                    let height = rect[3].abs();

                    // Ignore hairlines/tiny vector decorations.
                    if width >= 4.0 && height >= 2.0 {
                        let is_dark = current_fill.is_dark();
                        let possible_redaction = is_dark
                            && width >= 18.0
                            && height >= 4.0
                            && height <= 36.0
                            && width <= 520.0;

                        signals.push(FilledRectangleSignal {
                            physical_page,
                            rect,
                            color_space: current_fill.color_space().to_string(),
                            color_components: current_fill.components(),
                            is_dark,
                            possible_redaction,
                        });

                        if signals.len() >= MAX_RECTANGLE_SIGNALS_PER_PAGE {
                            break;
                        }
                    }
                }
            }
            "n" => {
                pending_rect = None;
            }
            _ => {}
        }
    }

    signals
}

fn object_to_f64(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(value) => Some(*value as f64),
        Object::Real(value) => Some(*value as f64),
        _ => None,
    }
}

fn object_to_number_array(object: &Object) -> Vec<f64> {
    object
        .as_array()
        .ok()
        .map(|items| items.iter().filter_map(object_to_f64).collect())
        .unwrap_or_default()
}

fn object_to_rect(object: &Object) -> Option<[f64; 4]> {
    let values = object_to_number_array(object);
    if values.len() < 4 {
        return None;
    }

    Some([values[0], values[1], values[2], values[3]])
}

fn color_is_dark(components: &[f64]) -> bool {
    match components {
        [gray] => *gray <= 0.12,
        [r, g, b] => *r <= 0.12 && *g <= 0.12 && *b <= 0.12,
        [c, m, y, k] => *k >= 0.75 && *c <= 0.35 && *m <= 0.35 && *y <= 0.35,
        _ => false,
    }
}

fn normalize_pdf_page(page: &str) -> String {
    normalize_page_lines(page).join("\n")
}

fn normalize_page_lines(page: &str) -> Vec<String> {
    let raw_lines: Vec<&str> = page
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();

    let mut normalized = Vec::new();
    let mut index = 0;

    while index < raw_lines.len() {
        let current = raw_lines[index];

        if let Ok(line_number) = current.parse::<u32>()
            && (1..=50).contains(&line_number)
            && index + 1 < raw_lines.len()
        {
            normalized.push(format!("{line_number} {}", raw_lines[index + 1]));
            index += 2;
            continue;
        }

        normalized.push(normalize_attached_line_number(current));
        index += 1;
    }

    normalized
}

fn normalize_attached_line_number(line: &str) -> String {
    let digit_count = line
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .count();

    if digit_count == 0 {
        return line.to_string();
    }

    let (number_text, remaining) = line.split_at(digit_count);
    let Ok(line_number) = number_text.parse::<u32>() else {
        return line.to_string();
    };

    if !(1..=50).contains(&line_number) {
        return line.to_string();
    }

    let remaining = remaining.trim_start();
    if remaining.is_empty() {
        return line.to_string();
    }

    format!("{line_number} {remaining}")
}

fn split_text_pages(text: &str) -> Vec<ExtractedPage> {
    if text.contains('\u{000c}') {
        let mut parts: Vec<&str> = text.split('\u{000c}').collect();
        if parts.last().is_some_and(|p| p.trim().is_empty()) {
            parts.pop();
        }
        return parts
            .into_iter()
            .enumerate()
            .map(|(i, p)| empty_signal_page((i + 1) as u32, p.to_string()))
            .collect();
    }
    let mut pages = Vec::new();
    let mut current_lines = Vec::new();
    let mut physical_page = 1u32;
    let mut has_started_page = false;

    for line in text.lines() {
        let trimmed = line.trim();

        if !has_started_page && trimmed.is_empty() {
            continue;
        }

        if is_page_marker(trimmed) {
            if has_meaningful_content(&current_lines) {
                pages.push(empty_signal_page(physical_page, current_lines.join("\n")));
                physical_page += 1;
                current_lines.clear();
            }

            has_started_page = true;
            current_lines.push(line.to_string());
            continue;
        }

        if !trimmed.is_empty() {
            has_started_page = true;
        }

        if has_started_page {
            current_lines.push(line.to_string());
        }
    }

    if has_meaningful_content(&current_lines) {
        pages.push(empty_signal_page(physical_page, current_lines.join("\n")));
    }

    if pages.is_empty() && !text.trim().is_empty() {
        pages.push(empty_signal_page(1, text.trim().to_string()));
    }

    pages
}

fn empty_signal_page(physical_page: u32, text: String) -> ExtractedPage {
    let raw_text = text.clone();
    let visual_row_count = raw_text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    let numbered_row_count = raw_text
        .lines()
        .filter(|line| {
            line.trim()
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<u16>().ok())
                .is_some_and(|value| (1..=50).contains(&value))
        })
        .count();
    let visual_rows: Vec<VisualRow> = text
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let line_number = line
                .split_whitespace()
                .next()
                .and_then(|value| value.parse::<u16>().ok())
                .filter(|value| (1..=50).contains(value));
            Some(VisualRow {
                panel: 0,
                printed_page: None,
                bbox: None,
                ocr_confidence: None,
                y: -(index as f64),
                left_x: None,
                line_number_x: None,
                line_number,
                text: line.to_string(),
                fragments: Vec::new(),
            })
        })
        .collect();

    ExtractedPage {
        physical_page,
        text,
        raw_text,
        section: PageSection::Unknown,
        section_confidence: 0.0,
        section_reasons: Vec::new(),
        visual_rows,
        layout: PageLayoutDiagnostics {
            positioned_text_available: false,
            used_positioned_reconstruction: false,
            visual_row_count,
            numbered_row_count,
            reconstruction_confidence: 1.0,
            ..PageLayoutDiagnostics::default()
        },
        annotations: Vec::new(),
        filled_rectangles: Vec::new(),
        image_count: 0,
    }
}

fn has_meaningful_content(lines: &[String]) -> bool {
    lines.iter().any(|line| !line.trim().is_empty())
}

fn is_page_marker(line: &str) -> bool {
    line.to_ascii_uppercase()
        .strip_prefix("PAGE ")
        .is_some_and(|page| page.trim().parse::<u32>().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_plain_text_document() {
        let input = b"\nPAGE 74\n12 Q. When did you see the leak?\n13 A. March 2022.\n\nPAGE 75\n1 Q. Did you report it?\n2 A. Yes.\n";
        let result = extract_document("test.txt", input).expect("TXT should extract");
        assert_eq!(result.kind, DocumentKind::Text);
        assert_eq!(result.source_page_count(), 2);
    }

    #[test]
    fn combines_separate_pdf_line_numbers() {
        let result = normalize_page_lines("12\nQ. When?\n13\nA. March 2022.");
        assert_eq!(result[0], "12 Q. When?");
        assert_eq!(result[1], "13 A. March 2022.");
    }

    #[test]
    fn fixes_attached_line_numbers() {
        let result = normalize_page_lines("12Q. When?\n13A. March 2022.");
        assert_eq!(result[0], "12 Q. When?");
        assert_eq!(result[1], "13 A. March 2022.");
    }

    #[test]
    fn rejects_fake_pdf() {
        assert!(matches!(
            extract_document("fake.pdf", b"This is not a PDF"),
            Err(DocumentError::InvalidPdfHeader)
        ));
    }

    #[test]
    fn ignores_leading_blank_lines_in_txt() {
        let pages = split_text_pages("\n\nPAGE 74\n12 Q. First?\n\nPAGE 75\n1 A. Yes.");
        assert_eq!(pages.len(), 2);
    }

    #[test]
    fn text_without_markers_is_one_page() {
        let pages = split_text_pages("12 Q. Hello?\n13 A. Yes.");
        assert_eq!(pages.len(), 1);
    }
}
