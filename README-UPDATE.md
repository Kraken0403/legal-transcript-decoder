# Legal Transcript Decoder — parser update

Extract this ZIP into the existing project root and overwrite the included files. The archive contains changed/new files only. Keep your other project files. No build, cargo check, cargo run, or cargo test was performed while preparing this update.

## Required setup

The parser and HTTP app remain Rust. Reliable PDF coordinates come from a bundled **local Python adapter** using pdfplumber and PDFium. Install Python 3.11 or newer, then run from the project directory:

```powershell
python -m pip install --only-binary=:all: -r requirements-parser.txt
```

For scanned PDFs, install Tesseract with its English language data and make `tesseract` available on PATH. No cloud OCR or AI service receives the PDF. If Python is outside PATH, set `TRANSCRIPT_PYTHON` to the full executable path. Optional variables:

- `TRANSCRIPT_TESSERACT`: full Tesseract executable path.
- `TRANSCRIPT_OCR_LANGUAGE`: installed Tesseract language codes, default `eng`.

The Rust bridge includes `src/document/pdf_adapter.py` at compile time. Keep that file in the project. Cargo.toml and Cargo.lock now declare `serde_json` and `sha2` directly; their versions were already present transitively in your lockfile.

If the coordinate adapter is unavailable, the old native extraction remains a fallback and produces review flags. Scanned text cannot be recovered without working OCR. OCR output always requires visual review.

## Main changes

- Speaker-format detection removes proven line gutters before reading labels. Period labels, colon labels, Q/A markers, and transitions between them can coexist.
- Font-aware glyph coordinates replace estimated character widths on the main PDF path. This preserves words, hyphens, punctuation, line gutters, and low-numbered page labels more accurately.
- Left/right gutters, common numbered two-/four-pane layouts, sparse continuation pages, nested PDF forms, and scanned pages have coordinate extraction support. Multiple panes/uncertain reading order remain review-gated.
- Appearance lists, covers, certificates, indexes, and unknown material remain in the source inventory. Numbered blanks do not become missing testimony.
- Unknown unnumbered text is retained. Repeated answers are not discarded as boilerplate. Body numbers are stripped only when they are proven gutter numbers, and only once.
- Explicit witness introductions change the active witness. Redacted examiner IDs represent observed examination scopes, not recovered identities.
- Removed context-only recovery that manufactured missing questions/answers. Missing source regions remain unresolved and reviewable.
- Lines, blocks, exchanges, graphs, and the AI-facing IR share the same parsed blocks. The old independent IR parser is removed.
- Source gaps and structural boundaries stop inappropriate Q/A links. Short interjections are no longer automatically treated as interruptions.
- The upload page offers a **complete JSON download** and displays review status. Extraction and parsing run on a blocking worker, with at most two concurrent processing tasks.

## Output contract

`schema_version` is `2.0`; existing top-level collections remain available.

- `source_sha256`: SHA-256 of the exact uploaded bytes, populated by `/upload`.
- `source_pages`: all physical pages, extracted text, visual rows, coordinates, layout/OCR diagnostics, and redaction candidates.
- `omni.canonical_rows`: complete extracted row inventory, including front matter, references, page furniture, blank numbered lines, dialogue, and unclassified rows.
- `lines[].source_row_id`, `blocks[].source_row_ids`, and `omni.utterances[].source_spans[].row_ids`: links back to canonical rows.
- `transcript_page_count`: physical-page/pane pairs containing parsed dialogue. This is distinct from the count of detected printed page labels; both parser views use the same definition.
- `quality.status`: `passed_structural_checks`, `needs_review`, or `blocked`.
- `quality.ready_for_ai`: structural gate only. It is false when extraction, identity, source accounting, OCR, or section issues require review.

On the coordinate adapter path, row `bbox` is `[left, top, right, bottom]` in displayed-page points. Fragment `x` is left-to-right; fragment `y` is negative bottom, preserving the existing row-sort convention. `panel` is zero-based within the physical page; `printed_page` is the observed printed label when available. Native fallback geometry is estimated and explicitly flagged. Original PDF bytes remain the final visual reference. `raw_text` is an extraction representation, not a byte-for-byte transcript or proof of visual correctness; detected redacted text is withheld from that representation as well.

Use `omni.utterances` and their exact source-row references for the next AI layer. Do not equate confidence scores or legacy `source_completeness` (number-grid occupancy) with factual accuracy. General entity discovery is deferred to the AI layer; only observed participant entities are populated at this stage.

## Verification performed

- Rust syntax parsing and source-struct field consistency checks, without compilation.
- Actual extraction of all 111 physical pages of the supplied PDF; coordinate rows preserved printed page labels and corrected the demonstrated spacing issues.
- Python extraction regressions cover proportional-font spacing, numeric testimony, right gutters, two/four panes, unnumbered speaker columns, nested PDF forms, redaction overlays, blank pages, sparse pages, and image-only OCR.
- Rust regression cases were added for mixed label formats, wrapped unnumbered dialogue, repeated answers, blank line numbers, source gaps, witness switches, reference retention, numeric preservation, and the redacted-examiner case. **They were not executed**, because Rust tests would compile the project.

The optional Python regression suite requires `reportlab` in addition to runtime packages:

```powershell
python -m pip install --only-binary=:all: reportlab
python -m unittest discover -s tools -p test_pdf_adapter.py -v
```

A Rust type-check/runtime pass and a broader, independently annotated transcript corpus are still needed before production reliance. There is no honest guarantee of perfect parsing for every PDF. The current grammar targets English transcript conventions; handwriting, unusual layouts, non-English labels, damaged encoding, visual overlays, and OCR ambiguity can require review. Encrypted PDFs need an unlocked copy. Default limits are 50 MiB uploads, 3000 pages, 250000 glyphs per page, a 256 MiB adapter result, and a 10-minute adapter timeout; split unusually large documents into volumes.

References for the local extraction interfaces: [pdfplumber](https://github.com/jsvine/pdfplumber), [pypdfium2](https://pypdfium2.readthedocs.io/), [Tesseract TSV output](https://tesseract-ocr.github.io/tessdoc/Command-Line-Usage.html).
