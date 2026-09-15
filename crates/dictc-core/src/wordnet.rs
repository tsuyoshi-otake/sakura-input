//! Streaming importer for the pinned Japanese WordNet LMF archive.
//!
//! The source deliberately has no readings.  A detail is therefore emitted only
//! when its written lemma resolves to one complete, exact Sakura entry identity
//! and the lemma has one WordNet sense.  This is conservative by design.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Read};

use flate2::read::GzDecoder;
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;
use quick_xml::XmlVersion;
use sakura_core::dictionary::DetailRelationKind;

use crate::{SourceDetail, SourceDetailRelation, SourceEntry};

/// The relation slots are bounded independently of definition length.
pub const MAX_RELATIONS_PER_KIND: usize = 3;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnresolvedReport {
    pub surface_ambiguous: usize,
    pub sense_ambiguous: usize,
    pub missing_definition: usize,
    pub relation_ambiguous: usize,
    pub relation_unsupported: usize,
    pub relation_truncated: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub schema_version: u32,
    pub detail_count: usize,
    pub unresolved: UnresolvedReport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    pub details: Vec<SourceDetail>,
    pub report: ImportReport,
}

#[derive(Debug, Clone)]
struct Sense {
    lemma: String,
    synset: String,
}

#[derive(Debug, Default)]
struct Parsed {
    senses: BTreeMap<String, Sense>,
    definitions: BTreeMap<String, String>,
    antonyms: BTreeMap<String, Vec<String>>,
    unsupported_relations: usize,
}

/// Reads a gzip-compressed official `jpn_wn_lmf.xml.gz` archive.
pub fn import_lmf_gzip(reader: impl Read, entries: &[SourceEntry]) -> Result<Import, String> {
    import_lmf(BufReader::new(GzDecoder::new(reader)), entries)
}

/// Reads a UTF-8 Japanese WordNet LMF document without building an XML DOM.
pub fn import_lmf(reader: impl BufRead, entries: &[SourceEntry]) -> Result<Import, String> {
    let parsed = parse_lmf(reader)?;
    resolve(parsed, entries)
}

fn parse_lmf(input: impl BufRead) -> Result<Parsed, String> {
    let mut xml = Reader::from_reader(input);
    xml.config_mut().trim_text(false);
    let mut out = Parsed::default();
    let mut buffer = Vec::new();
    let mut lemma = None::<String>;
    let mut active_sense = None::<String>;
    let mut active_synset = None::<String>;

    loop {
        match xml.read_event_into(&mut buffer) {
            Ok(Event::Start(element)) | Ok(Event::Empty(element)) => {
                match element.name().as_ref() {
                    b"LexicalEntry" => lemma = None,
                    b"Lemma" => lemma = attribute(&element, b"writtenForm")?,
                    b"Sense" => {
                        let id = required_attribute(&element, b"id")?;
                        let synset = required_attribute(&element, b"synset")?;
                        let lemma = lemma.clone().ok_or("Sense before Lemma in WordNet LMF")?;
                        if out
                            .senses
                            .insert(id.clone(), Sense { lemma, synset })
                            .is_some()
                        {
                            return Err(format!("duplicate WordNet sense id '{id}'"));
                        }
                        // LMF allows an empty `Sense` element. Keep its id until
                        // the enclosing lexical entry ends, so a later explicit
                        // SenseRelation (if a future official export includes one)
                        // is still tied to this sense.
                        active_sense = Some(id);
                    }
                    b"Synset" => {
                        if !element.is_empty() {
                            active_synset = Some(required_attribute(&element, b"id")?);
                        }
                    }
                    b"Definition" if active_synset.is_some() => {
                        // Wn-Ja 1.1 stores the whole Japanese definition in
                        // `gloss`; nested Statement elements are examples.
                        let synset = active_synset.clone().expect("checked above");
                        let gloss = required_attribute(&element, b"gloss")?;
                        if out.definitions.insert(synset.clone(), gloss).is_some() {
                            return Err(format!(
                                "duplicate Japanese definition for synset '{synset}'"
                            ));
                        }
                    }
                    b"SenseRelation" => {
                        let relation = attribute(&element, b"relType")?;
                        if relation.as_deref() == Some("ant") {
                            let source =
                                active_sense.clone().ok_or("SenseRelation outside Sense")?;
                            let targets = required_attribute(&element, b"targets")?;
                            out.antonyms
                                .entry(source)
                                .or_default()
                                .extend(targets.split_ascii_whitespace().map(str::to_owned));
                        } else {
                            out.unsupported_relations += 1;
                        }
                    }
                    b"SynsetRelation" => out.unsupported_relations += 1,
                    _ => {}
                }
            }
            Ok(Event::End(element)) => match element.name().as_ref() {
                b"LexicalEntry" => active_sense = None,
                b"Synset" => active_synset = None,
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("malformed WordNet LMF: {error}")),
            _ => {}
        }
        buffer.clear();
    }
    if active_synset.is_some() || active_sense.is_some() {
        return Err("truncated WordNet LMF".to_owned());
    }
    Ok(out)
}

fn attribute(element: &BytesStart<'_>, key: &[u8]) -> Result<Option<String>, String> {
    let mut found = None;
    for attribute in element.attributes().with_checks(true) {
        let attribute =
            attribute.map_err(|error| format!("invalid WordNet LMF attribute: {error}"))?;
        if attribute.key.as_ref() == key {
            if found.is_some() {
                return Err(format!(
                    "duplicate WordNet LMF attribute '{}'",
                    String::from_utf8_lossy(key)
                ));
            }
            found = Some(
                attribute
                    .decoded_and_normalized_value(XmlVersion::Implicit1_0, element.decoder())
                    .map_err(|error| error.to_string())?
                    .into_owned(),
            );
        }
    }
    Ok(found)
}

fn required_attribute(element: &BytesStart<'_>, key: &[u8]) -> Result<String, String> {
    attribute(element, key)?.ok_or_else(|| {
        format!(
            "WordNet LMF element '{}' lacks required attribute '{}'",
            String::from_utf8_lossy(element.name().as_ref()),
            String::from_utf8_lossy(key)
        )
    })
}

fn resolve(parsed: Parsed, entries: &[SourceEntry]) -> Result<Import, String> {
    let mut dictionary = BTreeMap::<&str, Vec<&SourceEntry>>::new();
    for entry in entries {
        dictionary.entry(&entry.surface).or_default().push(entry);
    }
    let mut by_lemma = BTreeMap::<&str, Vec<(&str, &Sense)>>::new();
    let mut by_synset = BTreeMap::<&str, BTreeSet<&str>>::new();
    for (sense_id, sense) in &parsed.senses {
        by_lemma
            .entry(&sense.lemma)
            .or_default()
            .push((sense_id, sense));
        by_synset
            .entry(&sense.synset)
            .or_default()
            .insert(&sense.lemma);
    }

    let mut unresolved = UnresolvedReport {
        relation_unsupported: parsed.unsupported_relations,
        ..UnresolvedReport::default()
    };
    let mut details = Vec::new();
    for (lemma, senses) in by_lemma {
        let Some(candidates) = dictionary.get(lemma) else {
            continue;
        };
        if candidates.len() != 1 {
            unresolved.surface_ambiguous += 1;
            continue;
        }
        if senses.len() != 1 {
            unresolved.sense_ambiguous += 1;
            continue;
        }
        let (sense_id, sense) = senses[0];
        let Some(description) = parsed.definitions.get(&sense.synset) else {
            unresolved.missing_definition += 1;
            continue;
        };
        let mut relations = Vec::new();
        let similar = by_synset
            .get(sense.synset.as_str())
            .expect("inserted with sense");
        append_relations(
            &mut relations,
            DetailRelationKind::Synonym,
            similar.iter().copied().filter(|target| *target != lemma),
            &mut unresolved,
        );
        if let Some(antonyms) = parsed.antonyms.get(sense_id) {
            let targets = antonyms.iter().filter_map(|target| {
                parsed
                    .senses
                    .get(target)
                    .map(|target_sense| target_sense.lemma.as_str())
            });
            append_relations(
                &mut relations,
                DetailRelationKind::Antonym,
                targets,
                &mut unresolved,
            );
        }
        let entry = candidates[0];
        details.push(SourceDetail {
            reading: entry.reading.clone(),
            surface: entry.surface.clone(),
            left_id: entry.left_id,
            right_id: entry.right_id,
            description: description.clone(),
            relations,
        });
    }
    details.sort_by(|left, right| {
        (&left.reading, &left.surface, left.left_id, left.right_id).cmp(&(
            &right.reading,
            &right.surface,
            right.left_id,
            right.right_id,
        ))
    });
    Ok(Import {
        report: ImportReport {
            schema_version: 2,
            detail_count: details.len(),
            unresolved,
        },
        details,
    })
}

fn append_relations<'a>(
    output: &mut Vec<SourceDetailRelation>,
    kind: DetailRelationKind,
    values: impl Iterator<Item = &'a str>,
    unresolved: &mut UnresolvedReport,
) {
    let values = values.collect::<BTreeSet<_>>();
    if values.len() > MAX_RELATIONS_PER_KIND {
        unresolved.relation_truncated += values.len() - MAX_RELATIONS_PER_KIND;
    }
    output.extend(
        values
            .into_iter()
            .take(MAX_RELATIONS_PER_KIND)
            .map(|target| SourceDetailRelation {
                kind,
                target: target.to_owned(),
            }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_entries;

    fn entries(text: &str) -> Vec<SourceEntry> {
        parse_entries("test", text).unwrap()
    }

    #[test]
    fn imports_only_unique_surface_and_sense_with_same_synset_similar() {
        let xml = r#"<LexicalResource><Lexicon>
          <LexicalEntry><Lemma writtenForm="&#x732B;"/><Sense id="cat" synset="s1"/></LexicalEntry>
          <LexicalEntry><Lemma writtenForm="&#x306D;&#x3053;"/><Sense id="cat-kana" synset="s1"/></LexicalEntry>
          <LexicalEntry><Lemma writtenForm="&#x66D6;&#x6627;"/><Sense id="ambiguous-1" synset="s2"/><Sense id="ambiguous-2" synset="s3"/></LexicalEntry>
          <Synset id="s1"><Definition gloss="definition"/><SynsetRelations><SynsetRelation relType="sim" targets="s2"/></SynsetRelations></Synset>
          <Synset id="s2"><Definition gloss="two"/></Synset><Synset id="s3"><Definition gloss="three"/></Synset>
        </Lexicon></LexicalResource>"#;
        let imported = import_lmf(
            BufReader::new(xml.as_bytes()),
            &entries("# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\u{306d}\u{3053}\t\u{732b}\t1\t1\t1\t1\t\t\n\u{3042}\u{3044}\u{307e}\u{3044}\t\u{66d6}\u{6627}\t1\t1\t1\t1\t\t\n"),
        )
        .unwrap();
        assert_eq!(imported.details.len(), 1);
        assert_eq!(imported.details[0].description, "definition");
        assert_eq!(
            imported.details[0].relations[0].kind,
            DetailRelationKind::Synonym
        );
        assert_eq!(imported.details[0].relations[0].target, "\u{306d}\u{3053}");
        assert_eq!(imported.report.unresolved.sense_ambiguous, 1);
        assert_eq!(imported.report.unresolved.relation_unsupported, 1);
    }

    #[test]
    fn ambiguous_surface_and_malformed_xml_never_produce_detail() {
        let xml = r#"<LexicalResource><Lexicon><LexicalEntry><Lemma writtenForm="&#x6A4B;"/><Sense id="bridge" synset="s"/></LexicalEntry><Synset id="s"><Definition gloss="bridge"/></Synset></Lexicon></LexicalResource>"#;
        let imported = import_lmf(
            BufReader::new(xml.as_bytes()),
            &entries("# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\u{306f}\u{3057}\t\u{6a4b}\t1\t1\t1\t1\t\t\n\u{304d}\u{3087}\u{3046}\t\u{6a4b}\t2\t2\t1\t1\t\t\n"),
        )
        .unwrap();
        assert!(imported.details.is_empty());
        assert_eq!(imported.report.unresolved.surface_ambiguous, 1);
        assert!(import_lmf(BufReader::new(&b"<x><Synset>"[..]), &[]).is_err());
    }

    #[test]
    #[ignore = "requires WORDNET_LMF pointing at the separately downloaded pinned official archive"]
    fn pinned_official_archive_is_streamed_without_a_dom() {
        let path = std::env::var_os("WORDNET_LMF").expect("WORDNET_LMF is required");
        let parsed = parse_lmf(BufReader::new(GzDecoder::new(
            std::fs::File::open(&path).unwrap(),
        )))
        .unwrap();
        assert_eq!(parsed.definitions.len(), 109_695);
        let entries = entries("# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\u{306d}\u{3053}\t\u{732b}\t1\t1\t1\t1\t\t\n");
        let imported = import_lmf_gzip(std::fs::File::open(path).unwrap(), &entries).unwrap();
        assert_eq!(imported.details.len(), 1);
        assert_eq!(imported.report.unresolved.surface_ambiguous, 0);
        assert_eq!(imported.report.unresolved.sense_ambiguous, 0);
        assert_eq!(imported.report.unresolved.missing_definition, 0);
        assert_eq!(imported.report.unresolved.relation_unsupported, 286_250);
    }
}
