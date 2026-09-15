//! Read-only queries over a validated image: bunsetsu boundaries, connection
//! costs, single-kanji readings and variants, trie prefix/prediction search,
//! and surface/annotation text. The tables they read are validated once at
//! parse time (`parse.rs`, `validate.rs`).

use super::{
    align_up_4, image_format, read_offset, read_u16, read_u32, to_usize, Dictionary, Entry,
    EntryFlags, Error, FixedStr, ImageVersion, PrefixMatch, SingleKanjiVariant,
    SingleKanjiVariantKind, TextSink, MAX_PREEDIT_BYTES,
};

impl<'a> Dictionary<'a> {
    /// Whether a segment boundary separates a word ending with connection
    /// class `right_id` from a following word starting with class `left_id`.
    ///
    /// `None` means the image predates the boundary table; callers must keep
    /// their existing segment granularity instead of guessing.  Out-of-range
    /// classes report a boundary so corruption can only ever split segments,
    /// never fuse text across a real boundary.
    pub fn bunsetsu_boundary(&self, right_id: u16, left_id: u16) -> Option<bool> {
        let rows = self.boundaries?;
        let (rid, lid) = (usize::from(right_id), usize::from(left_id));
        if rid >= self.class_count || lid >= self.class_count {
            return Some(true);
        }
        let row_bytes = self.class_count.div_ceil(8);
        Some(
            rows.get(rid * row_bytes + lid / 8)
                .is_none_or(|byte| byte & (1u8 << (lid % 8)) != 0),
        )
    }

    /// Whether this image carries the optional single-kanji table.
    pub const fn has_single_kanji(&self) -> bool {
        self.single_kanji.is_some()
    }

    /// The single kanji the pinned source lists for `reading`, in its
    /// preference order, or an empty iterator when the reading names none.
    ///
    /// This is deliberately not part of conversion search.  One short reading
    /// can name hundreds of characters, so these are appended to a finished
    /// candidate list rather than given lattice edges.
    pub fn single_kanji(&self, reading: &str) -> impl Iterator<Item = char> + '_ {
        let span = self.single_kanji_span(reading);
        let chars = self.single_kanji.map(|table| table.chars).unwrap_or(&[]);
        span.map(move |scalar| {
            let at = scalar * image_format::SINGLE_KANJI_CHAR_LEN;
            // Validated at parse time: every scalar in range decodes.
            read_u32(chars, at)
                .and_then(char::from_u32)
                .unwrap_or('\u{fffd}')
        })
    }

    /// How many single kanji `reading` names, without decoding any of them.
    pub fn single_kanji_count(&self, reading: &str) -> usize {
        self.single_kanji_span(reading).len()
    }

    fn single_kanji_span(&self, reading: &str) -> core::ops::Range<usize> {
        let Some(table) = self.single_kanji else {
            return 0..0;
        };
        let needle = reading.as_bytes();
        let record_at = |record: usize| -> (&[u8], usize, usize) {
            let at = record * image_format::SINGLE_KANJI_INDEX_LEN;
            // Validated at parse time: every record is in range.
            let offset = read_u32(table.index, at).unwrap_or(0) as usize;
            let start = read_u32(table.index, at + 4).unwrap_or(0) as usize;
            let len = usize::from(read_u16(table.index, at + 8).unwrap_or(0));
            let count = usize::from(read_u16(table.index, at + 10).unwrap_or(0));
            (
                table.readings.get(offset..offset + len).unwrap_or(&[]),
                start,
                count,
            )
        };
        let (mut low, mut high) = (0usize, table.index_count);
        while low < high {
            let middle = low + (high - low) / 2;
            let (candidate, start, count) = record_at(middle);
            match candidate.cmp(needle) {
                core::cmp::Ordering::Less => low = middle + 1,
                core::cmp::Ordering::Greater => high = middle,
                core::cmp::Ordering::Equal => return start..start + count,
            }
        }
        0..0
    }

    /// The variant relation recorded for `kanji`, if the pinned rules name one.
    ///
    /// A character can appear under more than one rule in the source; the
    /// compiler keeps exactly one relation per character so a candidate can
    /// never show two contradictory notes.
    pub fn single_kanji_variant(&self, kanji: char) -> Option<SingleKanjiVariant> {
        let table = self.single_kanji?;
        let needle = u32::from(kanji);
        let (mut low, mut high) = (0usize, table.variant_count);
        while low < high {
            let middle = low + (high - low) / 2;
            let at = middle * image_format::SINGLE_KANJI_VARIANT_LEN;
            let variant = read_u32(table.variants, at)?;
            match variant.cmp(&needle) {
                core::cmp::Ordering::Less => low = middle + 1,
                core::cmp::Ordering::Greater => high = middle,
                core::cmp::Ordering::Equal => {
                    return Some(SingleKanjiVariant {
                        original: char::from_u32(read_u32(table.variants, at + 4)?)?,
                        kind: SingleKanjiVariantKind::from_code(*table.variants.get(at + 8)?)?,
                    });
                }
            }
        }
        None
    }

    /// Encoded connection-table bytes, exposed for the release size gate.
    pub const fn matrix_bytes_len(&self) -> usize {
        self.matrix.len()
    }

    /// Returns `matrix[previous_right_id][next_left_id]`.
    pub fn connection_cost(&self, previous_right_id: u16, next_left_id: u16) -> Option<u16> {
        let row = usize::from(previous_right_id);
        let column = usize::from(next_left_id);
        if row >= self.class_count || column >= self.class_count {
            return None;
        }
        let modes_at = image_format::MATRIX_HEADER_LEN;
        let mode = read_u16(self.matrix, modes_at.checked_add(row.checked_mul(2)?)?)?;
        let rows_at = align_up_4(modes_at.checked_add(self.class_count.checked_mul(2)?)?)?;
        let descriptor = rows_at.checked_add(row.checked_mul(image_format::MATRIX_ROW_LEN)?)?;
        let start = to_usize(read_u32(self.matrix, descriptor)?).ok()?;
        let count = to_usize(read_u32(self.matrix, descriptor + 4)?).ok()?;
        let overrides_at =
            rows_at.checked_add(self.class_count.checked_mul(image_format::MATRIX_ROW_LEN)?)?;

        let mut low = 0usize;
        let mut high = count;
        while low < high {
            let middle = low + (high - low) / 2;
            let index = start.checked_add(middle)?;
            let at =
                overrides_at.checked_add(index.checked_mul(image_format::MATRIX_OVERRIDE_LEN)?)?;
            let left_id = usize::from(read_u16(self.matrix, at)?);
            match left_id.cmp(&column) {
                core::cmp::Ordering::Less => low = middle + 1,
                core::cmp::Ordering::Greater => high = middle,
                core::cmp::Ordering::Equal => return read_u16(self.matrix, at + 2),
            }
        }
        Some(mode)
    }

    /// Calls `visit` for every entry whose reading is a prefix of `reading`.
    /// Returning `false` from the callback stops enumeration immediately.
    pub fn common_prefix_search(
        &self,
        reading: &str,
        mut visit: impl FnMut(PrefixMatch) -> bool,
    ) -> Result<(), Error> {
        let mut node_index = 0usize;
        let mut matched_bytes = 0usize;
        for label in reading.chars() {
            let node = self.node(node_index)?;
            let Some(child) = self.find_child(node, label) else {
                break;
            };
            node_index = child;
            matched_bytes += label.len_utf8();
            let terminal = self.node(node_index)?;
            let end = terminal
                .value_start
                .checked_add(terminal.value_count)
                .ok_or(Error::BadTree)?;
            for entry_index in terminal.value_start..end {
                let entry = self.entry(entry_index)?;
                if !visit(PrefixMatch {
                    matched_bytes,
                    entry_index,
                    entry,
                }) {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Visits entries below the trie node identified by `reading_prefix`.
    ///
    /// Exact entries on the prefix node are intentionally skipped: this API is
    /// for bounded completion/coherence lookups, not ordinary conversion. Both
    /// node and entry work are caller-bounded, and the fixed traversal stack
    /// keeps the keystroke path allocation-free even for hostile valid images.
    pub fn visit_descendant_entries(
        &self,
        reading_prefix: &str,
        node_budget: usize,
        entry_budget: usize,
        mut visit: impl FnMut(Entry) -> bool,
    ) -> Result<(), Error> {
        if node_budget == 0 || entry_budget == 0 {
            return Ok(());
        }

        let mut node_index = 0usize;
        for label in reading_prefix.chars() {
            let node = self.node(node_index)?;
            let Some(child) = self.find_child(node, label) else {
                return Ok(());
            };
            node_index = child;
        }

        #[derive(Clone, Copy)]
        struct Frame {
            node_index: usize,
            next_child: usize,
            values_visited: bool,
        }
        const EMPTY: Frame = Frame {
            node_index: 0,
            next_child: 0,
            values_visited: false,
        };

        let mut stack = [EMPTY; MAX_PREEDIT_BYTES + 1];
        // Mark the prefix node's values visited so only strict descendants are
        // returned. Its children are still traversed normally.
        stack[0] = Frame {
            node_index,
            next_child: 0,
            values_visited: true,
        };
        let mut depth = 1usize;
        let mut visited_nodes = 0usize;
        let mut visited_entries = 0usize;

        while depth != 0 && visited_nodes < node_budget && visited_entries < entry_budget {
            let frame_index = depth - 1;
            let frame = stack[frame_index];
            let node = self.node(frame.node_index)?;
            if !frame.values_visited {
                stack[frame_index].values_visited = true;
                visited_nodes += 1;
                let end = node
                    .value_start
                    .checked_add(node.value_count)
                    .ok_or(Error::BadTree)?;
                for entry_index in node.value_start..end {
                    if visited_entries >= entry_budget {
                        return Ok(());
                    }
                    visited_entries += 1;
                    if !visit(self.entry(entry_index)?) {
                        return Ok(());
                    }
                }
                continue;
            }

            if frame.next_child < node.child_count {
                let child = node
                    .first_child
                    .checked_add(frame.next_child)
                    .ok_or(Error::BadTree)?;
                stack[frame_index].next_child += 1;
                if depth >= stack.len() {
                    return Err(Error::BadTree);
                }
                stack[depth] = Frame {
                    node_index: child,
                    next_child: 0,
                    values_visited: false,
                };
                depth += 1;
            } else {
                depth -= 1;
            }
        }
        Ok(())
    }

    /// Visits every entry marked for prediction together with its complete
    /// reading. The walk is iterative, so even a valid but pathologically deep
    /// hostile image cannot overflow the process stack.
    ///
    /// This is intentionally an index-construction API, not a keystroke-path
    /// query: callers build one compact process-wide prediction index at
    /// startup and scan that bounded index for each prefix. Returning `false`
    /// stops the walk immediately.
    pub fn visit_prediction_entries(
        &self,
        mut visit: impl FnMut(&str, Entry) -> bool,
    ) -> Result<(), Error> {
        self.visit_indexed_prediction_entries(|reading, _, entry| visit(reading, entry))
    }

    /// Visits every prediction entry together with its image entry index.
    ///
    /// The stable numeric index lets process-wide auxiliary indexes retain a
    /// four-byte reference into the mapped image instead of copying the
    /// materialized runtime entry into private working memory.
    pub fn visit_indexed_prediction_entries(
        &self,
        mut visit: impl FnMut(&str, usize, Entry) -> bool,
    ) -> Result<(), Error> {
        self.visit_entries_indexed(|reading, index, entry| {
            !entry.flags.contains(EntryFlags::PREDICTION)
                || entry.prediction_cost == i32::MAX
                || visit(reading, index, entry)
        })
    }

    /// Reads one entry from the already validated mapped image.
    ///
    /// This complements [`Self::visit_indexed_prediction_entries`]; callers
    /// can keep compact entry indexes and materialize a record only while it is
    /// being ranked or rendered.
    pub fn entry_at(&self, index: usize) -> Result<Entry, Error> {
        self.entry(index)
    }

    /// Visits every dictionary entry with its complete reading. Reconversion
    /// uses this cold-path walk to recover a reading from selected committed
    /// text without carrying a second, multi-megabyte reverse index in the
    /// resident engine. Returning `false` stops immediately.
    pub fn visit_entries(&self, mut visit: impl FnMut(&str, Entry) -> bool) -> Result<(), Error> {
        self.visit_entries_indexed(|reading, _, entry| visit(reading, entry))
    }

    fn visit_entries_indexed(
        &self,
        mut visit: impl FnMut(&str, usize, Entry) -> bool,
    ) -> Result<(), Error> {
        #[derive(Clone, Copy)]
        struct Frame {
            node_index: usize,
            next_child: usize,
            values_visited: bool,
        }

        let mut reading = FixedStr::<MAX_PREEDIT_BYTES>::new();
        let mut stack = Vec::with_capacity(32);
        stack.push(Frame {
            node_index: 0,
            next_child: 0,
            values_visited: false,
        });

        while let Some(frame_index) = stack.len().checked_sub(1) {
            let frame = stack[frame_index];
            let node = self.node(frame.node_index)?;

            if !frame.values_visited {
                stack[frame_index].values_visited = true;
                let end = node
                    .value_start
                    .checked_add(node.value_count)
                    .ok_or(Error::BadTree)?;
                for entry_index in node.value_start..end {
                    let entry = self.entry(entry_index)?;
                    if !visit(reading.as_str(), entry_index, entry) {
                        return Ok(());
                    }
                }
                continue;
            }

            if frame.next_child < node.child_count {
                let child = node
                    .first_child
                    .checked_add(frame.next_child)
                    .ok_or(Error::BadTree)?;
                stack[frame_index].next_child += 1;
                reading
                    .push(self.label(child)?)
                    .map_err(|_| Error::TextOverflow)?;
                stack.push(Frame {
                    node_index: child,
                    next_child: 0,
                    values_visited: false,
                });
                continue;
            }

            stack.pop();
            if !stack.is_empty() {
                let _ = reading.pop_char();
            }
        }
        Ok(())
    }

    /// Reconstructs an entry's front-coded surface into `sink`.
    pub fn write_surface(&self, entry: Entry, sink: &mut impl TextSink) -> Result<(), Error> {
        use image_format::SURFACE_RESTART_INTERVAL;

        let surface_id = to_usize(entry.surface_id)?;
        if surface_id >= self.surface_count {
            return Err(Error::BadEntry);
        }
        let restart = surface_id - (surface_id % SURFACE_RESTART_INTERVAL);
        let mut value = FixedStr::<MAX_PREEDIT_BYTES>::new();
        let mut v2_cursor = match self.version {
            ImageVersion::V1 => None,
            ImageVersion::V2 => Some(
                read_offset(self.surface_offsets, restart / SURFACE_RESTART_INTERVAL)
                    .ok_or(Error::BadTable(image_format::TAG_SURFACE_OFFSETS))?,
            ),
        };
        for index in restart..=surface_id {
            let record = match v2_cursor {
                None => self.text_record(
                    self.surface_offsets,
                    self.surface_count,
                    self.surfaces,
                    index,
                )?,
                Some(cursor) => {
                    let suffix_at = cursor
                        .checked_add(2)
                        .ok_or(Error::BadTable(image_format::TAG_SURFACES))?;
                    let suffix_len = usize::from(
                        read_u16(self.surfaces, suffix_at)
                            .ok_or(Error::BadTable(image_format::TAG_SURFACES))?,
                    );
                    let end = cursor
                        .checked_add(4)
                        .and_then(|at| at.checked_add(suffix_len))
                        .ok_or(Error::BadTable(image_format::TAG_SURFACES))?;
                    let record = self
                        .surfaces
                        .get(cursor..end)
                        .ok_or(Error::BadTable(image_format::TAG_SURFACES))?;
                    v2_cursor = Some(end);
                    record
                }
            };
            if record.len() < 4 {
                return Err(Error::BadTable(image_format::TAG_SURFACES));
            }
            let prefix = usize::from(
                read_u16(record, 0).ok_or(Error::BadTable(image_format::TAG_SURFACES))?,
            );
            let suffix_len = usize::from(
                read_u16(record, 2).ok_or(Error::BadTable(image_format::TAG_SURFACES))?,
            );
            if record.len() != 4 + suffix_len
                || prefix > value.len()
                || !value.as_str().is_char_boundary(prefix)
                || (index == restart && prefix != 0)
            {
                return Err(Error::BadTable(image_format::TAG_SURFACES));
            }
            let suffix = core::str::from_utf8(&record[4..]).map_err(|_| Error::BadUtf8)?;
            let keep_chars = value.as_str()[..prefix].chars().count();
            let remove_chars = value.as_str().chars().count() - keep_chars;
            value.truncate_chars(remove_chars);
            value.push_str(suffix).map_err(|_| Error::TextOverflow)?;
        }
        sink.push_str(value.as_str())
            .map_err(|_| Error::TextOverflow)
    }

    /// Writes the optional annotation for `entry`; a missing annotation writes
    /// nothing and succeeds.
    pub fn write_annotation(&self, entry: Entry, sink: &mut impl TextSink) -> Result<(), Error> {
        let annotation_id = match self.version {
            ImageVersion::V1 => {
                if entry.annotation_locator == image_format::NO_ANNOTATION {
                    return Ok(());
                }
                to_usize(entry.annotation_locator)?
            }
            ImageVersion::V2 => {
                let wanted = entry.annotation_locator;
                let index = self
                    .annotation_index
                    .ok_or(Error::BadTable(image_format::TAG_ANNOTATION_INDEX))?;
                let mut low = 0usize;
                let mut high = self.annotation_index_count;
                let found = loop {
                    if low >= high {
                        break None;
                    }
                    let middle = low + (high - low) / 2;
                    let at = middle
                        .checked_mul(image_format::ANNOTATION_INDEX_LEN_V2)
                        .ok_or(Error::BadTable(image_format::TAG_ANNOTATION_INDEX))?;
                    let ordinal = read_u32(index, at)
                        .ok_or(Error::BadTable(image_format::TAG_ANNOTATION_INDEX))?;
                    match ordinal.cmp(&wanted) {
                        core::cmp::Ordering::Less => low = middle + 1,
                        core::cmp::Ordering::Greater => high = middle,
                        core::cmp::Ordering::Equal => {
                            break Some(to_usize(
                                read_u32(index, at + 4)
                                    .ok_or(Error::BadTable(image_format::TAG_ANNOTATION_INDEX))?,
                            )?)
                        }
                    }
                };
                let Some(annotation_id) = found else {
                    return Ok(());
                };
                annotation_id
            }
        };
        let record = self.text_record(
            self.annotation_offsets,
            self.annotation_count,
            self.annotations,
            annotation_id,
        )?;
        let text = core::str::from_utf8(record).map_err(|_| Error::BadUtf8)?;
        sink.push_str(text).map_err(|_| Error::TextOverflow)
    }
}
