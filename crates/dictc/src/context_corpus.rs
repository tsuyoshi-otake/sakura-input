//! Streaming MediaWiki article extractor for Context Prediction research.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::io::{BufReader, Read, Write};
use std::path::Path;

use quick_xml::escape::unescape;
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const ARTICLE_SCHEMA_VERSION: u16 = 1;
pub const MAX_TITLE_BYTES: usize = 4 * 1024;
pub const MAX_ARTICLE_BYTES: usize = 8 * 1024 * 1024;
pub const EXTRACTOR_ALGORITHM: &str = "mediawiki-namespace-zero-current-revision-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractedArticle {
    pub schema_version: u16,
    pub source_id: String,
    pub article_id: u64,
    pub revision_id: u64,
    pub title: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionReport {
    pub pages_seen: u64,
    pub namespace_zero_seen: u64,
    pub articles_written: u64,
    pub non_article_namespace_skipped: u64,
    pub redirects_skipped: u64,
    pub oversized_skipped: u64,
    pub incomplete_skipped: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractionResult {
    pub report: ExtractionReport,
    pub input_xml_sha256: String,
    pub output_jsonl_sha256: String,
    pub output_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusArtifact {
    pub file: String,
    pub records: u64,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusManifest {
    pub schema_version: u16,
    pub source_id: String,
    pub snapshot: String,
    pub source_manifest_sha256: String,
    pub input_role: String,
    pub input_xml_sha256: String,
    pub extractor_sha256: String,
    pub extractor_algorithm: String,
    pub max_title_bytes: usize,
    pub max_article_bytes: usize,
    pub report: ExtractionReport,
    pub artifact: CorpusArtifact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capture {
    Title,
    Namespace,
    PageId,
    RevisionId,
    Text,
}

#[derive(Default)]
struct Page {
    title: String,
    namespace: String,
    page_id: String,
    revision_id: String,
    text: String,
    revisions: u8,
    redirect: bool,
    oversized: bool,
}

/// Extracts namespace-zero, non-redirect current revisions without building an
/// XML DOM. Input and output are hashed while streaming.
pub fn extract_articles(
    input: impl Read,
    output: impl Write,
    source_id: &str,
) -> Result<ExtractionResult, String> {
    if source_id.is_empty() {
        return Err("source_id is empty".into());
    }
    let mut hashed_input = HashingReader::new(input);
    let mut hashed_output = HashingWriter::new(output);
    let report = {
        let mut xml = Reader::from_reader(BufReader::new(&mut hashed_input));
        xml.config_mut().trim_text(false);
        parse_xml(&mut xml, &mut hashed_output, source_id)?
    };
    hashed_output
        .flush()
        .map_err(|error| format!("flush extracted JSONL: {error}"))?;
    let input_xml_sha256 = hashed_input.finish();
    let (output_jsonl_sha256, output_bytes) = hashed_output.finish();
    Ok(ExtractionResult {
        report,
        input_xml_sha256,
        output_jsonl_sha256,
        output_bytes,
    })
}

pub fn commit_extraction(
    directory: &Path,
    source_id: &str,
    snapshot: &str,
    source_manifest_sha256: &str,
    extractor_sha256: &str,
    result: ExtractionResult,
) -> Result<CorpusManifest, String> {
    validate_sha256(source_manifest_sha256, "source manifest SHA-256")?;
    validate_sha256(&result.input_xml_sha256, "input XML SHA-256")?;
    validate_sha256(&result.output_jsonl_sha256, "article artifact SHA-256")?;
    validate_sha256(extractor_sha256, "extractor SHA-256")?;
    let manifest = CorpusManifest {
        schema_version: 1,
        source_id: source_id.into(),
        snapshot: snapshot.into(),
        source_manifest_sha256: source_manifest_sha256.into(),
        input_role: "decompressed-mediawiki-xml-stream".into(),
        input_xml_sha256: result.input_xml_sha256,
        extractor_sha256: extractor_sha256.into(),
        extractor_algorithm: EXTRACTOR_ALGORITHM.into(),
        max_title_bytes: MAX_TITLE_BYTES,
        max_article_bytes: MAX_ARTICLE_BYTES,
        artifact: CorpusArtifact {
            file: "articles.jsonl".into(),
            records: result.report.articles_written,
            bytes: result.output_bytes,
            sha256: result.output_jsonl_sha256,
        },
        report: result.report,
    };
    let mut bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    let path = directory.join("manifest.json");
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|error| format!("create {}: {error}", path.display()))?;
    file.write_all(&bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("write {}: {error}", path.display()))?;
    if let Err(error) = verify_extraction(directory) {
        let _ = fs::remove_file(path);
        return Err(format!(
            "generated extraction failed verification; commit marker removed: {error}"
        ));
    }
    Ok(manifest)
}

pub fn verify_extraction(directory: &Path) -> Result<CorpusManifest, String> {
    let manifest_path = directory.join("manifest.json");
    let manifest_bytes = fs::read(&manifest_path)
        .map_err(|error| format!("read {}: {error}", manifest_path.display()))?;
    let manifest: CorpusManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("invalid extraction manifest: {error}"))?;
    if manifest.schema_version != 1
        || manifest.source_id.is_empty()
        || manifest.snapshot.len() != 8
        || manifest.input_role != "decompressed-mediawiki-xml-stream"
        || manifest.extractor_algorithm != EXTRACTOR_ALGORITHM
        || manifest.max_title_bytes != MAX_TITLE_BYTES
        || manifest.max_article_bytes != MAX_ARTICLE_BYTES
        || manifest.artifact.file != "articles.jsonl"
    {
        return Err("extraction manifest contract is invalid".into());
    }
    for (value, label) in [
        (&manifest.source_manifest_sha256, "source manifest SHA-256"),
        (&manifest.input_xml_sha256, "input XML SHA-256"),
        (&manifest.extractor_sha256, "extractor SHA-256"),
        (&manifest.artifact.sha256, "article artifact SHA-256"),
    ] {
        validate_sha256(value, label)?;
    }
    if manifest.report.pages_seen
        != manifest
            .report
            .namespace_zero_seen
            .saturating_add(manifest.report.non_article_namespace_skipped)
        || manifest.report.namespace_zero_seen
            != manifest
                .report
                .articles_written
                .saturating_add(manifest.report.redirects_skipped)
                .saturating_add(manifest.report.oversized_skipped)
                .saturating_add(manifest.report.incomplete_skipped)
        || manifest.artifact.records != manifest.report.articles_written
    {
        return Err("extraction terminal-state accounting is invalid".into());
    }
    let bytes = fs::read(directory.join("articles.jsonl"))
        .map_err(|error| format!("read articles.jsonl: {error}"))?;
    if bytes.contains(&b'\r')
        || (!bytes.is_empty() && !bytes.ends_with(b"\n"))
        || bytes.len() as u64 != manifest.artifact.bytes
        || hex_digest(Sha256::digest(&bytes)) != manifest.artifact.sha256
        || bytes.iter().filter(|byte| **byte == b'\n').count() as u64 != manifest.artifact.records
    {
        return Err("article artifact does not match extraction manifest".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| "articles.jsonl is not UTF-8")?;
    let mut article_ids = BTreeSet::new();
    for (index, line) in text.lines().enumerate() {
        let record: ExtractedArticle = serde_json::from_str(line)
            .map_err(|error| format!("articles.jsonl line {}: {error}", index + 1))?;
        if record.schema_version != ARTICLE_SCHEMA_VERSION
            || record.source_id != manifest.source_id
            || record.article_id == 0
            || record.revision_id == 0
            || record.title.is_empty()
            || record.title.len() > MAX_TITLE_BYTES
            || record.text.is_empty()
            || record.text.len() > MAX_ARTICLE_BYTES
            || !article_ids.insert(record.article_id)
        {
            return Err(format!("invalid extracted article at line {}", index + 1));
        }
    }
    Ok(manifest)
}

fn validate_sha256(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(format!("{label} must be lowercase hexadecimal"));
    }
    Ok(())
}

fn parse_xml(
    xml: &mut Reader<impl std::io::BufRead>,
    output: &mut impl Write,
    source_id: &str,
) -> Result<ExtractionReport, String> {
    let mut report = ExtractionReport::default();
    let mut buffer = Vec::new();
    let mut depth = 0usize;
    let mut page_depth = None;
    let mut revision_depth = None;
    let mut page = None::<Page>;
    let mut capture = None::<(Capture, usize)>;

    loop {
        match xml.read_event_into(&mut buffer) {
            Ok(Event::Start(element)) => {
                let name = element.local_name();
                match name.as_ref() {
                    b"page" if page.is_none() => {
                        page = Some(Page::default());
                        page_depth = Some(depth);
                    }
                    b"revision" if page_depth.is_some_and(|value| depth == value + 1) => {
                        let current = page.as_mut().expect("page exists");
                        current.revisions = current.revisions.saturating_add(1);
                        revision_depth = Some(depth);
                    }
                    b"redirect" if page_depth.is_some_and(|value| depth == value + 1) => {
                        page.as_mut().expect("page exists").redirect = true;
                    }
                    b"title" if page_depth.is_some_and(|value| depth == value + 1) => {
                        capture = Some((Capture::Title, depth));
                    }
                    b"ns" if page_depth.is_some_and(|value| depth == value + 1) => {
                        capture = Some((Capture::Namespace, depth));
                    }
                    b"id" if page_depth.is_some_and(|value| depth == value + 1) => {
                        capture = Some((Capture::PageId, depth));
                    }
                    b"id" if revision_depth.is_some_and(|value| depth == value + 1) => {
                        capture = Some((Capture::RevisionId, depth));
                    }
                    b"text" if revision_depth.is_some_and(|value| depth == value + 1) => {
                        capture = Some((Capture::Text, depth));
                    }
                    _ => {}
                }
                depth = depth.saturating_add(1);
            }
            Ok(Event::Empty(element)) => {
                if element.local_name().as_ref() == b"redirect"
                    && page_depth.is_some_and(|value| depth == value + 1)
                {
                    page.as_mut().expect("page exists").redirect = true;
                }
            }
            Ok(Event::Text(text)) => {
                if let Some((field, _)) = capture {
                    let decoded = text
                        .xml_content(XmlVersion::Implicit1_0)
                        .map_err(|error| format!("decode MediaWiki XML text: {error}"))?;
                    let decoded = unescape(&decoded)
                        .map_err(|error| format!("unescape MediaWiki XML text: {error}"))?;
                    append_capture(page.as_mut().expect("page exists"), field, &decoded);
                }
            }
            Ok(Event::CData(text)) => {
                if let Some((field, _)) = capture {
                    let decoded = text
                        .xml_content(XmlVersion::Implicit1_0)
                        .map_err(|error| format!("decode MediaWiki CDATA: {error}"))?;
                    append_capture(page.as_mut().expect("page exists"), field, &decoded);
                }
            }
            Ok(Event::GeneralRef(reference)) => {
                if let Some((field, _)) = capture {
                    let reference = reference
                        .xml_content(XmlVersion::Implicit1_0)
                        .map_err(|error| format!("decode MediaWiki XML reference: {error}"))?;
                    let escaped = format!("&{reference};");
                    let decoded = unescape(&escaped)
                        .map_err(|error| format!("resolve MediaWiki XML reference: {error}"))?;
                    append_capture(page.as_mut().expect("page exists"), field, &decoded);
                }
            }
            Ok(Event::End(element)) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or("malformed MediaWiki XML depth")?;
                if capture.is_some_and(|(_, start_depth)| start_depth == depth) {
                    capture = None;
                }
                let name = element.local_name();
                if name.as_ref() == b"revision" && revision_depth == Some(depth) {
                    revision_depth = None;
                }
                if name.as_ref() == b"page" && page_depth == Some(depth) {
                    finish_page(
                        page.take().expect("page exists"),
                        output,
                        source_id,
                        &mut report,
                    )?;
                    page_depth = None;
                    revision_depth = None;
                    capture = None;
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("malformed MediaWiki XML: {error}")),
            _ => {}
        }
        buffer.clear();
    }
    if page.is_some() || depth != 0 {
        return Err("truncated MediaWiki XML".into());
    }
    Ok(report)
}

fn append_capture(page: &mut Page, field: Capture, value: &str) {
    let (target, maximum) = match field {
        Capture::Title => (&mut page.title, MAX_TITLE_BYTES),
        Capture::Namespace => (&mut page.namespace, 32),
        Capture::PageId => (&mut page.page_id, 32),
        Capture::RevisionId => (&mut page.revision_id, 32),
        Capture::Text => (&mut page.text, MAX_ARTICLE_BYTES),
    };
    if target.len().saturating_add(value.len()) > maximum {
        page.oversized = true;
        target.clear();
    } else if !page.oversized {
        target.push_str(value);
    }
}

fn finish_page(
    page: Page,
    output: &mut impl Write,
    source_id: &str,
    report: &mut ExtractionReport,
) -> Result<(), String> {
    report.pages_seen += 1;
    if page.namespace.trim() != "0" {
        report.non_article_namespace_skipped += 1;
        return Ok(());
    }
    report.namespace_zero_seen += 1;
    if page.redirect {
        report.redirects_skipped += 1;
        return Ok(());
    }
    if page.oversized {
        report.oversized_skipped += 1;
        return Ok(());
    }
    let article_id = page
        .page_id
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|id| *id != 0);
    let revision_id = page
        .revision_id
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|id| *id != 0);
    let title = page.title.trim();
    if page.revisions != 1
        || article_id.is_none()
        || revision_id.is_none()
        || title.is_empty()
        || page.text.is_empty()
    {
        report.incomplete_skipped += 1;
        return Ok(());
    }
    let record = ExtractedArticle {
        schema_version: ARTICLE_SCHEMA_VERSION,
        source_id: source_id.into(),
        article_id: article_id.expect("checked"),
        revision_id: revision_id.expect("checked"),
        title: title.into(),
        text: page.text,
    };
    serde_json::to_writer(&mut *output, &record).map_err(|error| error.to_string())?;
    output
        .write_all(b"\n")
        .map_err(|error| format!("write extracted JSONL: {error}"))?;
    report.articles_written += 1;
    Ok(())
}

struct HashingReader<R> {
    inner: R,
    hash: Sha256,
}

impl<R> HashingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            hash: Sha256::new(),
        }
    }

    fn finish(self) -> String {
        hex_digest(self.hash.finalize())
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.hash.update(&buffer[..read]);
        Ok(read)
    }
}

struct HashingWriter<W> {
    inner: W,
    hash: Sha256,
    bytes: u64,
}

impl<W> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hash: Sha256::new(),
            bytes: 0,
        }
    }

    fn finish(self) -> (String, u64) {
        (hex_digest(self.hash.finalize()), self.bytes)
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.hash.update(&buffer[..written]);
        self.bytes = self.bytes.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let mut output = String::with_capacity(64);
    for byte in digest.as_ref() {
        write!(output, "{byte:02x}").expect("String write");
    }
    output
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "sakura-context-corpus-{}-{}",
                std::process::id(),
                NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).expect("create temporary directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn streams_namespace_zero_and_reports_every_terminal_page_state() {
        let xml = r#"<?xml version="1.0"?><mediawiki>
<page><title>A &amp; B</title><ns>0</ns><id>10</id><revision><id>20</id><text>本文&amp;続き</text></revision></page>
<page><title>Talk</title><ns>1</ns><id>11</id><revision><id>21</id><text>skip</text></revision></page>
<page><title>Redirect</title><ns>0</ns><id>12</id><redirect title="A"/><revision><id>22</id><text>#REDIRECT</text></revision></page>
<page><title>Missing</title><ns>0</ns><id>13</id><revision><text/></revision></page>
</mediawiki>"#;
        let mut output = Vec::new();
        let result = extract_articles(xml.as_bytes(), &mut output, "source").expect("extract");
        assert_eq!(result.report.pages_seen, 4);
        assert_eq!(result.report.articles_written, 1);
        assert_eq!(result.report.non_article_namespace_skipped, 1);
        assert_eq!(result.report.redirects_skipped, 1);
        assert_eq!(result.report.incomplete_skipped, 1);
        let record: ExtractedArticle =
            serde_json::from_slice(output.strip_suffix(b"\n").expect("newline")).expect("JSON");
        assert_eq!(record.title, "A & B");
        assert_eq!(record.text, "本文&続き");
        assert_eq!(result.output_bytes as usize, output.len());
    }

    #[test]
    fn malformed_and_multiple_revision_pages_fail_closed() {
        assert!(extract_articles(&b"<mediawiki><page>"[..], Vec::new(), "source").is_err());
        let xml = br#"<mediawiki><page><title>A</title><ns>0</ns><id>1</id><revision><id>2</id><text>x</text></revision><revision><id>3</id><text>y</text></revision></page></mediawiki>"#;
        let mut output = Vec::new();
        let result = extract_articles(&xml[..], &mut output, "source").expect("extract");
        assert!(output.is_empty());
        assert_eq!(result.report.incomplete_skipped, 1);
    }

    #[test]
    fn extraction_manifest_is_hash_bound_and_tampering_fails() {
        let xml = br#"<mediawiki><page><title>A</title><ns>0</ns><id>1</id><revision><id>2</id><text>x</text></revision></page></mediawiki>"#;
        let mut output = Vec::new();
        let result = extract_articles(&xml[..], &mut output, "source").expect("extract");
        let directory = TestDirectory::new();
        fs::write(directory.0.join("articles.jsonl"), &output).expect("write artifact");
        let manifest = commit_extraction(
            &directory.0,
            "source",
            "20260801",
            &"11".repeat(32),
            &"22".repeat(32),
            result,
        )
        .expect("commit extraction");
        assert_eq!(manifest.artifact.records, 1);
        assert_eq!(verify_extraction(&directory.0).expect("verify"), manifest);
        fs::OpenOptions::new()
            .append(true)
            .open(directory.0.join("articles.jsonl"))
            .and_then(|mut file| file.write_all(b"{}\n"))
            .expect("tamper");
        assert!(verify_extraction(&directory.0).is_err());
    }
}
