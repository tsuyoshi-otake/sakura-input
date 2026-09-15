//! Record-level and cross-table validation of a parsed dictionary image.
//!
//! `parse` checks the header, directory, and fixed table shapes first; the
//! functions here reject malformed offsets, ordinals, UTF-8, and optional
//! tables before any lookup can read them.

use super::{
    align_up_4, bit_at, image_format, read_offset, read_u16, read_u32, to_usize,
    DetailRelationKind, Details, Dictionary, Error, FixedStr, ImageVersion, SingleKanji,
    SingleKanjiVariantKind, Table, MAX_PREEDIT_BYTES,
};

impl<'a> Dictionary<'a> {
    pub(super) fn validate_tables(&self) -> Result<(), Error> {
        use image_format as format;

        let expected_bits = self
            .node_count
            .checked_mul(2)
            .and_then(|n| n.checked_sub(1))
            .ok_or(Error::BadTree)?;
        if self.louds_bits != expected_bits {
            return Err(Error::BadTree);
        }
        if self.label(0)? != '\0' {
            return Err(Error::BadTree);
        }

        let mut bit = 0usize;
        let mut edge_count = 0usize;
        for node_index in 0..self.node_count {
            // Validate every stored scalar, including nodes a malformed child
            // range might otherwise leave unobserved.
            self.label(node_index)?;
            let node = self.node(node_index)?;
            if node.value_start > self.entry_count
                || node.value_count > self.entry_count - node.value_start
            {
                return Err(Error::BadTree);
            }
            if node.child_count == 0 {
                if node.first_child != 0 {
                    return Err(Error::BadTree);
                }
            } else {
                if node.first_child == 0
                    || node.first_child > self.node_count
                    || node.child_count > self.node_count - node.first_child
                {
                    return Err(Error::BadTree);
                }
                let mut previous = None;
                for child in node.first_child..node.first_child + node.child_count {
                    let label = self.label(child)?;
                    if previous.is_some_and(|before| before >= label) {
                        return Err(Error::BadTree);
                    }
                    previous = Some(label);
                }
            }
            for _ in 0..node.child_count {
                if !self.louds_bit(bit)? {
                    return Err(Error::BadTree);
                }
                bit += 1;
                edge_count += 1;
            }
            if self.louds_bit(bit)? {
                return Err(Error::BadTree);
            }
            bit += 1;
        }
        if bit != self.louds_bits || edge_count + 1 != self.node_count {
            return Err(Error::BadTree);
        }
        for padding in self.louds_bits..self.louds.len() * 8 {
            if bit_at(self.louds, padding).unwrap_or(true) {
                return Err(Error::BadTree);
            }
        }

        match self.version {
            ImageVersion::V1 => self.validate_offsets(
                self.surface_offsets,
                self.surface_count,
                self.surfaces,
                format::TAG_SURFACES,
            )?,
            ImageVersion::V2 => self.validate_v2_surfaces()?,
        }

        if let Some(details) = self.details {
            self.validate_details(details)?;
        }
        self.validate_offsets(
            self.annotation_offsets,
            self.annotation_count,
            self.annotations,
            format::TAG_ANNOTATIONS,
        )?;
        if self.version == ImageVersion::V2 {
            self.validate_v2_annotations()?;
            self.validate_v2_annotation_index()?;
        }

        for index in 0..self.entry_count {
            let entry = self.entry(index)?;
            if to_usize(entry.surface_id)? >= self.surface_count
                || usize::from(entry.left_id) >= self.class_count
                || usize::from(entry.right_id) >= self.class_count
                || (self.version == ImageVersion::V1
                    && entry.annotation_locator != format::NO_ANNOTATION
                    && to_usize(entry.annotation_locator)? >= self.annotation_count)
            {
                return Err(Error::BadEntry);
            }
        }
        Ok(())
    }

    fn validate_v2_surfaces(&self) -> Result<(), Error> {
        use image_format as format;

        let restart_count = self
            .surface_count
            .checked_add(format::SURFACE_RESTART_INTERVAL - 1)
            .ok_or(Error::BadTable(format::TAG_SURFACE_OFFSETS))?
            / format::SURFACE_RESTART_INTERVAL;
        if restart_count == 0 {
            if !self.surface_offsets.is_empty() || !self.surfaces.is_empty() {
                return Err(Error::BadTable(format::TAG_SURFACES));
            }
            return Ok(());
        }

        let mut previous_offset = None;
        let mut decoded = 0usize;
        for restart_index in 0..restart_count {
            let start = read_offset(self.surface_offsets, restart_index)
                .ok_or(Error::BadTable(format::TAG_SURFACE_OFFSETS))?;
            let end = if restart_index + 1 < restart_count {
                read_offset(self.surface_offsets, restart_index + 1)
                    .ok_or(Error::BadTable(format::TAG_SURFACE_OFFSETS))?
            } else {
                self.surfaces.len()
            };
            if (restart_index == 0 && start != 0)
                || start >= self.surfaces.len()
                || end > self.surfaces.len()
                || start >= end
                || previous_offset.is_some_and(|previous| previous >= start)
            {
                return Err(Error::BadTable(format::TAG_SURFACE_OFFSETS));
            }
            previous_offset = Some(start);

            let remaining = self.surface_count - decoded;
            let records = remaining.min(format::SURFACE_RESTART_INTERVAL);
            let mut cursor = start;
            let mut value = FixedStr::<MAX_PREEDIT_BYTES>::new();
            for record_index in 0..records {
                let prefix = usize::from(
                    read_u16(self.surfaces, cursor).ok_or(Error::BadTable(format::TAG_SURFACES))?,
                );
                let suffix_at = cursor
                    .checked_add(2)
                    .ok_or(Error::BadTable(format::TAG_SURFACES))?;
                let suffix_len = usize::from(
                    read_u16(self.surfaces, suffix_at)
                        .ok_or(Error::BadTable(format::TAG_SURFACES))?,
                );
                let suffix_start = cursor
                    .checked_add(4)
                    .ok_or(Error::BadTable(format::TAG_SURFACES))?;
                let record_end = suffix_start
                    .checked_add(suffix_len)
                    .ok_or(Error::BadTable(format::TAG_SURFACES))?;
                if record_end > end
                    || prefix > value.len()
                    || !value.as_str().is_char_boundary(prefix)
                    || (record_index == 0 && prefix != 0)
                {
                    return Err(Error::BadTable(format::TAG_SURFACES));
                }
                let suffix = core::str::from_utf8(
                    self.surfaces
                        .get(suffix_start..record_end)
                        .ok_or(Error::BadTable(format::TAG_SURFACES))?,
                )
                .map_err(|_| Error::BadUtf8)?;
                let keep_chars = value.as_str()[..prefix].chars().count();
                let remove_chars = value.as_str().chars().count() - keep_chars;
                value.truncate_chars(remove_chars);
                value.push_str(suffix).map_err(|_| Error::TextOverflow)?;
                cursor = record_end;
                decoded += 1;
            }
            if cursor != end {
                return Err(Error::BadTable(format::TAG_SURFACES));
            }
        }
        if decoded != self.surface_count {
            return Err(Error::BadTable(format::TAG_SURFACES));
        }
        Ok(())
    }

    fn validate_v2_annotations(&self) -> Result<(), Error> {
        use image_format as format;

        if self.annotation_count == 0 {
            if !self.annotation_offsets.is_empty() || !self.annotations.is_empty() {
                return Err(Error::BadTable(format::TAG_ANNOTATIONS));
            }
            return Ok(());
        }
        if read_offset(self.annotation_offsets, 0) != Some(0) {
            return Err(Error::BadTable(format::TAG_ANNOTATION_OFFSETS));
        }
        for index in 0..self.annotation_count {
            let record = self.text_record(
                self.annotation_offsets,
                self.annotation_count,
                self.annotations,
                index,
            )?;
            core::str::from_utf8(record).map_err(|_| Error::BadUtf8)?;
        }
        Ok(())
    }

    fn validate_v2_annotation_index(&self) -> Result<(), Error> {
        use image_format as format;

        let index = self
            .annotation_index
            .ok_or(Error::MissingTable(format::TAG_ANNOTATION_INDEX))?;
        let mut previous_entry = None;
        for record_index in 0..self.annotation_index_count {
            let at = record_index
                .checked_mul(format::ANNOTATION_INDEX_LEN_V2)
                .ok_or(Error::BadTable(format::TAG_ANNOTATION_INDEX))?;
            let entry = to_usize(
                read_u32(index, at).ok_or(Error::BadTable(format::TAG_ANNOTATION_INDEX))?,
            )?;
            let annotation = to_usize(
                read_u32(index, at + 4).ok_or(Error::BadTable(format::TAG_ANNOTATION_INDEX))?,
            )?;
            if entry >= self.entry_count
                || annotation >= self.annotation_count
                || previous_entry.is_some_and(|previous| previous >= entry)
            {
                return Err(Error::BadTable(format::TAG_ANNOTATION_INDEX));
            }
            previous_entry = Some(entry);
        }
        Ok(())
    }

    fn validate_details(&self, details: Details<'a>) -> Result<(), Error> {
        use image_format as format;

        self.validate_offsets(
            details.text_offsets,
            details.text_count,
            details.text,
            format::TAG_DETAIL_TEXT,
        )?;
        let mut previous_entry = None;
        for index in 0..details.index_count {
            let at = index * format::DETAIL_INDEX_LEN;
            let entry = to_usize(
                read_u32(details.index, at).ok_or(Error::BadTable(format::TAG_DETAIL_INDEX))?,
            )?;
            let record = to_usize(
                read_u32(details.index, at + 4).ok_or(Error::BadTable(format::TAG_DETAIL_INDEX))?,
            )?;
            if entry >= self.entry_count
                || record >= details.record_count
                || previous_entry.is_some_and(|previous| previous >= entry)
            {
                return Err(Error::BadTable(format::TAG_DETAIL_INDEX));
            }
            previous_entry = Some(entry);
        }
        for index in 0..details.record_count {
            let at = index * format::DETAIL_RECORD_LEN;
            let description = to_usize(
                read_u32(details.records, at).ok_or(Error::BadTable(format::TAG_DETAIL_RECORDS))?,
            )?;
            let display = to_usize(
                read_u32(details.records, at + 4)
                    .ok_or(Error::BadTable(format::TAG_DETAIL_RECORDS))?,
            )?;
            let relation_start = to_usize(
                read_u32(details.records, at + 8)
                    .ok_or(Error::BadTable(format::TAG_DETAIL_RECORDS))?,
            )?;
            let relation_count = to_usize(
                read_u32(details.records, at + 12)
                    .ok_or(Error::BadTable(format::TAG_DETAIL_RECORDS))?,
            )?;
            if description >= details.text_count
                || display >= details.text_count
                || relation_start
                    .checked_add(relation_count)
                    .is_none_or(|end| end > details.relation_count)
            {
                return Err(Error::BadTable(format::TAG_DETAIL_RECORDS));
            }
            self.detail_text(details, description)?;
            self.detail_text(details, display)?;
        }
        for index in 0..details.relation_count {
            let at = index * format::DETAIL_RELATION_LEN;
            if details.relations.get(at + 1..at + 4) != Some(&[0, 0, 0])
                || DetailRelationKind::from_byte(
                    *details
                        .relations
                        .get(at)
                        .ok_or(Error::BadTable(format::TAG_DETAIL_RELATIONS))?,
                )
                .is_none()
            {
                return Err(Error::BadTable(format::TAG_DETAIL_RELATIONS));
            }
            let text = to_usize(
                read_u32(details.relations, at + 4)
                    .ok_or(Error::BadTable(format::TAG_DETAIL_RELATIONS))?,
            )?;
            if text >= details.text_count {
                return Err(Error::BadTable(format::TAG_DETAIL_RELATIONS));
            }
            self.detail_text(details, text)?;
        }
        Ok(())
    }

    fn detail_text(&self, details: Details<'a>, index: usize) -> Result<&'a str, Error> {
        let record = self.text_record(
            details.text_offsets,
            details.text_count,
            details.text,
            index,
        )?;
        core::str::from_utf8(record).map_err(|_| Error::BadUtf8)
    }

    fn validate_offsets(
        &self,
        offsets: &[u8],
        count: usize,
        data: &[u8],
        tag: [u8; 4],
    ) -> Result<(), Error> {
        if count == 0 {
            return Ok(());
        }
        let mut previous = None;
        for index in 0..count {
            let offset = read_offset(offsets, index).ok_or(Error::BadTable(tag))?;
            if offset >= data.len() || previous.is_some_and(|before| before >= offset) {
                return Err(Error::BadTable(tag));
            }
            previous = Some(offset);
        }
        Ok(())
    }
}

/// Validates the optional bunsetsu-boundary table and returns its row bytes.
///
/// The invariants enforced here let conversion index the bitmap without any
/// further bounds thinking: exact length, zeroed row-tail padding bits, and an
/// unconditional boundary on both sides of BOS/EOS (class 0), matching the
/// Mozc segmenter this table is generated from.
pub(super) fn validate_boundary_table<'a>(
    table: Table<'a>,
    class_count: usize,
) -> Result<&'a [u8], Error> {
    use image_format as format;

    let bad = || Error::BadTable(format::TAG_BOUNDARIES);
    let row_bytes = class_count.checked_add(7).ok_or_else(bad)? / 8;
    let rows_len = class_count.checked_mul(row_bytes).ok_or_else(bad)?;
    let expected_len = format::BOUNDARY_HEADER_LEN
        .checked_add(rows_len)
        .ok_or_else(bad)?;
    if table.count != class_count
        || table.bytes.len() != expected_len
        || table.bytes.get(..4) != Some(format::BOUNDARY_MAGIC.as_slice())
        || usize::from(read_u16(table.bytes, 4).ok_or_else(bad)?) != class_count
        || read_u16(table.bytes, 6).ok_or_else(bad)? != 0
    {
        return Err(bad());
    }
    let rows = &table.bytes[format::BOUNDARY_HEADER_LEN..];
    let boundary_bit = |rid: usize, lid: usize| {
        rows.get(rid * row_bytes + lid / 8)
            .is_some_and(|byte| byte & (1u8 << (lid % 8)) != 0)
    };
    for rid in 0..class_count {
        if !boundary_bit(rid, 0) || !boundary_bit(0, rid) {
            return Err(bad());
        }
        for padding_bit in class_count..row_bytes * 8 {
            if boundary_bit(rid, padding_bit) {
                return Err(bad());
            }
        }
    }
    Ok(rows)
}

/// Validates the optional single-kanji tables.
///
/// Everything the lookups depend on is established once here so they can index
/// without re-deriving bounds: index records ascend strictly by reading bytes,
/// every reading slice is in range and valid UTF-8, every character span is in
/// range, every stored scalar is a real `char`, and every variant record
/// ascends strictly by variant scalar with a decodable kind.  A table that
/// fails any of these is rejected rather than degraded, because a half-trusted
/// index would surface silently wrong characters.
pub(super) fn validate_single_kanji_table(table: &SingleKanji<'_>) -> Result<(), Error> {
    use image_format as format;

    let bad_index = || Error::BadTable(format::TAG_SINGLE_KANJI_INDEX);
    let bad_readings = || Error::BadTable(format::TAG_SINGLE_KANJI_READINGS);
    let bad_chars = || Error::BadTable(format::TAG_SINGLE_KANJI_CHARS);
    let bad_variants = || Error::BadTable(format::TAG_SINGLE_KANJI_VARIANTS);

    if table.index_count == 0 || table.char_count == 0 {
        return Err(bad_index());
    }
    let mut previous: Option<&[u8]> = None;
    for record in 0..table.index_count {
        let at = record * format::SINGLE_KANJI_INDEX_LEN;
        let reading_offset = to_usize(read_u32(table.index, at).ok_or_else(bad_index)?)?;
        let char_start = to_usize(read_u32(table.index, at + 4).ok_or_else(bad_index)?)?;
        let reading_len = usize::from(read_u16(table.index, at + 8).ok_or_else(bad_index)?);
        let chars = usize::from(read_u16(table.index, at + 10).ok_or_else(bad_index)?);
        if reading_len == 0 || chars == 0 {
            return Err(bad_index());
        }
        let reading_end = reading_offset
            .checked_add(reading_len)
            .ok_or_else(bad_index)?;
        let reading = table
            .readings
            .get(reading_offset..reading_end)
            .ok_or_else(bad_readings)?;
        if core::str::from_utf8(reading).is_err() {
            return Err(bad_readings());
        }
        if previous.is_some_and(|earlier| earlier >= reading) {
            return Err(bad_index());
        }
        previous = Some(reading);
        let char_end = char_start.checked_add(chars).ok_or_else(bad_index)?;
        if char_end > table.char_count {
            return Err(bad_index());
        }
    }
    for scalar in 0..table.char_count {
        let at = scalar * format::SINGLE_KANJI_CHAR_LEN;
        let value = read_u32(table.chars, at).ok_or_else(bad_chars)?;
        if char::from_u32(value).is_none() {
            return Err(bad_chars());
        }
    }
    let mut previous_variant = None;
    for record in 0..table.variant_count {
        let at = record * format::SINGLE_KANJI_VARIANT_LEN;
        let variant = read_u32(table.variants, at).ok_or_else(bad_variants)?;
        let original = read_u32(table.variants, at + 4).ok_or_else(bad_variants)?;
        let kind = *table.variants.get(at + 8).ok_or_else(bad_variants)?;
        let padding = table
            .variants
            .get(at + 9..at + 12)
            .ok_or_else(bad_variants)?;
        if char::from_u32(variant).is_none()
            || char::from_u32(original).is_none()
            || SingleKanjiVariantKind::from_code(kind).is_none()
            || padding != [0, 0, 0]
            || previous_variant.is_some_and(|earlier| earlier >= variant)
        {
            return Err(bad_variants());
        }
        previous_variant = Some(variant);
    }
    Ok(())
}

pub(super) fn validate_matrix_table(table: Table<'_>, class_count: usize) -> Result<(), Error> {
    use image_format as format;

    let bad = || Error::BadTable(format::TAG_MATRIX);
    if table.count != class_count
        || table.bytes.len() < format::MATRIX_HEADER_LEN
        || table.bytes.get(..4) != Some(format::MATRIX_MAGIC.as_slice())
        || usize::from(read_u16(table.bytes, 4).ok_or_else(bad)?) != class_count
        || read_u16(table.bytes, 6).ok_or_else(bad)? != 0
        || read_u32(table.bytes, 12).ok_or_else(bad)? != 0
    {
        return Err(bad());
    }

    let override_count =
        usize::try_from(read_u32(table.bytes, 8).ok_or_else(bad)?).map_err(|_| bad())?;
    let modes_end = format::MATRIX_HEADER_LEN
        .checked_add(class_count.checked_mul(2).ok_or_else(bad)?)
        .ok_or_else(bad)?;
    let rows_at = align_up_4(modes_end).ok_or_else(bad)?;
    if table
        .bytes
        .get(modes_end..rows_at)
        .is_none_or(|padding| padding.iter().any(|byte| *byte != 0))
    {
        return Err(bad());
    }
    let rows_end = rows_at
        .checked_add(
            class_count
                .checked_mul(format::MATRIX_ROW_LEN)
                .ok_or_else(bad)?,
        )
        .ok_or_else(bad)?;
    let expected_len = rows_end
        .checked_add(
            override_count
                .checked_mul(format::MATRIX_OVERRIDE_LEN)
                .ok_or_else(bad)?,
        )
        .ok_or_else(bad)?;
    if expected_len != table.bytes.len() {
        return Err(bad());
    }

    let mut next_start = 0usize;
    for row in 0..class_count {
        let mode = read_u16(table.bytes, format::MATRIX_HEADER_LEN + row * 2).ok_or_else(bad)?;
        let descriptor = rows_at + row * format::MATRIX_ROW_LEN;
        let start = usize::try_from(read_u32(table.bytes, descriptor).ok_or_else(bad)?)
            .map_err(|_| bad())?;
        let count = usize::try_from(read_u32(table.bytes, descriptor + 4).ok_or_else(bad)?)
            .map_err(|_| bad())?;
        let end = start.checked_add(count).ok_or_else(bad)?;
        if start != next_start || end > override_count {
            return Err(bad());
        }

        let mut previous_left = None;
        for index in start..end {
            let at = rows_end + index * format::MATRIX_OVERRIDE_LEN;
            let left = usize::from(read_u16(table.bytes, at).ok_or_else(bad)?);
            let cost = read_u16(table.bytes, at + 2).ok_or_else(bad)?;
            if left >= class_count
                || previous_left.is_some_and(|previous| previous >= left)
                || cost == mode
            {
                return Err(bad());
            }
            previous_left = Some(left);
        }
        next_start = end;
    }
    if next_start != override_count {
        return Err(bad());
    }
    Ok(())
}
