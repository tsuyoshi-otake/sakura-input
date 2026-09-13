use super::super::Converter;

/// Once a trustworthy whole-reading entry exists, a much more expensive
/// all-system segmentation is usually a mosaic of individually valid short
/// words. Word-sized Japanese readings and atomic loanwords use this gate;
/// long Japanese compounds still retain legitimate split alternatives.
const EXACT_LEXICAL_COMPOSITE_COST_WINDOW: i64 = 4_000;
const MAX_EXACT_WORD_READING_CHARS: usize = 6;

impl Converter {
    /// Protects trustworthy whole-reading lexical evidence from speculative
    /// repair paths and the low-information tail of a fully lexical N-best
    /// search. A repair remains available when no direct exact entry exists;
    /// it is only suppressed when the dictionary already answers the query.
    pub(in crate::conversion) fn apply_exact_lexical_quality_gate(&mut self, reading: &str) {
        let is_word_sized = reading.chars().count() <= MAX_EXACT_WORD_READING_CHARS;
        let suppress_unconfirmed_repairs = self.candidates.iter().any(|candidate| {
            candidate.system_entry_index().is_some()
                && !candidate.path_evidence().has_unconfirmed_repair()
                && is_trustworthy_exact_surface(candidate.text())
        });
        let Some(best_exact_cost) = self
            .candidates
            .iter()
            .filter(|candidate| {
                candidate.system_entry_index().is_some()
                    && !candidate.path_evidence().has_unconfirmed_repair()
                    && (is_word_sized || is_atomic_whole_reading_surface(candidate.text()))
            })
            .map(|candidate| candidate.cost)
            .min()
        else {
            return;
        };
        let maximum_composite_cost =
            best_exact_cost.saturating_add(EXACT_LEXICAL_COMPOSITE_COST_WINDOW);
        self.candidates.retain(|candidate| {
            let evidence = candidate.path_evidence();
            !(suppress_unconfirmed_repairs && evidence.has_unconfirmed_repair())
                && (candidate.system_entry_index().is_some()
                    || evidence.user_edges != 0
                    || evidence.system_edges < 2
                    || evidence.fallback_edges != 0
                    || evidence.generated_edges != 0
                    || candidate.cost <= maximum_composite_cost)
        });
    }
}

fn is_atomic_whole_reading_surface(surface: &str) -> bool {
    let mut characters = surface.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if characters.next().is_none() {
        return !first.is_whitespace();
    }
    surface.chars().all(|character| {
        ('\u{30a0}'..='\u{30ff}').contains(&character)
            || ('\u{ff65}'..='\u{ff9f}').contains(&character)
            || character.is_ascii_alphanumeric()
            || matches!(character, ' ' | '-' | '_' | '.' | '+' | '#' | '/')
    })
}

fn is_trustworthy_exact_surface(surface: &str) -> bool {
    let mut characters = surface.chars();
    if characters.next().is_none() || characters.next().is_none() {
        return false;
    }
    is_atomic_whole_reading_surface(surface)
        || surface.chars().all(|character| {
            matches!(
                character,
                '\u{3400}'..='\u{4dbf}'
                    | '\u{4e00}'..='\u{9fff}'
                    | '\u{f900}'..='\u{faff}'
                    | '\u{20000}'..='\u{2ffff}'
            )
        })
}
