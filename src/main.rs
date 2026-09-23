use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
};

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use pinyin::ToPinyin;
use serde::{Deserialize, Serialize};

const RADICALS: &str = include_str!("../data/radical.yaml");
const COMPONENT_VARIANTS: &str = include_str!("../data/component_variants.txt");
const JP_VARIANTS: &str = include_str!("../data/jp_variants.txt");
const CHAR_VARIANTS: &str = include_str!("../data/char_variants.txt");
const IDS_STRUCT: &str = include_str!("../data/ids_struct.txt");
const STROKES: &str = include_str!("../data/stoke.dat");
const WORDS: &str = include_str!("../data/zuci.txt");
const MAX_INPUT_CHARS: usize = 8;
const MAX_LOOKUP_CHARS: usize = 204;
const MAX_RESULTS: usize = 100;
const MAX_VARIANTS: usize = 20;

#[derive(Clone, Debug)]
struct Entry {
    word: String,
    parts: Vec<String>,
    strokes_after_radical: u32,
}

#[derive(Clone, Debug, Default)]
struct Structure {
    name: String,
    parts: Vec<String>,
}

#[derive(Clone, Default)]
struct Dictionary {
    entries: Vec<Entry>,
    exact: HashMap<String, Vec<usize>>,
    by_part: HashMap<String, Vec<usize>>,
    by_word: HashMap<String, usize>,
    component_variants: HashMap<String, Vec<String>>,
    char_variants: HashMap<String, Vec<String>>,
    structures: HashMap<String, Structure>,
    strokes: HashMap<String, u32>,
    phrases: Vec<String>,
    grades: HashMap<&'static str, &'static str>,
}

#[derive(Deserialize)]
struct SearchQuery {
    parts: Option<String>,
}

#[derive(Deserialize)]
struct TextQuery {
    text: Option<String>,
}

#[derive(Serialize, Debug, PartialEq)]
struct Word {
    word: String,
    pinyin: String,
    codepoint: String,
    parts: Vec<String>,
    structure: Option<String>,
    simple_parts: Vec<String>,
    glyph_url: Option<String>,
}

#[derive(Serialize)]
struct SearchResponse {
    parts: String,
    exact: Vec<Word>,
    partial: Vec<Word>,
    variants: Vec<Word>,
    character_variants: Vec<Word>,
    single_word: Option<Word>,
}

#[derive(Serialize)]
struct CharacterWord {
    word: String,
    bushou: String,
    exclude_bushou: String,
    exclude_bushou_stroke: u32,
    pinyin: String,
    zuci: Vec<String>,
}

#[derive(Serialize)]
struct CharacterResponse {
    text: String,
    words: Vec<CharacterWord>,
}

#[derive(Serialize)]
struct GradeResponse {
    grade: String,
    text: String,
    words: Vec<CharacterWord>,
}

#[derive(Serialize)]
struct PinyinCharacter {
    character: String,
    pinyin: String,
}

#[derive(Serialize)]
struct PinyinResponse {
    text: String,
    truncated: bool,
    characters: Vec<PinyinCharacter>,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
}

#[tokio::main]
async fn main() {
    let dictionary = Arc::new(Dictionary::load());
    let app = Router::new()
        .route("/healthz", get(health))
        .route("/v1/zuzi", get(search))
        .route("/v1/hanzi", get(lookup_characters))
        .route("/v1/grades/{grade}", get(lookup_grade))
        .route("/v1/pinyin", get(lookup_pinyin))
        .with_state(dictionary)
        .layer(axum::middleware::from_fn(cors));

    let port = std::env::var("PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(8080);
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("bind HTTP listener");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("serve HTTP requests");
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn health() -> &'static str {
    "ok"
}

async fn search(
    State(dictionary): State<Arc<Dictionary>>,
    Query(query): Query<SearchQuery>,
) -> Response {
    let Some(raw) = query.parts else {
        return api_error(StatusCode::BAD_REQUEST, "the parts query parameter is required");
    };
    let parts = parse_codepoint(raw.trim()).unwrap_or_else(|| raw.trim().to_owned());
    if parts.is_empty() || parts.chars().count() > MAX_INPUT_CHARS {
        return api_error(StatusCode::BAD_REQUEST, "parts must contain 1 to 8 supported Han characters");
    }
    if !dictionary.supports(&parts) {
        return api_error(StatusCode::BAD_REQUEST, "parts contains an unsupported component");
    }

    let (exact, partial) = dictionary.search(&parts);
    let exact_words = dictionary.words_for(exact.iter().copied());
    let partial_words = dictionary.words_for(partial.iter().copied());
    let variants = dictionary.exact_variants(&exact, &partial);
    let character_variants = if parts.chars().count() == 1 {
        dictionary.character_variants(&parts)
    } else {
        Vec::new()
    };
    let single_word = (parts.chars().count() == 1).then(|| dictionary.word_for_char(&parts));

    let mut response = Json(SearchResponse {
        parts,
        exact: exact_words,
        partial: partial_words,
        variants,
        character_variants,
        single_word,
    })
    .into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=300, s-maxage=86400"));
    response
}

async fn lookup_characters(
    State(dictionary): State<Arc<Dictionary>>,
    Query(query): Query<TextQuery>,
) -> Response {
    let Some(text) = query.text else {
        return api_error(StatusCode::BAD_REQUEST, "the text query parameter is required");
    };
    let text = han_characters(&text);
    if text.is_empty() || text.chars().count() > MAX_LOOKUP_CHARS {
        return api_error(StatusCode::BAD_REQUEST, "text must contain 1 to 204 Han characters");
    }
    cached_json(CharacterResponse { words: dictionary.character_words(&text), text })
}

async fn lookup_grade(
    State(dictionary): State<Arc<Dictionary>>,
    Path(grade): Path<String>,
) -> Response {
    let Some(source) = dictionary.grades.get(grade.as_str()) else {
        return api_error(StatusCode::NOT_FOUND, "unknown grade");
    };
    let text = han_characters(source);
    cached_json(GradeResponse {
        grade,
        words: dictionary.character_words(&text),
        text,
    })
}

async fn lookup_pinyin(Query(query): Query<TextQuery>) -> Response {
    let Some(raw) = query.text else {
        return api_error(StatusCode::BAD_REQUEST, "the text query parameter is required");
    };
    let truncated = raw.chars().count() > MAX_LOOKUP_CHARS;
    let text: String = raw.chars().take(MAX_LOOKUP_CHARS).collect();
    cached_json(PinyinResponse {
        characters: text.chars().map(|character| PinyinCharacter {
            character: character.to_string(),
            pinyin: character.to_pinyin().map(|value| value.with_tone().to_owned()).unwrap_or_default(),
        }).collect(),
        text,
        truncated,
    })
}

fn cached_json<T: Serialize>(value: T) -> Response {
    let mut response = Json(value).into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=300, s-maxage=86400"));
    response
}

fn api_error(status: StatusCode, error: &'static str) -> Response {
    (status, Json(ErrorResponse { error })).into_response()
}

// The API is public, but browser access is deliberately limited to the site
// that consumes it. Non-browser callers can still use ordinary GET requests.
async fn cors(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    if request.method() == Method::OPTIONS {
        return StatusCode::NO_CONTENT.into_response();
    }
    let origin = request.headers().get(header::ORIGIN).cloned();
    let mut response = next.run(request).await;
    if let Some(origin) = origin.filter(allowed_origin) {
        response.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        response.headers_mut().insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    response
}

fn allowed_origin(origin: &HeaderValue) -> bool {
    matches!(origin.to_str(), Ok("https://www.cyeam.com") | Ok("https://cyeam.com"))
}

impl Dictionary {
    fn load() -> Self {
        let mut dictionary = Self::default();
        dictionary.strokes = STROKES.lines().filter_map(|line| {
            let mut fields = line.split('|');
            let _codepoint = fields.next()?;
            let character = fields.next()?;
            let strokes = fields.next()?.parse().ok()?;
            Some((character.to_owned(), strokes))
        }).collect();
        for line in RADICALS.lines() {
            let mut fields = line.split('\t');
            let Some(word) = fields.next().filter(|value| !value.is_empty()) else {
                continue;
            };
            let Some(parts) = fields.next() else { continue };
            let parts: Vec<String> = parts.split_whitespace().map(str::to_owned).collect();
            if parts.is_empty() {
                continue;
            }
            let index = dictionary.entries.len();
            let key = component_key(&parts);
            dictionary.exact.entry(key).or_default().push(index);
            for (position, part) in parts.iter().enumerate() {
                if !parts[..position].contains(part) {
                    dictionary.by_part.entry(part.clone()).or_default().push(index);
                }
            }
            dictionary.by_word.entry(word.to_owned()).or_insert(index);
            let strokes_after_radical = parts.iter().skip(1).filter_map(|part| dictionary.strokes.get(part)).sum();
            dictionary.entries.push(Entry { word: word.to_owned(), parts, strokes_after_radical });
        }
        dictionary.component_variants = variant_map(&[JP_VARIANTS, COMPONENT_VARIANTS]);
        dictionary.char_variants = variant_map(&[CHAR_VARIANTS]);
        dictionary.structures = structure_map();
        dictionary.phrases = WORDS.lines().map(str::trim).filter(|word| !word.is_empty()).map(str::to_owned).collect();
        dictionary.grades = grade_map();
        dictionary
    }

    fn supports(&self, parts: &str) -> bool {
        parts.chars().all(|character| {
            let part = character.to_string();
            self.by_part.contains_key(&part)
                || self.component_candidates(&part).iter().any(|candidate| self.by_part.contains_key(candidate))
        })
    }

    fn component_candidates(&self, part: &str) -> Vec<String> {
        let mut candidates = vec![part.to_owned()];
        for variants in [self.component_variants.get(part), self.char_variants.get(part)] {
            if let Some(variants) = variants {
                for value in variants {
                    if !candidates.contains(value) {
                        candidates.push(value.clone());
                    }
                }
            }
        }
        candidates
    }

    fn search(&self, parts: &str) -> (Vec<usize>, Vec<usize>) {
        let candidates: Vec<Vec<String>> = parts
            .chars()
            .map(|character| self.component_candidates(&character.to_string()))
            .collect();
        let mut exact = Vec::new();
        let mut seen = HashSet::new();
        self.exact_combinations(&candidates, 0, &mut Vec::new(), &mut seen, &mut exact);

        let mut smallest: Option<HashSet<usize>> = None;
        for candidates in &candidates {
            let indexes: HashSet<usize> = candidates
                .iter()
                .flat_map(|candidate| self.by_part.get(candidate).into_iter().flatten().copied())
                .collect();
            if indexes.is_empty() {
                return (exact, Vec::new());
            }
            if smallest.as_ref().is_none_or(|current| indexes.len() < current.len()) {
                smallest = Some(indexes);
            }
        }
        let mut partial: Vec<usize> = smallest
            .unwrap_or_default()
            .into_iter()
            .filter(|index| !seen.contains(&self.entries[*index].word))
            .filter(|index| candidates.iter().all(|choices| choices.iter().any(|choice| self.entries[*index].parts.contains(choice))))
            .collect();
        partial.sort_by_key(|index| self.entries[*index].parts.len());
        let mut partial_seen = HashSet::new();
        partial.retain(|index| partial_seen.insert(self.entries[*index].word.clone()));
        partial.truncate(MAX_RESULTS);
        (exact, partial)
    }

    fn exact_combinations(
        &self,
        candidates: &[Vec<String>],
        offset: usize,
        selected: &mut Vec<String>,
        seen: &mut HashSet<String>,
        output: &mut Vec<usize>,
    ) {
        if output.len() >= MAX_RESULTS { return; }
        if offset == candidates.len() {
            if let Some(indexes) = self.exact.get(&component_key(selected)) {
                for index in indexes {
                    if seen.insert(self.entries[*index].word.clone()) {
                        output.push(*index);
                        if output.len() >= MAX_RESULTS { break; }
                    }
                }
            }
            return;
        }
        for candidate in &candidates[offset] {
            selected.push(candidate.clone());
            self.exact_combinations(candidates, offset + 1, selected, seen, output);
            selected.pop();
            if output.len() >= MAX_RESULTS { break; }
        }
    }

    fn words_for(&self, indexes: impl Iterator<Item = usize>) -> Vec<Word> {
        indexes.map(|index| self.word_from_entry(&self.entries[index])).collect()
    }

    fn word_for_char(&self, word: &str) -> Word {
        self.by_word.get(word)
            .map(|index| self.word_from_entry(&self.entries[*index]))
            .unwrap_or_else(|| self.word_without_parts(word))
    }

    fn character_variants(&self, word: &str) -> Vec<Word> {
        self.char_variants.get(word).into_iter().flatten().take(MAX_VARIANTS).map(|variant| self.word_for_char(variant)).collect()
    }

    fn exact_variants(&self, exact: &[usize], partial: &[usize]) -> Vec<Word> {
        let mut seen: HashSet<&str> = exact.iter().chain(partial).map(|index| self.entries[*index].word.as_str()).collect();
        let mut results = Vec::new();
        for index in exact {
            if let Some(variants) = self.char_variants.get(&self.entries[*index].word) {
                for variant in variants {
                    if seen.insert(variant) {
                        results.push(self.word_for_char(variant));
                        if results.len() == MAX_VARIANTS { return results; }
                    }
                }
            }
        }
        results
    }

    fn word_from_entry(&self, entry: &Entry) -> Word {
        let mut word = self.word_without_parts(&entry.word);
        word.parts = entry.parts.clone();
        if let Some(structure) = self.structures.get(&entry.word) {
            if structure.parts != word.parts {
                word.structure = Some(structure.name.clone());
                word.simple_parts = structure.parts.clone();
            }
        }
        word
    }

    fn word_without_parts(&self, word: &str) -> Word {
        let character = word.chars().next();
        let codepoint = character.map(|value| format!("U+{:04X}", value as u32)).unwrap_or_default();
        let glyph_url = character.filter(|value| *value > '\u{FFFF}').map(|value| format!("https://glyphwiki.org/glyph/u{:x}.svg", value as u32));
        let pinyin = character.and_then(|value| value.to_pinyin()).map(|value| value.with_tone().to_string()).unwrap_or_default();
        Word { word: word.to_owned(), pinyin, codepoint, parts: Vec::new(), structure: None, simple_parts: Vec::new(), glyph_url }
    }

    fn character_words(&self, text: &str) -> Vec<CharacterWord> {
        text.chars().map(|character| self.character_word(&character.to_string())).collect()
    }

    fn character_word(&self, character: &str) -> CharacterWord {
        let pinyin = character.chars().next().and_then(|value| value.to_pinyin()).map(|value| value.with_tone().to_owned()).unwrap_or_default();
        let Some(index) = self.by_word.get(character) else {
            return CharacterWord { word: character.to_owned(), bushou: String::new(), exclude_bushou: String::new(), exclude_bushou_stroke: 0, pinyin, zuci: self.phrases_for(character) };
        };
        let entry = &self.entries[*index];
        CharacterWord {
            word: entry.word.clone(),
            bushou: entry.parts.first().cloned().unwrap_or_default(),
            exclude_bushou: entry.parts.iter().skip(1).cloned().collect::<Vec<_>>().join(" "),
            exclude_bushou_stroke: entry.strokes_after_radical,
            pinyin,
            zuci: self.phrases_for(character),
        }
    }

    fn phrases_for(&self, character: &str) -> Vec<String> {
        let mut results = Vec::with_capacity(2);
        for phrase in self.phrases.iter().filter(|phrase| phrase.starts_with(character)).chain(self.phrases.iter().filter(|phrase| phrase.contains(character))) {
            if !results.contains(phrase) {
                results.push(phrase.clone());
                if results.len() == 2 { break; }
            }
        }
        while results.len() < 2 { results.push(String::new()); }
        results
    }
}

fn han_characters(text: &str) -> String {
    text.chars().filter(|character| is_han(*character)).collect()
}

fn is_han(character: char) -> bool {
    matches!(character as u32,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF |
        0x20000..=0x2EBEF | 0x30000..=0x323AF
    )
}

fn grade_map() -> HashMap<&'static str, &'static str> {
    HashMap::from([
        ("onegrade1st", include_str!("../data/chaizi/onegrade1st.txt")),
        ("onegrade2nd", include_str!("../data/chaizi/onegrade2nd.txt")),
        ("twograde1st", include_str!("../data/chaizi/twograde1st.txt")),
        ("twograde2nd", include_str!("../data/chaizi/twograde2nd.txt")),
        ("threegrade1st", include_str!("../data/chaizi/threegrade1st.txt")),
        ("threegrade2nd", include_str!("../data/chaizi/threegrade2nd.txt")),
        ("fourgrade1st", include_str!("../data/chaizi/fourgrade1st.txt")),
        ("fourgrade2nd", include_str!("../data/chaizi/fourgrade2nd.txt")),
        ("fivegrade1st", include_str!("../data/chaizi/fivegrade1st.txt")),
        ("fivegrade2nd", include_str!("../data/chaizi/fivegrade2nd.txt")),
        ("sixgrade1st", include_str!("../data/chaizi/sixgrade1st.txt")),
        ("sixgrade2nd", include_str!("../data/chaizi/sixgrade2nd.txt")),
    ])
}

fn variant_map(sources: &[&str]) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for source in sources {
        for line in source.lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 2 { continue; }
            for target in &fields[1..] {
                add_variant(&mut map, fields[0], target);
                add_variant(&mut map, target, fields[0]);
            }
        }
    }
    map
}

fn add_variant(map: &mut HashMap<String, Vec<String>>, from: &str, to: &str) {
    let variants = map.entry(from.to_owned()).or_default();
    if !variants.iter().any(|value| value == to) { variants.push(to.to_owned()); }
}

fn structure_map() -> HashMap<String, Structure> {
    let names = HashMap::from([
        ("⿰", "左右结构"), ("⿱", "上下结构"), ("⿲", "左中右结构"), ("⿳", "上中下结构"),
        ("⿴", "全包围结构"), ("⿵", "半包围结构"), ("⿶", "半包围结构"), ("⿷", "半包围结构"),
        ("⿸", "半包围结构"), ("⿹", "半包围结构"), ("⿺", "半包围结构"), ("⿻", "镶嵌结构"),
    ]);
    IDS_STRUCT.lines().filter_map(|line| {
        let fields: Vec<&str> = line.splitn(3, '\t').collect();
        (fields.len() == 3).then(|| (fields[0].to_owned(), Structure { name: names.get(fields[1]).unwrap_or(&"").to_string(), parts: fields[2].split_whitespace().map(str::to_owned).collect() }))
    }).collect()
}

fn component_key(parts: &[String]) -> String {
    let mut sorted = parts.to_vec();
    sorted.sort_unstable();
    sorted.join(" ")
}

fn parse_codepoint(input: &str) -> Option<String> {
    let hex = input.strip_prefix("U+").or_else(|| input.strip_prefix("u+"))?;
    (4..=6).contains(&hex.len()).then_some(())?;
    u32::from_str_radix(hex, 16).ok().and_then(char::from_u32).map(|value| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_exact_and_simplified_component_matches() {
        let dictionary = Dictionary::load();
        let (exact, _) = dictionary.search("豐去皿");
        assert!(exact.iter().any(|index| dictionary.entries[*index].word == "豔"));
        let (exact, _) = dictionary.search("豊去皿");
        assert!(exact.iter().any(|index| dictionary.entries[*index].word == "豔"));
    }

    #[test]
    fn parses_valid_codepoints() {
        assert_eq!(parse_codepoint("U+263F6"), Some("𦏶".to_owned()));
        assert_eq!(parse_codepoint("U+D800"), None);
    }
}
