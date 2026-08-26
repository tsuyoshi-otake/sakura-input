use std::collections::BTreeSet;
use std::path::Path;

use dictc::{merge_entries, parse_entries};
use sakura_core::dictionary::EntryFlags;

const REQUIRED_TERMS: &[(&str, &str)] = &[
    ("actix", "Actix"),
    ("amazon", "Amazon"),
    ("android", "Android"),
    ("apple", "Apple"),
    ("astro", "Astro"),
    ("axum", "Axum"),
    ("biome", "Biome"),
    ("bitbucket", "Bitbucket"),
    ("bun", "Bun"),
    ("cargo", "Cargo"),
    ("claude", "Claude Desktop"),
    ("claudedesktop", "Claude Desktop"),
    ("cloudflare", "Cloudflare"),
    ("chrome", "Google Chrome"),
    ("cursor", "Cursor"),
    ("dart", "Dart"),
    ("discord", "Discord"),
    ("django", "Django"),
    ("dropbox", "Dropbox"),
    ("electron", "Electron"),
    ("eslint", "ESLint"),
    ("excel", "Excel"),
    ("firefox", "Firefox"),
    ("flask", "Flask"),
    ("gitlab", "GitLab"),
    ("gmail", "Gmail"),
    ("go", "Go"),
    ("google", "Google"),
    ("googlechrome", "Google Chrome"),
    ("googledrive", "Google Drive"),
    ("grok", "Grok"),
    ("heroku", "Heroku"),
    ("homebrew", "Homebrew"),
    ("intel", "Intel"),
    ("intellij", "IntelliJ IDEA"),
    ("intellijidea", "IntelliJ IDEA"),
    ("ios", "iOS"),
    ("jira", "Jira"),
    ("keras", "Keras"),
    ("langchain", "LangChain"),
    ("linkedin", "LinkedIn"),
    ("llama", "Llama"),
    ("llamaindex", "LlamaIndex"),
    ("meta", "Meta"),
    ("microsoft", "Microsoft"),
    ("microsoft365", "Microsoft 365"),
    ("microsoftedge", "Microsoft Edge"),
    ("microsoftteams", "Microsoft Teams"),
    ("mistral", "Mistral"),
    ("mozilla", "Mozilla"),
    ("netlify", "Netlify"),
    ("notion", "Notion"),
    ("npm", "npm"),
    ("office365", "Office 365"),
    ("ollama", "Ollama"),
    ("onedrive", "OneDrive"),
    ("onenote", "OneNote"),
    ("openaiapi", "OpenAI API"),
    ("oracle", "Oracle"),
    ("outlook", "Outlook"),
    ("perplexity", "Perplexity"),
    ("pnpm", "pnpm"),
    ("prettier", "Prettier"),
    ("pytorch", "PyTorch"),
    ("rails", "Rails"),
    ("reddit", "Reddit"),
    ("remix", "Remix"),
    ("ruby", "Ruby"),
    ("ruff", "Ruff"),
    ("rust", "Rust"),
    ("safari", "Safari"),
    ("sakurainput", "Sakura Input"),
    ("serde", "Serde"),
    ("sharepoint", "SharePoint"),
    ("slack", "Slack"),
    ("supabase", "Supabase"),
    ("svelte", "Svelte"),
    ("tauri", "Tauri"),
    ("tensorflow", "TensorFlow"),
    ("tiktok", "TikTok"),
    ("tokio", "Tokio"),
    ("uv", "uv"),
    ("vercel", "Vercel"),
    ("vue", "Vue.js"),
    ("vuejs", "Vue.js"),
    ("whatsapp", "WhatsApp"),
    ("windsurf", "Windsurf"),
    ("word", "Word"),
    ("xai", "xAI"),
    ("yarn", "Yarn"),
    ("youtube", "YouTube"),
    ("zoom", "Zoom"),
];

fn data_file(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("data")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

#[test]
fn curated_terms_cover_canonical_shift_input_without_shadow_duplicates() {
    let curated_text = data_file("curated-terms.tsv");
    let curated = parse_entries("data/curated-terms.tsv", &curated_text).expect("curated terms");
    let generated_text = data_file("it-terms.tsv");
    let generated = parse_entries("data/it-terms.tsv", &generated_text).expect("generated terms");

    let mut identities = BTreeSet::new();
    let mut shift_readings = 0usize;
    let mut kana_readings = 0usize;
    for entry in &curated {
        let shape = match reading_shape(&entry.reading) {
            Some(ReadingShape::ShiftAscii) => {
                shift_readings += 1;
                ReadingShape::ShiftAscii
            }
            Some(ReadingShape::Hiragana) => {
                kana_readings += 1;
                ReadingShape::Hiragana
            }
            None => panic!(
                "{} is neither a continuous Shift+ASCII run nor a hiragana reading",
                entry.reading
            ),
        };
        assert!(
            entry.flags.contains(EntryFlags::IT),
            "{} is not IT",
            entry.surface
        );
        // A Shift+ASCII reading only ever competes with other Latin runs, so
        // predicting from it costs general Japanese nothing. A kana reading
        // shares its prefixes with ordinary words: `えすえすえいち` is reachable
        // from `え`, and hundreds of acronyms crowding that prefix would be a
        // general-Japanese regression traded for an IT gain. These rows stay
        // conversion-only until prediction has its own evaluation.
        match shape {
            ReadingShape::ShiftAscii => assert!(
                entry.flags.contains(EntryFlags::PREDICTION) && entry.prediction_cost != i32::MAX,
                "{} is not predictive",
                entry.surface
            ),
            ReadingShape::Hiragana => assert!(
                !entry.flags.contains(EntryFlags::PREDICTION) && entry.prediction_cost == i32::MAX,
                "kana reading {} must stay out of prediction",
                entry.reading
            ),
        }
        assert!(
            entry.annotation.is_empty(),
            "{} carries a user-visible candidate note",
            entry.surface
        );
        assert!(
            identities.insert((entry.reading.as_str(), entry.surface.as_str())),
            "duplicate curated edge {} -> {}",
            entry.reading,
            entry.surface
        );
    }

    assert!(
        shift_readings > 0 && kana_readings > 0,
        "the overlay must serve both input paths, saw {shift_readings} Shift+ASCII and {kana_readings} kana readings"
    );

    for &(reading, surface) in REQUIRED_TERMS {
        assert!(
            identities.contains(&(reading, surface)),
            "missing canonical Shift term {reading} -> {surface}"
        );
    }

    let generated_identities = generated
        .iter()
        .map(|entry| (entry.reading.as_str(), entry.surface.as_str()))
        .collect::<BTreeSet<_>>();
    for identity in &identities {
        assert!(
            !generated_identities.contains(identity),
            "curated term duplicates generated glossary edge {} -> {}",
            identity.0,
            identity.1
        );
    }

    merge_entries(generated, curated).expect("generated and curated overlays must merge cleanly");
}

/// The overlay serves two input paths, and a reading belongs to exactly one of
/// them. `abcd1234` is typed as a continuous Shift+ASCII run; `あいうえー` is
/// typed as ordinary kana composition, and is the only way an ASCII surface can
/// be reached from inside a Japanese sentence. A reading that mixes the two
/// scripts is reachable from neither path, so it is a typo rather than a third
/// case.
fn reading_shape(reading: &str) -> Option<ReadingShape> {
    if reading.is_empty() {
        return None;
    }
    if reading
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return Some(ReadingShape::ShiftAscii);
    }
    if reading
        .chars()
        .all(|c| matches!(c, '\u{3041}'..='\u{3096}' | 'ー'))
    {
        return Some(ReadingShape::Hiragana);
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ReadingShape {
    ShiftAscii,
    Hiragana,
}
