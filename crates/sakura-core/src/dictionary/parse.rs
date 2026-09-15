//! Header, directory, and table-shape validation that turns an untrusted image
//! into borrowed [`Dictionary`] table views.
//!
//! Record-level cross-table checks run afterwards in `Dictionary::validate_tables`.

use super::{
    image_format, read_u16, read_u32, to_usize, validate_boundary_table, validate_matrix_table,
    validate_single_kanji_table, Details, Dictionary, Error, ImageVersion, SingleKanji, Table,
};

impl<'a> Dictionary<'a> {
    /// Validates an image and returns borrowed table views.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        use image_format as format;

        if bytes.len() < format::HEADER_LEN {
            return Err(Error::Truncated);
        }
        if bytes.get(..8) != Some(format::MAGIC.as_slice()) {
            return Err(Error::BadMagic);
        }
        let raw_version = read_u16(bytes, 8).ok_or(Error::Truncated)?;
        let version = match raw_version {
            format::VERSION => ImageVersion::V1,
            format::VERSION_V2 => ImageVersion::V2,
            _ => return Err(Error::UnsupportedVersion(raw_version)),
        };
        let header_len = usize::from(read_u16(bytes, 10).ok_or(Error::Truncated)?);
        let table_count = usize::from(read_u16(bytes, 12).ok_or(Error::Truncated)?);
        let class_count = usize::from(read_u16(bytes, 14).ok_or(Error::Truncated)?);
        let entry_count = to_usize(read_u32(bytes, 16).ok_or(Error::Truncated)?)?;
        let node_count = to_usize(read_u32(bytes, 20).ok_or(Error::Truncated)?)?;
        let image_len = to_usize(read_u32(bytes, 24).ok_or(Error::Truncated)?)?;
        let reserved = read_u32(bytes, 28).ok_or(Error::Truncated)?;

        if header_len != format::HEADER_LEN
            || table_count == 0
            || table_count > format::MAX_TABLES
            || class_count == 0
            || node_count == 0
            || image_len != bytes.len()
            || reserved != 0
        {
            return Err(Error::BadHeader);
        }
        let directory_len = table_count
            .checked_mul(format::DIRECTORY_ENTRY_LEN)
            .ok_or(Error::BadDirectory)?;
        let directory_end = format::HEADER_LEN
            .checked_add(directory_len)
            .ok_or(Error::BadDirectory)?;
        if directory_end > bytes.len() {
            return Err(Error::Truncated);
        }

        validate_directory(bytes, table_count, directory_end)?;

        let louds_table = required_table(bytes, table_count, format::TAG_LOUDS)?;
        let nodes = required_table(bytes, table_count, format::TAG_NODES)?;
        let labels = match version {
            ImageVersion::V1 => required_table(bytes, table_count, format::TAG_LABELS)?,
            ImageVersion::V2 => {
                if optional_table(bytes, table_count, format::TAG_LABELS)?.is_some() {
                    return Err(Error::BadTable(format::TAG_LABELS));
                }
                Table {
                    tag: format::TAG_LABELS,
                    bytes: &[],
                    count: 0,
                }
            }
        };
        let entries = required_table(bytes, table_count, format::TAG_ENTRIES)?;
        let surface_offsets = required_table(bytes, table_count, format::TAG_SURFACE_OFFSETS)?;
        let surfaces = required_table(bytes, table_count, format::TAG_SURFACES)?;
        let annotation_offsets =
            required_table(bytes, table_count, format::TAG_ANNOTATION_OFFSETS)?;
        let annotations = required_table(bytes, table_count, format::TAG_ANNOTATIONS)?;
        let annotation_index = match version {
            ImageVersion::V1 => {
                if optional_table(bytes, table_count, format::TAG_ANNOTATION_INDEX)?.is_some() {
                    return Err(Error::BadTable(format::TAG_ANNOTATION_INDEX));
                }
                None
            }
            ImageVersion::V2 => Some(required_table(
                bytes,
                table_count,
                format::TAG_ANNOTATION_INDEX,
            )?),
        };
        let matrix = required_table(bytes, table_count, format::TAG_MATRIX)?;

        let boundaries = match optional_table(bytes, table_count, format::TAG_BOUNDARIES)? {
            Some(table) => Some(validate_boundary_table(table, class_count)?),
            None => None,
        };

        let detail_index = optional_table(bytes, table_count, format::TAG_DETAIL_INDEX)?;
        let detail_records = optional_table(bytes, table_count, format::TAG_DETAIL_RECORDS)?;
        let detail_relations = optional_table(bytes, table_count, format::TAG_DETAIL_RELATIONS)?;
        let detail_text_offsets =
            optional_table(bytes, table_count, format::TAG_DETAIL_TEXT_OFFSETS)?;
        let detail_text = optional_table(bytes, table_count, format::TAG_DETAIL_TEXT)?;
        let detail_tables_present = [
            detail_index.is_some(),
            detail_records.is_some(),
            detail_relations.is_some(),
            detail_text_offsets.is_some(),
            detail_text.is_some(),
        ];
        if detail_tables_present.iter().any(|present| *present)
            && detail_tables_present.iter().any(|present| !*present)
        {
            return Err(Error::BadTable(format::TAG_DETAIL_INDEX));
        }

        let single_kanji_index =
            optional_table(bytes, table_count, format::TAG_SINGLE_KANJI_INDEX)?;
        let single_kanji_readings =
            optional_table(bytes, table_count, format::TAG_SINGLE_KANJI_READINGS)?;
        let single_kanji_chars =
            optional_table(bytes, table_count, format::TAG_SINGLE_KANJI_CHARS)?;
        let single_kanji_variants =
            optional_table(bytes, table_count, format::TAG_SINGLE_KANJI_VARIANTS)?;
        let single_kanji_present = [
            single_kanji_index.is_some(),
            single_kanji_readings.is_some(),
            single_kanji_chars.is_some(),
            single_kanji_variants.is_some(),
        ];
        if single_kanji_present.iter().any(|present| *present)
            && single_kanji_present.iter().any(|present| !*present)
        {
            return Err(Error::BadTable(format::TAG_SINGLE_KANJI_INDEX));
        }

        expect_fixed_count(nodes, node_count, version.node_len())?;
        if version == ImageVersion::V1 {
            expect_fixed_count(labels, node_count, 4)?;
        }
        expect_fixed_count(entries, entry_count, version.entry_len())?;
        let surface_count = match version {
            ImageVersion::V1 => {
                expect_fixed_count(surface_offsets, surface_offsets.count, 4)?;
                surface_offsets.count
            }
            ImageVersion::V2 => {
                let restart_count = surfaces
                    .count
                    .checked_add(format::SURFACE_RESTART_INTERVAL - 1)
                    .ok_or(Error::BadTable(format::TAG_SURFACE_OFFSETS))?
                    / format::SURFACE_RESTART_INTERVAL;
                expect_fixed_count(surface_offsets, restart_count, 4)?;
                surfaces.count
            }
        };
        expect_fixed_count(annotation_offsets, annotation_offsets.count, 4)?;
        if version == ImageVersion::V2 && annotations.count != annotation_offsets.count {
            return Err(Error::BadTable(format::TAG_ANNOTATIONS));
        }
        if let Some(index) = annotation_index {
            expect_fixed_count(index, index.count, format::ANNOTATION_INDEX_LEN_V2)?;
        }
        validate_matrix_table(matrix, class_count)?;

        if louds_table.bytes.len() < 4 {
            return Err(Error::BadTable(format::TAG_LOUDS));
        }

        let details = if detail_tables_present[0] {
            let index = detail_index.ok_or(Error::BadTable(format::TAG_DETAIL_INDEX))?;
            let records = detail_records.ok_or(Error::BadTable(format::TAG_DETAIL_RECORDS))?;
            let relations =
                detail_relations.ok_or(Error::BadTable(format::TAG_DETAIL_RELATIONS))?;
            let text_offsets =
                detail_text_offsets.ok_or(Error::BadTable(format::TAG_DETAIL_TEXT_OFFSETS))?;
            let text = detail_text.ok_or(Error::BadTable(format::TAG_DETAIL_TEXT))?;
            expect_fixed_count(index, index.count, format::DETAIL_INDEX_LEN)?;
            expect_fixed_count(records, records.count, format::DETAIL_RECORD_LEN)?;
            expect_fixed_count(relations, relations.count, format::DETAIL_RELATION_LEN)?;
            expect_fixed_count(text_offsets, text_offsets.count, 4)?;
            Some(Details {
                index: index.bytes,
                index_count: index.count,
                records: records.bytes,
                record_count: records.count,
                relations: relations.bytes,
                relation_count: relations.count,
                text_offsets: text_offsets.bytes,
                text_count: text_offsets.count,
                text: text.bytes,
            })
        } else {
            None
        };
        let single_kanji = if single_kanji_present[0] {
            let index =
                single_kanji_index.ok_or(Error::BadTable(format::TAG_SINGLE_KANJI_INDEX))?;
            let readings =
                single_kanji_readings.ok_or(Error::BadTable(format::TAG_SINGLE_KANJI_READINGS))?;
            let chars =
                single_kanji_chars.ok_or(Error::BadTable(format::TAG_SINGLE_KANJI_CHARS))?;
            let variants =
                single_kanji_variants.ok_or(Error::BadTable(format::TAG_SINGLE_KANJI_VARIANTS))?;
            expect_fixed_count(index, index.count, format::SINGLE_KANJI_INDEX_LEN)?;
            expect_fixed_count(chars, chars.count, format::SINGLE_KANJI_CHAR_LEN)?;
            expect_fixed_count(variants, variants.count, format::SINGLE_KANJI_VARIANT_LEN)?;
            if readings.count != readings.bytes.len() {
                return Err(Error::BadTable(format::TAG_SINGLE_KANJI_READINGS));
            }
            let table = SingleKanji {
                index: index.bytes,
                index_count: index.count,
                readings: readings.bytes,
                chars: chars.bytes,
                char_count: chars.count,
                variants: variants.bytes,
                variant_count: variants.count,
            };
            validate_single_kanji_table(&table)?;
            Some(table)
        } else {
            None
        };
        let louds_bits =
            to_usize(read_u32(louds_table.bytes, 0).ok_or(Error::BadTable(format::TAG_LOUDS))?)?;
        let louds_bytes = louds_bits
            .checked_add(7)
            .ok_or(Error::BadTable(format::TAG_LOUDS))?
            / 8;
        if louds_table.count != louds_bits || louds_table.bytes.len() != 4 + louds_bytes {
            return Err(Error::BadTable(format::TAG_LOUDS));
        }

        let dictionary = Dictionary {
            version,
            class_count,
            entry_count,
            node_count,
            louds: &louds_table.bytes[4..],
            louds_bits,
            nodes: nodes.bytes,
            labels: labels.bytes,
            entries: entries.bytes,
            surface_offsets: surface_offsets.bytes,
            surface_count,
            surfaces: surfaces.bytes,
            annotation_offsets: annotation_offsets.bytes,
            annotation_count: annotation_offsets.count,
            annotations: annotations.bytes,
            annotation_index: annotation_index.map(|table| table.bytes),
            annotation_index_count: annotation_index.map_or(0, |table| table.count),
            matrix: matrix.bytes,
            boundaries,
            details,
            single_kanji,
        };
        dictionary.validate_tables()?;
        Ok(dictionary)
    }
}

fn validate_directory(bytes: &[u8], table_count: usize, directory_end: usize) -> Result<(), Error> {
    for index in 0..table_count {
        let table = directory_table(bytes, index)?;
        let start = table.bytes.as_ptr() as usize - bytes.as_ptr() as usize;
        if start < directory_end || !start.is_multiple_of(8) {
            return Err(Error::BadDirectory);
        }
        for other_index in index + 1..table_count {
            let other = directory_table(bytes, other_index)?;
            if table.tag == other.tag {
                return Err(Error::DuplicateTable(table.tag));
            }
            let other_start = other.bytes.as_ptr() as usize - bytes.as_ptr() as usize;
            let table_end = start + table.bytes.len();
            let other_end = other_start + other.bytes.len();
            if start < other_end && other_start < table_end {
                return Err(Error::BadDirectory);
            }
        }
    }
    Ok(())
}

fn required_table<'a>(bytes: &'a [u8], count: usize, tag: [u8; 4]) -> Result<Table<'a>, Error> {
    for index in 0..count {
        let table = directory_table(bytes, index)?;
        if table.tag == tag {
            return Ok(table);
        }
    }
    Err(Error::MissingTable(tag))
}

fn optional_table<'a>(
    bytes: &'a [u8],
    count: usize,
    tag: [u8; 4],
) -> Result<Option<Table<'a>>, Error> {
    for index in 0..count {
        let table = directory_table(bytes, index)?;
        if table.tag == tag {
            return Ok(Some(table));
        }
    }
    Ok(None)
}

fn directory_table(bytes: &[u8], index: usize) -> Result<Table<'_>, Error> {
    let at = image_format::HEADER_LEN
        .checked_add(
            index
                .checked_mul(image_format::DIRECTORY_ENTRY_LEN)
                .ok_or(Error::BadDirectory)?,
        )
        .ok_or(Error::BadDirectory)?;
    let record = bytes
        .get(at..at + image_format::DIRECTORY_ENTRY_LEN)
        .ok_or(Error::Truncated)?;
    let tag: [u8; 4] = record[0..4].try_into().map_err(|_| Error::BadDirectory)?;
    let offset = to_usize(read_u32(record, 4).ok_or(Error::BadDirectory)?)?;
    let len = to_usize(read_u32(record, 8).ok_or(Error::BadDirectory)?)?;
    let count = to_usize(read_u32(record, 12).ok_or(Error::BadDirectory)?)?;
    let end = offset.checked_add(len).ok_or(Error::BadDirectory)?;
    let table_bytes = bytes.get(offset..end).ok_or(Error::BadDirectory)?;
    Ok(Table {
        tag,
        bytes: table_bytes,
        count,
    })
}

fn expect_fixed_count(table: Table<'_>, expected_count: usize, stride: usize) -> Result<(), Error> {
    if table.count != expected_count
        || expected_count
            .checked_mul(stride)
            .is_none_or(|len| len != table.bytes.len())
    {
        return Err(Error::BadTable(table.tag));
    }
    Ok(())
}
