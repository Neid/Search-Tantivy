//! Language-aware text analyzers.
//!
//! Tokenizers are not persisted in a tantivy index, only their names in the
//! schema, so every index we create or open gets all of ours registered
//! under stable names:
//!
//! - `st_<code>`      lowercase + stemming for a language
//! - `st_<code>_fold` the same plus ASCII folding ("Żółw" ~ "zolw")
//! - `st_<code>_stop`, `st_<code>_stop_fold`: also drop the language's stop
//!   words ("the", "and", ...) before stemming
//! - `st_fold`        lowercase + ASCII folding, no stemming

use std::borrow::Cow;
use std::mem;

use tantivy::tokenizer::{
    AsciiFoldingFilter, Language, LowerCaser, RemoveLongFilter, SimpleTokenizer, Stemmer,
    StopWordFilter, TextAnalyzer, Token, TokenFilter, TokenStream, Tokenizer, TokenizerManager,
};

/// Tokens longer than this are dropped, as in tantivy's default analyzer.
const MAX_TOKEN_LEN: usize = 40;

/// How a language is stemmed.
#[derive(Clone, Copy)]
enum Stem {
    /// Snowball stemmer bundled with tantivy.
    Snowball(Language),
    /// Polish (Yarovoy) from tantivy-stemmers; it has a variant made for
    /// text without diacritics, used when folding.
    Polish,
}

/// (ISO 639-1 code, English name, stemmer)
const LANGUAGES: &[(&str, &str, Stem)] = &[
    ("ar", "arabic", Stem::Snowball(Language::Arabic)),
    ("da", "danish", Stem::Snowball(Language::Danish)),
    ("de", "german", Stem::Snowball(Language::German)),
    ("el", "greek", Stem::Snowball(Language::Greek)),
    ("en", "english", Stem::Snowball(Language::English)),
    ("es", "spanish", Stem::Snowball(Language::Spanish)),
    ("fi", "finnish", Stem::Snowball(Language::Finnish)),
    ("fr", "french", Stem::Snowball(Language::French)),
    ("hu", "hungarian", Stem::Snowball(Language::Hungarian)),
    ("it", "italian", Stem::Snowball(Language::Italian)),
    ("nl", "dutch", Stem::Snowball(Language::Dutch)),
    ("no", "norwegian", Stem::Snowball(Language::Norwegian)),
    ("pl", "polish", Stem::Polish),
    ("pt", "portuguese", Stem::Snowball(Language::Portuguese)),
    ("ro", "romanian", Stem::Snowball(Language::Romanian)),
    ("ru", "russian", Stem::Snowball(Language::Russian)),
    ("sv", "swedish", Stem::Snowball(Language::Swedish)),
    ("ta", "tamil", Stem::Snowball(Language::Tamil)),
    ("tr", "turkish", Stem::Snowball(Language::Turkish)),
];

/// Frequent Polish function words (tantivy has no Polish list). Folded
/// spellings are added in `stop_words`, so "sie" goes as well as "się".
const POLISH_STOP_WORDS: &[&str] = &[
    "a", "aby", "ale", "albo", "ani", "bo", "być", "był", "była", "było", "były", "by", "co",
    "czy", "dla", "do", "gdy", "gdzie", "go", "i", "ich", "im", "jak", "jako", "je", "jego", "jej",
    "jest", "jeszcze", "jeśli", "już", "ku", "lub", "ma", "mi", "mnie", "na", "nad", "nam", "nas",
    "nie", "nim", "niż", "o", "od", "on", "ona", "one", "oni", "ono", "oraz", "po", "pod", "przez",
    "przy", "się", "są", "ta", "tak", "także", "te", "tego", "tej", "ten", "to", "tu", "tym", "u",
    "w", "we", "więc", "z", "za", "ze", "że", "żeby",
];

fn stop_words(stem: Stem) -> Option<StopWordFilter> {
    match stem {
        Stem::Snowball(l) => StopWordFilter::new(l),
        Stem::Polish => {
            let fold = |w: &str| -> String {
                w.chars()
                    .map(|c| match c {
                        'ą' => 'a',
                        'ć' => 'c',
                        'ę' => 'e',
                        'ł' => 'l',
                        'ń' => 'n',
                        'ó' => 'o',
                        'ś' => 's',
                        'ź' | 'ż' => 'z',
                        c => c,
                    })
                    .collect()
            };
            let words = POLISH_STOP_WORDS
                .iter()
                .flat_map(|w| [w.to_string(), fold(w)]);
            Some(StopWordFilter::remove(words))
        }
    }
}

/// Whether `stop_words` is available for a language code.
pub fn has_stop_words(code: &str) -> bool {
    LANGUAGES
        .iter()
        .any(|(c, _, stem)| *c == code && stop_words(*stem).is_some())
}

/// Canonical code for a language given as code or English name (any case).
pub fn language_code(input: &str) -> Option<&'static str> {
    let input = input.trim().to_ascii_lowercase();
    LANGUAGES
        .iter()
        .find(|(code, name, _)| *code == input || *name == input)
        .map(|(code, _, _)| *code)
}

/// Comma-separated list for error messages.
pub fn supported_languages() -> String {
    LANGUAGES
        .iter()
        .map(|(code, name, _)| format!("{code} ({name})"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Tokenizer name for a text field, or `None` for tantivy's default.
/// `stop` needs a language (callers check `has_stop_words`).
pub fn tokenizer_name(code: Option<&str>, fold: bool, stop: bool) -> Option<String> {
    let stop = if stop { "_stop" } else { "" };
    match (code, fold) {
        (None, false) => None,
        (None, true) => Some("st_fold".into()),
        (Some(c), false) => Some(format!("st_{c}{stop}")),
        (Some(c), true) => Some(format!("st_{c}{stop}_fold")),
    }
}

fn analyzer(stem: Option<Stem>, fold: bool, stop: Option<StopWordFilter>) -> TextAnalyzer {
    let mut b = TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(MAX_TOKEN_LEN))
        .filter(LowerCaser)
        .dynamic();
    // Before stemming: the lists hold whole words.
    if let Some(stop) = stop {
        b = b.filter_dynamic(stop);
    }
    match (stem, fold) {
        (None, false) => b,
        (None, true) => b.filter_dynamic(AsciiFoldingFilter),
        // Fold first, then the stemmer written for unaccented Polish, so
        // "żółwie" and "zolwie" end up as the same term.
        (Some(Stem::Polish), true) => {
            b.filter_dynamic(AsciiFoldingFilter)
                .filter_dynamic(FnStemmer(
                    tantivy_stemmers::algorithms::polish_yarovoy_unaccented,
                ))
        }
        (Some(Stem::Polish), false) => {
            b.filter_dynamic(FnStemmer(tantivy_stemmers::algorithms::polish_yarovoy))
        }
        // Snowball stemmers expect real spelling, so stem before folding.
        (Some(Stem::Snowball(l)), false) => b.filter_dynamic(Stemmer::new(l)),
        (Some(Stem::Snowball(l)), true) => b
            .filter_dynamic(Stemmer::new(l))
            .filter_dynamic(AsciiFoldingFilter),
    }
    .build()
}

/// Register every analyzer this library can put in a schema.
pub fn register_all(manager: &TokenizerManager) {
    manager.register("st_fold", analyzer(None, true, None));
    for (code, _, stem) in LANGUAGES {
        for fold in [false, true] {
            let name = tokenizer_name(Some(code), fold, false).expect("language given");
            manager.register(&name, analyzer(Some(*stem), fold, None));
            if let Some(stop) = stop_words(*stem) {
                let name = tokenizer_name(Some(code), fold, true).expect("language given");
                manager.register(&name, analyzer(Some(*stem), fold, Some(stop)));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// A token filter around a plain `fn(&str) -> Cow<str>` stemmer
// (tantivy-stemmers' own tokenizer types target an older tantivy API).
// ---------------------------------------------------------------------------

type StemFn = for<'a> fn(&'a str) -> Cow<'a, str>;

#[derive(Clone, Copy)]
struct FnStemmer(StemFn);

impl TokenFilter for FnStemmer {
    type Tokenizer<T: Tokenizer> = FnStemmerFilter<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> FnStemmerFilter<T> {
        FnStemmerFilter {
            stem: self.0,
            inner: tokenizer,
        }
    }
}

#[derive(Clone)]
struct FnStemmerFilter<T> {
    stem: StemFn,
    inner: T,
}

impl<T: Tokenizer> Tokenizer for FnStemmerFilter<T> {
    type TokenStream<'a> = FnStemmerStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        FnStemmerStream {
            tail: self.inner.token_stream(text),
            stem: self.stem,
            buffer: String::new(),
        }
    }
}

struct FnStemmerStream<T> {
    tail: T,
    stem: StemFn,
    buffer: String,
}

impl<T: TokenStream> TokenStream for FnStemmerStream<T> {
    fn advance(&mut self) -> bool {
        if !self.tail.advance() {
            return false;
        }
        let token = self.tail.token_mut();
        match (self.stem)(&token.text) {
            Cow::Owned(s) => token.text = s,
            Cow::Borrowed(s) => {
                self.buffer.clear();
                self.buffer.push_str(s);
                mem::swap(&mut token.text, &mut self.buffer);
            }
        }
        true
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(code: Option<&str>, fold: bool, text: &str) -> Vec<String> {
        terms_stop(code, fold, false, text)
    }

    fn terms_stop(code: Option<&str>, fold: bool, stop: bool, text: &str) -> Vec<String> {
        let m = TokenizerManager::default();
        register_all(&m);
        let mut a = match tokenizer_name(code, fold, stop) {
            Some(n) => m.get(&n).expect("registered"),
            None => m.get("default").expect("default"),
        };
        let mut out = Vec::new();
        a.token_stream(text)
            .process(&mut |t: &Token| out.push(t.text.clone()));
        out
    }

    #[test]
    fn english_stems() {
        assert_eq!(terms(Some("en"), false, "Running dogs"), ["run", "dog"]);
    }

    #[test]
    fn polish_inflections_share_a_stem() {
        let a = terms(Some("pl"), false, "książka");
        let b = terms(Some("pl"), false, "książki");
        assert_eq!(a, b);
    }

    #[test]
    fn polish_fold_matches_text_without_diacritics() {
        assert_eq!(
            terms(Some("pl"), true, "Żółwie"),
            terms(Some("pl"), true, "zolwie")
        );
    }

    #[test]
    fn fold_only() {
        assert_eq!(terms(None, true, "Crème Brûlée"), ["creme", "brulee"]);
    }

    #[test]
    fn stop_words_removed_before_stemming() {
        assert_eq!(
            terms_stop(Some("en"), false, true, "The running of the dogs"),
            ["run", "dog"]
        );
        assert_eq!(
            terms_stop(Some("pl"), true, true, "Kot się nie boi psa, ale sie boi"),
            terms_stop(Some("pl"), true, true, "kot boi psa boi")
        );
    }

    #[test]
    fn stop_word_lists() {
        assert!(has_stop_words("en"));
        assert!(has_stop_words("pl"));
        assert!(!has_stop_words("ta"));
    }

    #[test]
    fn codes_and_names() {
        assert_eq!(language_code("Polish"), Some("pl"));
        assert_eq!(language_code("PL"), Some("pl"));
        assert_eq!(language_code("klingon"), None);
    }
}
