//! Reviewed dictionary details (DIDX/DREC/DREL/DTOF/DTXT tables, Issue #30).
//!
//! `detail_at` resolves one exact ENTR ordinal through the sorted detail index;
//! `DictionaryDetail` reads its description and relations. Structural table
//! checks run once at parse time in `validate.rs`.

use super::{
    image_format, read_offset, read_u32, to_usize, DetailRelationKind, Dictionary,
    DictionaryDetail, Error, FixedStr, TextSink,
};

impl<'a> Dictionary<'a> {
    /// Returns source-backed details for one exact ENTR-table ordinal.  Old
    /// images return `Ok(None)`; surface-only lookup is intentionally absent
    /// because homographs may have unrelated meanings.
    pub fn detail_at(&self, entry_index: usize) -> Result<Option<DictionaryDetail<'a>>, Error> {
        let Some(details) = self.details else {
            return Ok(None);
        };
        if entry_index >= self.entry_count {
            return Err(Error::BadEntry);
        }
        let wanted = entry_index;
        let mut low = 0usize;
        let mut high = details.index_count;
        while low < high {
            let middle = low + (high - low) / 2;
            let at = middle
                .checked_mul(image_format::DETAIL_INDEX_LEN)
                .ok_or(Error::BadTable(image_format::TAG_DETAIL_INDEX))?;
            let indexed_entry = to_usize(
                read_u32(details.index, at)
                    .ok_or(Error::BadTable(image_format::TAG_DETAIL_INDEX))?,
            )?;
            match indexed_entry.cmp(&wanted) {
                core::cmp::Ordering::Less => low = middle + 1,
                core::cmp::Ordering::Greater => high = middle,
                core::cmp::Ordering::Equal => {
                    let record_index = to_usize(
                        read_u32(details.index, at + 4)
                            .ok_or(Error::BadTable(image_format::TAG_DETAIL_INDEX))?,
                    )?;
                    if record_index >= details.record_count {
                        return Err(Error::BadTable(image_format::TAG_DETAIL_INDEX));
                    }
                    return Ok(Some(DictionaryDetail {
                        details,
                        record_index,
                    }));
                }
            }
        }
        Ok(None)
    }
}

impl<'a> DictionaryDetail<'a> {
    /// Writes the complete source description without UI-specific shortening.
    pub fn write_description(&self, sink: &mut impl TextSink) -> Result<(), Error> {
        self.write_text(0, sink)
    }

    /// Copies a bounded UTF-8 preview into a fixed caller buffer and reports
    /// whether source text remains.  This never treats a long source
    /// definition as malformed, allocates no memory, and never splits a UTF-8
    /// scalar.  The caller owns its visible-line policy.
    pub fn write_description_preview<const N: usize>(
        &self,
        sink: &mut FixedStr<N>,
        maximum_bytes: usize,
    ) -> Result<bool, Error> {
        let text = self.description()?;
        let remaining = sink.capacity().saturating_sub(sink.len());
        let mut end = text.len().min(maximum_bytes).min(remaining);
        while end != 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        sink.push_str(&text[..end])
            .map_err(|_| Error::TextOverflow)?;
        Ok(end != text.len())
    }

    /// Writes the complete description for callers that choose a display
    /// channel.  Line limits are applied by the renderer, not the dictionary.
    pub fn write_display_description(&self, sink: &mut impl TextSink) -> Result<(), Error> {
        self.write_text(4, sink)
    }

    /// Visits explicit relationships in source order.  The target text is
    /// borrowed from the mapped image and remains valid for the dictionary's
    /// lifetime.
    pub fn visit_relations(
        &self,
        mut visit: impl FnMut(DetailRelationKind, &str) -> bool,
    ) -> Result<(), Error> {
        let at = self.record_at()?;
        let start = to_usize(
            read_u32(self.details.records, at + 8)
                .ok_or(Error::BadTable(image_format::TAG_DETAIL_RECORDS))?,
        )?;
        let count = to_usize(
            read_u32(self.details.records, at + 12)
                .ok_or(Error::BadTable(image_format::TAG_DETAIL_RECORDS))?,
        )?;
        for index in start
            ..start
                .checked_add(count)
                .ok_or(Error::BadTable(image_format::TAG_DETAIL_RECORDS))?
        {
            let relation_at = index
                .checked_mul(image_format::DETAIL_RELATION_LEN)
                .ok_or(Error::BadTable(image_format::TAG_DETAIL_RELATIONS))?;
            let kind = DetailRelationKind::from_byte(
                *self
                    .details
                    .relations
                    .get(relation_at)
                    .ok_or(Error::BadTable(image_format::TAG_DETAIL_RELATIONS))?,
            )
            .ok_or(Error::BadTable(image_format::TAG_DETAIL_RELATIONS))?;
            let text_id = to_usize(
                read_u32(self.details.relations, relation_at + 4)
                    .ok_or(Error::BadTable(image_format::TAG_DETAIL_RELATIONS))?,
            )?;
            let text = self.text(text_id)?;
            if !visit(kind, text) {
                break;
            }
        }
        Ok(())
    }

    fn write_text(&self, field_offset: usize, sink: &mut impl TextSink) -> Result<(), Error> {
        let at = self.record_at()?;
        let text_id = to_usize(
            read_u32(self.details.records, at + field_offset)
                .ok_or(Error::BadTable(image_format::TAG_DETAIL_RECORDS))?,
        )?;
        sink.push_str(self.text(text_id)?)
            .map_err(|_| Error::TextOverflow)
    }

    fn description(&self) -> Result<&'a str, Error> {
        let at = self.record_at()?;
        let text_id = to_usize(
            read_u32(self.details.records, at)
                .ok_or(Error::BadTable(image_format::TAG_DETAIL_RECORDS))?,
        )?;
        self.text(text_id)
    }

    fn record_at(&self) -> Result<usize, Error> {
        self.record_index
            .checked_mul(image_format::DETAIL_RECORD_LEN)
            .filter(|at| *at + image_format::DETAIL_RECORD_LEN <= self.details.records.len())
            .ok_or(Error::BadTable(image_format::TAG_DETAIL_RECORDS))
    }

    fn text(&self, index: usize) -> Result<&'a str, Error> {
        if index >= self.details.text_count {
            return Err(Error::BadTable(image_format::TAG_DETAIL_TEXT));
        }
        let start = read_offset(self.details.text_offsets, index)
            .ok_or(Error::BadTable(image_format::TAG_DETAIL_TEXT))?;
        let end = if index + 1 < self.details.text_count {
            read_offset(self.details.text_offsets, index + 1)
                .ok_or(Error::BadTable(image_format::TAG_DETAIL_TEXT))?
        } else {
            self.details.text.len()
        };
        let bytes = self
            .details
            .text
            .get(start..end)
            .ok_or(Error::BadTable(image_format::TAG_DETAIL_TEXT))?;
        core::str::from_utf8(bytes).map_err(|_| Error::BadUtf8)
    }
}
