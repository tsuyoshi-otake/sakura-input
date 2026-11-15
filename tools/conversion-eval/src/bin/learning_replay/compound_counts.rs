//! Count-only headroom measurement for #480.

use sakura_core::Dictionary;

#[derive(Debug, Default)]
pub(super) struct Counts {
    mismatches: usize,
    compound_swaps: usize,
    absent: usize,
    absent_first_reading: usize,
}

impl Counts {
    pub(super) fn observe(
        &mut self,
        dictionary: &Dictionary<'_>,
        reading: &str,
        committed: &str,
        first: &str,
        first_reading: bool,
    ) -> Result<(), String> {
        if first == committed {
            return Ok(());
        }
        self.mismatches += 1;
        if !compound_swap(reading, committed, first) {
            return Ok(());
        }
        self.compound_swaps += 1;
        if !exact_dictionary_entry(dictionary, reading, committed)? {
            self.absent += 1;
            self.absent_first_reading += usize::from(first_reading);
        }
        Ok(())
    }

    pub(super) fn report(&self) -> String {
        format!(
            "compound-mismatches\t{}\ncompound-swaps\t{}\ncompound-absent\t{}\ncompound-absent-first-reading\t{}\n",
            self.mismatches, self.compound_swaps, self.absent, self.absent_first_reading
        )
    }
}

fn is_kanji(c: char) -> bool {
    matches!(
        c,
        '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' |
        '\u{f900}'..='\u{faff}' | '\u{20000}'..='\u{2fa1f}' |
        '\u{30000}'..='\u{323af}' | '々'
    )
}

fn compound_swap(reading: &str, committed: &str, first: &str) -> bool {
    if reading.chars().count() < 4 {
        return false;
    }
    let left: Vec<_> = committed.chars().collect();
    let right: Vec<_> = first.chars().collect();
    if left.is_empty()
        || left.len() != right.len()
        || !left.iter().chain(&right).all(|c| is_kanji(*c))
    {
        return false;
    }
    let mut runs = 0;
    let mut in_difference = false;
    for (a, b) in left.iter().zip(&right) {
        if a != b {
            if !in_difference {
                runs += 1;
                in_difference = true;
            }
        } else {
            in_difference = false;
        }
    }
    runs == 1
}

fn exact_dictionary_entry(
    dictionary: &Dictionary<'_>,
    reading: &str,
    surface: &str,
) -> Result<bool, String> {
    let mut found = false;
    let mut error = None;
    dictionary
        .common_prefix_search(reading, |matched| {
            if matched.matched_bytes != reading.len() {
                return true;
            }
            let mut actual = String::new();
            if let Err(e) = dictionary.write_surface(matched.entry, &mut actual) {
                error = Some(e);
                return false;
            }
            if actual == surface {
                found = true;
                return false;
            }
            true
        })
        .map_err(|e| format!("dictionary lookup failed: {e:?}"))?;
    if let Some(error) = error {
        return Err(format!("dictionary surface decode failed: {error:?}"));
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compound_definition_rejects_separate_runs_and_non_kanji() {
        assert!(compound_swap("あいうえ", "山川海空", "山川森空"));
        assert!(compound_swap("あいうえ", "山川海空", "山森林空"));
        assert!(compound_swap("あいうえ", "山川", "森林"));
        assert!(compound_swap("あいうえ", "時々", "刻々"));
        assert!(!compound_swap("あいう", "山川", "山林"));
        assert!(!compound_swap("あいうえ", "山川海空", "森川林空"));
        assert!(!compound_swap("あいうえ", "山川", "山川"));
        assert!(!compound_swap("あいうえ", "山川", "山林海"));
        assert!(!compound_swap("あいうえ", "山川", "山かな"));
    }

    #[test]
    fn exact_lookup_requires_full_reading_and_surface() {
        let source = "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
                      あい\t山川\t0\t0\t100\t100\t\tfixture\n\
                      あいうえ\t山林\t0\t0\t100\t100\t\tfixture\n";
        let entries = dictc_core::parse_entries("fixture.tsv", source).expect("entries");
        let matrix = dictc_core::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t1\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        let image = dictc_core::compile(&entries, &matrix).expect("image");
        let dictionary = Dictionary::parse(&image).expect("dictionary");
        assert!(exact_dictionary_entry(&dictionary, "あいうえ", "山林").unwrap());
        assert!(!exact_dictionary_entry(&dictionary, "あいうえ", "山川").unwrap());
        assert!(!exact_dictionary_entry(&dictionary, "あいうえ", "山海").unwrap());
    }
}
