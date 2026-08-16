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
    for entry in &curated {
        assert!(
            entry
                .reading
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()),
            "{} must be typeable as a continuous Shift+ASCII run",
            entry.reading
        );
        assert!(
            entry.flags.contains(EntryFlags::IT),
            "{} is not IT",
            entry.surface
        );
        assert!(
            entry.flags.contains(EntryFlags::PREDICTION),
            "{} is not predictive",
            entry.surface
        );
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
