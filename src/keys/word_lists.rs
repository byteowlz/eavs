//! Human-readable ID generation using adjective-noun combinations.
//!
//! Generates memorable IDs like "cold-lamp" or "blue-frog" for virtual API keys.
//! Word lists are loaded from a TOML file, which can be configured or auto-downloaded
//! from the eavs GitHub repository.

use rand::seq::SliceRandom;
use std::path::Path;

/// Default URL for downloading word lists from eavs GitHub repository.
const WORD_LISTS_URL: &str =
    "https://raw.githubusercontent.com/wismut/eavs/main/data/word_lists.toml";

/// Word lists for human-readable ID generation.
#[derive(Debug, Clone)]
pub struct WordLists {
    pub adjectives: Vec<String>,
    pub nouns: Vec<String>,
}

/// Errors that can occur when working with word lists.
#[derive(Debug)]
pub enum WordListError {
    /// Failed to read word lists file.
    Io(std::io::Error),
    /// Failed to parse TOML content.
    Parse(String),
    /// Failed to download word lists.
    Download(String),
    /// Word lists are empty or invalid.
    Empty(String),
}

impl std::fmt::Display for WordListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WordListError::Io(e) => write!(f, "IO error: {}", e),
            WordListError::Parse(e) => write!(f, "Parse error: {}", e),
            WordListError::Download(e) => write!(f, "Download error: {}", e),
            WordListError::Empty(e) => write!(f, "Empty word lists: {}", e),
        }
    }
}

impl std::error::Error for WordListError {}

impl From<std::io::Error> for WordListError {
    fn from(e: std::io::Error) -> Self {
        WordListError::Io(e)
    }
}

impl WordLists {
    /// Parse word lists from TOML content.
    pub fn from_toml(content: &str) -> Result<Self, WordListError> {
        let parsed: toml::Value = content
            .parse()
            .map_err(|e| WordListError::Parse(format!("parsing word lists: {e}")))?;

        let adjectives = parsed
            .get("adjectives")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let nouns = parsed
            .get("nouns")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let lists = Self { adjectives, nouns };
        lists.validate()?;
        Ok(lists)
    }

    /// Load word lists from a file path.
    pub fn from_file(path: &Path) -> Result<Self, WordListError> {
        let content = std::fs::read_to_string(path)?;
        Self::from_toml(&content)
    }

    /// Load word lists, downloading from GitHub if the file doesn't exist.
    ///
    /// This is a blocking operation that will download the file synchronously
    /// if it doesn't exist at the specified path.
    pub fn load_or_download(path: &Path) -> Result<Self, WordListError> {
        if path.exists() {
            return Self::from_file(path);
        }

        // Download from GitHub
        tracing::info!("Word lists not found at {:?}, downloading from GitHub...", path);
        let content = download_word_lists()?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Save to disk for future use
        std::fs::write(path, &content)?;
        tracing::info!("Word lists saved to {:?}", path);

        Self::from_toml(&content)
    }

    /// Load word lists asynchronously, downloading from GitHub if the file doesn't exist.
    pub async fn load_or_download_async(path: &Path) -> Result<Self, WordListError> {
        if path.exists() {
            let content = tokio::fs::read_to_string(path).await?;
            return Self::from_toml(&content);
        }

        // Download from GitHub
        tracing::info!("Word lists not found at {:?}, downloading from GitHub...", path);
        let content = download_word_lists_async().await?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // Save to disk for future use
        tokio::fs::write(path, &content).await?;
        tracing::info!("Word lists saved to {:?}", path);

        Self::from_toml(&content)
    }

    /// Validate that word lists are usable.
    fn validate(&self) -> Result<(), WordListError> {
        if self.adjectives.is_empty() {
            return Err(WordListError::Empty("no adjectives found".into()));
        }
        if self.nouns.is_empty() {
            return Err(WordListError::Empty("no nouns found".into()));
        }
        Ok(())
    }

    /// Get the number of possible unique combinations.
    ///
    /// This excludes same-word pairs (e.g., "cold-cold").
    pub fn combination_count(&self) -> usize {
        let total = self.adjectives.len() * self.nouns.len();
        let overlap = self
            .adjectives
            .iter()
            .filter(|adj| self.nouns.contains(adj))
            .count();
        total - overlap
    }

    /// Generate a random human-readable ID.
    ///
    /// Format: `<adjective>-<noun>` (e.g., "cold-lamp", "blue-frog")
    pub fn generate_id(&self) -> Option<String> {
        let mut rng = rand::thread_rng();
        let adj = self.adjectives.choose(&mut rng)?;
        let noun = self.nouns.choose(&mut rng)?;

        // Avoid same-word combinations
        if adj == noun {
            // Try again with a different noun
            let filtered: Vec<_> = self.nouns.iter().filter(|n| *n != adj).collect();
            let noun = filtered.choose(&mut rng)?;
            return Some(format!("{}-{}", adj, noun));
        }

        Some(format!("{}-{}", adj, noun))
    }

    /// Generate multiple unique human-readable IDs.
    ///
    /// Returns up to `count` unique IDs. May return fewer if there aren't
    /// enough unique combinations available.
    pub fn generate_ids(&self, count: usize) -> Vec<String> {
        let mut ids = std::collections::HashSet::with_capacity(count);
        let max_attempts = count * 10; // Prevent infinite loops

        for _ in 0..max_attempts {
            if ids.len() >= count {
                break;
            }
            if let Some(id) = self.generate_id() {
                ids.insert(id);
            }
        }

        ids.into_iter().collect()
    }

    /// Get embedded fallback word lists (minimal set compiled into binary).
    pub fn embedded() -> Self {
        Self {
            adjectives: EMBEDDED_ADJECTIVES.iter().map(|s| s.to_string()).collect(),
            nouns: EMBEDDED_NOUNS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// Download word lists from GitHub (blocking).
fn download_word_lists() -> Result<String, WordListError> {
    // Use ureq for synchronous HTTP requests (already in dependencies via other crates)
    // Fall back to embedded lists if download fails
    match ureq::get(WORD_LISTS_URL).call() {
        Ok(response) => response
            .into_string()
            .map_err(|e| WordListError::Download(format!("reading response: {e}"))),
        Err(e) => {
            tracing::warn!("Failed to download word lists: {}, using embedded fallback", e);
            Err(WordListError::Download(e.to_string()))
        }
    }
}

/// Download word lists from GitHub (async).
async fn download_word_lists_async() -> Result<String, WordListError> {
    let client = reqwest::Client::new();
    match client.get(WORD_LISTS_URL).send().await {
        Ok(response) => {
            if response.status().is_success() {
                response
                    .text()
                    .await
                    .map_err(|e| WordListError::Download(format!("reading response: {e}")))
            } else {
                Err(WordListError::Download(format!(
                    "HTTP {}: {}",
                    response.status(),
                    response.status().canonical_reason().unwrap_or("Unknown")
                )))
            }
        }
        Err(e) => {
            tracing::warn!("Failed to download word lists: {}, using embedded fallback", e);
            Err(WordListError::Download(e.to_string()))
        }
    }
}

/// Embedded adjectives (minimal fallback set).
const EMBEDDED_ADJECTIVES: &[&str] = &[
    "able", "acid", "aged", "airy", "akin", "alto", "arch", "arid", "avid", "back",
    "bald", "bare", "base", "bass", "beat", "bent", "best", "beta", "blue", "bold",
    "bone", "bony", "boon", "born", "boss", "buff", "bulk", "bush", "bust", "busy",
    "calm", "camp", "chic", "clad", "cold", "cool", "cosy", "cozy", "curt", "cute",
    "cyan", "daft", "damp", "dank", "dark", "deaf", "dear", "deep", "deft", "dire",
    "dirt", "done", "dour", "down", "drab", "dual", "dull", "dyed", "each", "east",
    "easy", "edgy", "epic", "even", "evil", "eyed", "fair", "fake", "fast", "faux",
    "fell", "fine", "firm", "five", "flat", "flip", "fond", "foul", "foxy", "free",
    "full", "gaga", "game", "gilt", "glad", "glib", "glum", "gold", "gone", "good",
    "gray", "grey", "grim", "hale", "half", "halt", "hard", "hazy", "held", "here",
    "hick", "high", "hind", "holy", "home", "huge", "iced", "icky", "idle", "iffy",
    "inky", "iron", "just", "keen", "kept", "kind", "lacy", "laid", "lame", "lank",
    "last", "late", "lazy", "lean", "left", "less", "lest", "like", "limp", "lite",
    "live", "loco", "lone", "long", "lost", "loud", "lush", "luxe", "made", "main",
    "male", "many", "mass", "maxi", "mean", "meek", "meet", "mere", "midi", "mild",
    "mini", "mint", "mock", "mono", "moot", "more", "most", "much", "must", "mute",
    "near", "neat", "next", "nice", "nigh", "nine", "none", "nosy", "nude", "null",
    "numb", "nuts", "oily", "okay", "only", "open", "oral", "oval", "over", "paid",
    "pale", "pass", "past", "pent", "pied", "pink", "plus", "poor", "port", "posh",
    "prim", "puff", "punk", "puny", "pure", "racy", "rank", "rare", "rash", "real",
    "rear", "rich", "rife", "ripe", "roan", "rosy", "rude", "rust", "safe", "salt",
    "same", "sane", "sear", "self", "sent", "sewn", "sham", "shed", "shot", "shut",
    "side", "sign", "size", "skew", "skim", "slim", "slow", "smug", "snub", "snug",
    "soft", "sold", "sole", "solo", "some", "sore", "sour", "sown", "spry", "star",
    "such", "sunk", "sure", "tall", "tame", "tart", "taut", "teal", "teen", "then",
    "thin", "tidy", "tied", "tiny", "toed", "tops", "torn", "trig", "trim", "true",
    "twin", "ugly", "used", "vain", "vast", "very", "vile", "void", "warm", "wary",
    "wavy", "waxy", "weak", "wide", "wild", "wily", "wise", "worn", "zany", "zero",
];

/// Embedded nouns (minimal fallback set).
const EMBEDDED_NOUNS: &[&str] = &[
    "acid", "acre", "acts", "aged", "aide", "aims", "airs", "ally", "aloe", "alto",
    "amen", "amps", "ante", "anti", "ants", "apes", "apex", "aqua", "arch", "arcs",
    "area", "aria", "arms", "army", "arts", "atom", "aunt", "aura", "auto", "axes",
    "axis", "axle", "babe", "baby", "back", "bags", "bail", "bait", "bale", "ball",
    "balm", "band", "bane", "bang", "bank", "bans", "barb", "bark", "barn", "bars",
    "base", "bash", "bass", "bath", "bats", "bays", "bead", "beak", "beam", "bean",
    "bear", "beat", "beds", "beef", "beer", "bees", "beet", "bell", "belt", "bend",
    "bent", "best", "beta", "bets", "bias", "bids", "bike", "bill", "bind", "bins",
    "bird", "bite", "bits", "blob", "bloc", "blog", "blot", "blow", "blue", "blur",
    "boar", "boat", "body", "boil", "bold", "bolt", "bond", "bone", "book", "boom",
    "boon", "boot", "bore", "born", "boss", "bout", "bowl", "bows", "boys", "brag",
    "bran", "bras", "brat", "brew", "brig", "brim", "brit", "brow", "buck", "buds",
    "buff", "bugs", "bulb", "bulk", "bull", "bump", "bums", "bunk", "buns", "buoy",
    "burn", "burr", "bush", "bust", "buys", "buzz", "byte", "cabs", "cafe", "cage",
    "cake", "calf", "call", "calm", "camo", "camp", "cams", "cane", "cans", "cape",
    "caps", "card", "care", "carp", "cars", "cart", "case", "cash", "cast", "cats",
    "cave", "cell", "cent", "chap", "char", "chat", "chef", "chew", "chic", "chin",
    "chip", "chop", "cite", "city", "clam", "clan", "clap", "claw", "clay", "clip",
    "clot", "club", "clue", "coal", "coat", "coca", "coco", "code", "coil", "coin",
    "cola", "cold", "colt", "coma", "comb", "come", "comp", "cone", "cons", "cool",
    "coop", "cope", "cops", "copy", "cord", "core", "cork", "corn", "corp", "cost",
    "cosy", "coup", "cove", "cows", "cozy", "crab", "crew", "crib", "crop", "crow",
    "crux", "cube", "cubs", "cues", "cuff", "cult", "cups", "curb", "cure", "curl",
    "cusp", "cuts", "cyst", "dads", "dame", "damp", "dams", "dare", "dark", "darn",
    "dart", "dash", "data", "date", "days", "deaf", "deal", "dear", "debt", "deck",
    "deed", "deep", "deer", "deli", "demo", "dent", "desk", "dial", "dice", "dies",
    "diet", "digs", "dime", "ding", "dips", "dirt", "disc", "dish", "disk", "diva",
    "dive", "dock", "docs", "does", "dogs", "dole", "doll", "dome", "dong", "dons",
    "doom", "door", "dope", "dork", "dorm", "dose", "dots", "dove", "down", "drab",
    "drag", "draw", "drip", "drop", "drum", "dubs", "duck", "duct", "dude", "duel",
    "dues", "duet", "duff", "dump", "dune", "dung", "dunk", "dusk", "dust", "duty",
    "dyes", "dyke", "ears", "ease", "east", "eats", "echo", "edge", "eels", "eggs",
    "egos", "emir", "ends", "envy", "epic", "eras", "even", "evil", "exam", "exec",
    "exit", "expo", "eyes", "face", "fact", "fade", "fair", "fake", "fall", "fame",
    "fang", "fans", "fare", "farm", "fast", "fate", "fats", "fawn", "fear", "feat",
    "feds", "feed", "feel", "fees", "feet", "fell", "felt", "fern", "feud", "fife",
    "figs", "file", "fill", "film", "find", "fine", "fink", "fins", "fire", "firm",
    "fish", "fist", "fits", "five", "flag", "flak", "flap", "flat", "flaw", "flax",
    "flea", "flex", "flip", "flop", "flow", "flux", "foam", "foes", "foil", "fold",
    "folk", "font", "food", "fool", "foot", "fork", "form", "fort", "foul", "fowl",
    "frat", "fray", "free", "fret", "frog", "fuel", "full", "fund", "funk", "furs",
    "fury", "fuse", "fuss", "fuzz", "gage", "gags", "gain", "gait", "gala", "gale",
    "gall", "gals", "game", "gang", "gaps", "garb", "gasp", "gate", "gays", "gaze",
    "gear", "geek", "gems", "gent", "germ", "gets", "gift", "gigs", "gill", "gilt",
    "girl", "gist", "give", "glad", "glee", "glow", "glue", "goal", "goat", "gods",
    "goes", "gold", "golf", "gong", "good", "goon", "goth", "gout", "gown", "grab",
    "grad", "gran", "gray", "grey", "grid", "grin", "grip", "grit", "grub", "gulf",
    "gums", "guns", "gust", "guts", "guys", "gyms", "hack", "hail", "hair", "half",
    "hall", "halo", "halt", "hams", "hand", "hang", "hank", "hare", "harm", "harp",
    "hash", "hasp", "hats", "haul", "hawk", "haze", "hazy", "head", "heal", "heap",
    "heat", "heck", "heel", "heir", "held", "helm", "help", "hems", "herd", "here",
    "hero", "hick", "hide", "high", "hike", "hill", "hind", "hint", "hips", "hire",
    "hits", "hive", "hoax", "hobs", "hock", "hogs", "hold", "hole", "holy", "home",
    "hone", "honk", "hood", "hook", "hoop", "hope", "hops", "horn", "hose", "host",
    "hour", "hubs", "hues", "huff", "huge", "hugs", "hull", "hump", "hums", "hung",
    "hunk", "hunt", "hush", "husk", "huts", "hymn", "hype", "iced", "icon", "idea",
    "idle", "idol", "iffy", "inch", "info", "inks", "inky", "inns", "into", "ions",
    "iris", "iron", "isle", "itch", "item", "jabs", "jack", "jade", "jail", "jams",
    "jars", "java", "jaws", "jazz", "jean", "jeer", "jell", "jerk", "jest", "jets",
    "jobs", "jock", "jogs", "join", "joke", "jolt", "jots", "jump", "june", "junk",
    "jury", "just", "keel", "keen", "keep", "kegs", "kelp", "kept", "kick", "kids",
    "kiln", "kilt", "kind", "king", "kiss", "kite", "kits", "knee", "knit", "knob",
    "know", "labs", "lace", "lack", "lacy", "lads", "lady", "lags", "laid", "lair",
    "lake", "lamb", "lame", "lamp", "land", "lane", "laps", "lard", "lark", "last",
    "late", "lava", "lawn", "laws", "lays", "lazy", "lead", "leaf", "leak", "lean",
    "leap", "left", "legs", "lend", "lens", "lent", "less", "liar", "lice", "lick",
    "lids", "lien", "lies", "life", "lift", "like", "limb", "lime", "limp", "line",
    "link", "lint", "lion", "lips", "list", "lite", "live", "load", "loaf", "loan",
    "lobe", "lobs", "lock", "loco", "loft", "logo", "logs", "lone", "long", "look",
    "loom", "loop", "loot", "lord", "lore", "lose", "loss", "lost", "lots", "loud",
    "lout", "love", "luck", "lull", "lump", "lung", "lure", "lurk", "lush", "lynx",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_word_lists_from_toml() {
        let toml = r#"
        adjectives = ["cold", "blue", "warm"]
        nouns = ["lamp", "frog", "desk"]
        "#;
        let words = WordLists::from_toml(toml).unwrap();
        assert_eq!(words.adjectives, vec!["cold", "blue", "warm"]);
        assert_eq!(words.nouns, vec!["lamp", "frog", "desk"]);
    }

    #[test]
    fn test_word_lists_embedded() {
        let words = WordLists::embedded();
        assert!(!words.adjectives.is_empty());
        assert!(!words.nouns.is_empty());
        assert!(words.adjectives.len() >= 100);
        assert!(words.nouns.len() >= 100);
    }

    #[test]
    fn test_generate_id() {
        let words = WordLists::embedded();
        let id = words.generate_id().unwrap();
        assert!(id.contains('-'));
        let parts: Vec<_> = id.split('-').collect();
        assert_eq!(parts.len(), 2);
        assert!(words.adjectives.contains(&parts[0].to_string()));
        assert!(words.nouns.contains(&parts[1].to_string()));
    }

    #[test]
    fn test_generate_ids_unique() {
        let words = WordLists::embedded();
        let ids = words.generate_ids(100);
        assert_eq!(ids.len(), 100);

        // All should be unique
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), 100);
    }

    #[test]
    fn test_combination_count() {
        let words = WordLists {
            adjectives: vec!["cold".into(), "blue".into()],
            nouns: vec!["lamp".into(), "frog".into()],
        };
        assert_eq!(words.combination_count(), 4); // 2 * 2, no overlap

        let words_with_overlap = WordLists {
            adjectives: vec!["cold".into(), "blue".into()],
            nouns: vec!["cold".into(), "frog".into()], // "cold" overlaps
        };
        assert_eq!(words_with_overlap.combination_count(), 3); // 2 * 2 - 1 overlap
    }

    #[test]
    fn test_no_same_word_ids() {
        let words = WordLists {
            adjectives: vec!["cold".into()],
            nouns: vec!["cold".into(), "lamp".into()],
        };

        // Generate many IDs and ensure none are "cold-cold"
        for _ in 0..100 {
            let id = words.generate_id().unwrap();
            assert_ne!(id, "cold-cold");
        }
    }

    #[test]
    fn test_empty_lists_error() {
        let result = WordLists::from_toml("adjectives = []\nnouns = []");
        assert!(result.is_err());
    }
}
