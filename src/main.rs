mod document;
mod transcript;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Multipart},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post},
};

use document::extract_document;
use transcript::{TranscriptProfile, parse_transcript, preflight_transcript};

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/", get(index))
        .route("/upload", post(upload_transcript))
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .expect("Could not start TCP listener");

    println!("Legal Transcript Decoder is running!");
    println!("Open http://localhost:3000");

    axum::serve(listener, app).await.expect("Server failed");
}

async fn index() -> Html<&'static str> {
    Html(
        r#"
<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>Legal Transcript Decoder</title>
    <style>
        * { box-sizing: border-box; }
        body {
            font-family: Arial, Helvetica, sans-serif;
            max-width: 780px;
            margin: 70px auto;
            padding: 20px;
            background: #f5f5f5;
            color: #111;
        }
        .card {
            background: white;
            padding: 32px;
            border-radius: 12px;
            box-shadow: 0 4px 20px rgba(0,0,0,0.08);
        }
        h1 { margin-top: 0; }
        p, li { line-height: 1.55; }
        .note { color: #666; font-size: 14px; }
        .features {
            margin: 20px 0;
            padding: 18px;
            border: 1px solid #ddd;
            border-radius: 8px;
            background: #fafafa;
        }
        input {
            display: block;
            width: 100%;
            margin: 24px 0;
            padding: 12px;
            border: 1px solid #ccc;
            border-radius: 6px;
        }
        button {
            background: #111;
            color: white;
            border: 0;
            border-radius: 6px;
            padding: 12px 20px;
            cursor: pointer;
        }
    </style>
</head>
<body>
    <div class="card">
        <h1>Legal Transcript Decoder</h1>
        <p>Position-aware transcript reconstruction: layout + adaptive preflight + participant resolver + verifier + graph.</p>
        <p class="note">
            The engine learns this transcript's format, participants and Q/A conventions, preserves source gaps/redactions,
            reconstructs multi-speaker conversation, verifies the result, and exposes uncertainty instead of guessing.
            OCR and unresolved source regions are marked for review before analysis.
        </p>
        <div class="features">
            <strong>Current pipeline</strong>
            <ul>
                <li>PDF/TXT extraction with physical page preservation</li>
                <li>Position-aware PDF text reconstruction (X/Y visual rows) with safe plain-text fallback</li>
                <li>Automatic cover / appearances / transcript / word-index / exhibit-index / certificate separation</li>
                <li>PDF metadata extraction</li>
                <li>Adaptive Q./A. → Q/A → colon/word marker priority</li>
                <li>Participant and likely examiner discovery</li>
                <li>Missing transcript line/page detection</li>
                <li>Multiple attorneys, witnesses, interviewers, interviewees, counsel, law-enforcement, interpreters, reporters and videographers</li>
                <li>Explicit redaction annotations + dark vector/annotation candidates</li>
                <li>Unknown source text retained with review flags</li>
                <li>Stable participant IDs, including anonymous/redacted speakers</li>
                <li>Evidence-backed classifications and independent verification</li>
                <li>Conversation graph for responses, interruptions and resumptions</li>
            </ul>
        </div>
        <form id="upload-form" action="/upload" method="post" enctype="multipart/form-data">
            <input
                type="file"
                name="transcript"
                accept=".txt,.pdf,text/plain,application/pdf"
                required
            />
            <button type="submit">Preflight + Parse Transcript</button>
        </form>
        <p id="status" role="status" aria-live="polite"></p>
        <a id="download" hidden>Download complete JSON</a>
        <pre id="review" style="white-space:pre-wrap;overflow-wrap:anywhere"></pre>
    </div>
    <script>
    const form=document.getElementById('upload-form');
    let downloadUrl=null;
    form.addEventListener('submit',async event=>{
        event.preventDefault();
        const button=form.querySelector('button');
        const status=document.getElementById('status');
        const link=document.getElementById('download');
        button.disabled=true;link.hidden=true;document.getElementById('review').textContent='';
        status.textContent='Extracting and parsing all pages. Scanned documents can take several minutes.';
        try {
            const response=await fetch('/upload',{method:'POST',body:new FormData(form)});
            if(!response.ok)throw new Error(await response.text());
            const blob=await response.blob();
            const result=JSON.parse(await blob.text());
            if(downloadUrl)URL.revokeObjectURL(downloadUrl);
            downloadUrl=URL.createObjectURL(blob);link.href=downloadUrl;
            link.download='transcript-parsed.json';link.hidden=false;
            status.textContent=`${result.physical_page_count} physical pages; ${result.line_count} dialogue rows; ${result.blocks.length} blocks. Status: ${result.quality.status}.`;
            const issues=result.quality.issues;
            document.getElementById('review').textContent=issues.length
              ? `${issues.length} review flags. The complete list is in the JSON.\n\n`+issues.slice(0,12).map(i=>`${i.physical_page?'Page '+i.physical_page+': ':''}${i.message}`).join('\n')
              : 'Structural checks passed. Review source accuracy before relying on the analysis.';
        }catch(error){status.textContent=error.message;}finally{button.disabled=false;}
    });
    </script>
</body>
</html>
"#,
    )
}

async fn upload_transcript(mut multipart: Multipart) -> impl IntoResponse {
    let mut uploaded_filename: Option<String> = None;
    let mut uploaded_bytes = None;

    while let Ok(Some(field)) = multipart.next_field().await {
        if field.name() != Some("transcript") {
            continue;
        }

        let filename = field.file_name().unwrap_or("transcript").to_string();
        let bytes = match field.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("Failed to read uploaded file: {error}");
                return (
                    StatusCode::BAD_REQUEST,
                    "Could not read uploaded file.".to_string(),
                )
                    .into_response();
            }
        };

        uploaded_filename = Some(filename);
        uploaded_bytes = Some(bytes);
        break;
    }

    let filename = match uploaded_filename {
        Some(filename) => filename,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "No transcript file was uploaded.".to_string(),
            )
                .into_response();
        }
    };

    let bytes = match uploaded_bytes {
        Some(bytes) => bytes,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "Could not read transcript contents.".to_string(),
            )
                .into_response();
        }
    };

    static PROCESSING_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
    let permit = match PROCESSING_SLOTS.try_acquire() {
        Ok(permit) => permit,
        Err(_) => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                "Two documents are already being processed. Retry when one completes.",
            )
                .into_response();
        }
    };
    let result = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let _permit = permit;
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(bytes.as_ref())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let extracted = extract_document(&filename, bytes.as_ref()).map_err(|e| e.to_string())?;
        let profile = TranscriptProfile::us_english();
        let context = preflight_transcript(&extracted, &profile);
        let mut parsed = parse_transcript(filename, &extracted, &context, &profile)
            .map_err(|e| e.to_string())?;
        parsed.source_sha256 = Some(digest);
        Ok(parsed)
    })
    .await;
    match result {
        Ok(Ok(parsed)) => (StatusCode::OK, Json(parsed)).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, error).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Document processing failed; no partial parse is certified.",
        )
            .into_response(),
    }
}
