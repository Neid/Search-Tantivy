//! C ABI over tantivy, consumed by the Perl module Search::Tantivy via FFI::Platypus.
//!
//! Conventions:
//! - Every exported function catches panics; nothing unwinds across the FFI boundary.
//! - On failure a function returns NULL / -1 and stores a message readable with
//!   `st_last_error()` (thread-local, valid until the next call on that thread).
//! - Strings returned to the caller are owned by Rust and must be released with
//!   `st_string_free()`.
//! - Data crosses the boundary as JSON (schema, documents, search results).
//!
//! # Safety
//! Every `unsafe extern "C"` function expects string arguments that are NULL
//! or valid NUL-terminated strings, and index handles that are NULL or were
//! returned by `st_index_create` / `st_index_open` and not yet freed. Handles
//! must not be used from several threads at once.
//!
//! # fork()
//! A forked child inherits the memory of an index writer but not its
//! threads. The handle remembers which process opened the writer; any other
//! process gets an error when it tries to write, and freeing the handle
//! there leaks the writer instead of joining threads that do not exist
//! (which would hang) or releasing the parent's write lock. Searching
//! works in both processes.

#![allow(clippy::missing_safety_doc)] // covered by the crate-level Safety section

use std::any::Any;
use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::mem;
use std::process;

use tantivy::collector::{Collector, Count, TopDocs};
use tantivy::query::{
    BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, Query, QueryParser, TermQuery,
};
use tantivy::schema::document::Document as _;
use tantivy::schema::{
    DateOptions, FAST, Field, FieldType, IndexRecordOption, NumericOptions, STORED, STRING, Schema,
    TEXT, TextFieldIndexing, TextOptions,
};
use tantivy::snippet::SnippetGenerator;
use tantivy::{
    DateTime, DocAddress, Index, IndexReader, IndexWriter, Order, ReloadPolicy, Searcher,
    TantivyDocument, Term,
};

mod analyzers;

const WRITER_HEAP_BYTES: usize = 50_000_000;

/// At most this many index terms stand in for one misspelled query term.
const MAX_FUZZY_VARIANTS: usize = 50;
/// Score weight of a fuzzy variant relative to the term as typed.
const FUZZY_VARIANT_BOOST: f32 = 0.5;

type Res<T> = Result<T, String>;

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_error(msg: impl Into<String>) {
    let msg = msg.into().replace('\0', " ");
    let c = CString::new(msg).expect("NULs removed");
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(c));
}

fn panic_message(p: &(dyn Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Run `f`, converting both `Err` and panics into `fallback` + last error.
fn guard<T>(fallback: T, f: impl FnOnce() -> Res<T>) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            set_error(e);
            fallback
        }
        Err(p) => {
            set_error(format!("internal panic: {}", panic_message(&*p)));
            fallback
        }
    }
}

/// Borrow a C string; NULL or an empty string yields `None`.
unsafe fn opt_str<'a>(p: *const c_char) -> Res<Option<&'a str>> {
    if p.is_null() {
        return Ok(None);
    }
    let s = unsafe { CStr::from_ptr(p) }
        .to_str()
        .map_err(|e| format!("string is not valid UTF-8: {e}"))?;
    Ok(if s.is_empty() { None } else { Some(s) })
}

unsafe fn req_str<'a>(p: *const c_char, what: &str) -> Res<&'a str> {
    unsafe { opt_str(p) }?.ok_or_else(|| format!("{what} is required"))
}

fn into_c_string(s: String) -> Res<*mut c_char> {
    CString::new(s)
        .map(CString::into_raw)
        .map_err(|e| format!("result contains NUL byte: {e}"))
}

fn err<E: std::fmt::Display>(ctx: &'static str) -> impl FnOnce(E) -> String {
    move |e| format!("{ctx}: {e}")
}

// ---------------------------------------------------------------------------
// Index handle
// ---------------------------------------------------------------------------

pub struct StIndex {
    index: Index,
    reader: IndexReader,
    /// The writer and the id of the process that opened it.
    writer: Option<(IndexWriter, u32)>,
}

impl StIndex {
    fn new(index: Index) -> Res<Self> {
        analyzers::register_all(index.tokenizers());
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(err("cannot open index reader"))?;
        Ok(Self {
            index,
            reader,
            writer: None,
        })
    }

    fn writer(&mut self) -> Res<&mut IndexWriter> {
        if let Some((_, pid)) = &self.writer
            && *pid != process::id()
        {
            return Err("this index was opened for writing before a fork(); \
                        writing is only possible in the process that opened it \
                        (open the index again in this process to write)"
                .into());
        }
        if self.writer.is_none() {
            let w = self
                .index
                .writer(WRITER_HEAP_BYTES)
                .map_err(err("cannot open index writer"))?;
            self.writer = Some((w, process::id()));
        }
        Ok(&mut self.writer.as_mut().expect("just set").0)
    }
}

unsafe fn handle<'a>(h: *mut StIndex) -> Res<&'a mut StIndex> {
    unsafe { h.as_mut() }.ok_or_else(|| "index handle is NULL".to_string())
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct FieldSpec {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default = "default_true")]
    stored: bool,
    /// Stemming language for `text` fields (ISO code or English name).
    #[serde(default)]
    language: Option<String>,
    /// Fold accented characters to ASCII in `text` fields.
    #[serde(default)]
    ascii_folding: bool,
    /// Drop the language's stop words from `text` fields.
    #[serde(default)]
    stop_words: bool,
}

fn default_true() -> bool {
    true
}

fn build_schema(json: &str) -> Res<Schema> {
    let specs: Vec<FieldSpec> = serde_json::from_str(json).map_err(err("invalid schema"))?;
    if specs.is_empty() {
        return Err("schema needs at least one field".into());
    }
    let mut b = Schema::builder();
    for s in specs {
        let numeric = || {
            let o = NumericOptions::default().set_indexed().set_fast();
            if s.stored { o.set_stored() } else { o }
        };
        if s.kind != "text" && (s.language.is_some() || s.ascii_folding || s.stop_words) {
            return Err(format!(
                "field '{}': language, ascii_folding and stop_words only apply to text fields",
                s.name
            ));
        }
        match s.kind.as_str() {
            "text" => {
                let code = match &s.language {
                    None => None,
                    Some(l) => Some(analyzers::language_code(l).ok_or_else(|| {
                        format!(
                            "field '{}': unsupported language '{l}' (supported: {})",
                            s.name,
                            analyzers::supported_languages()
                        )
                    })?),
                };
                if s.stop_words {
                    match code {
                        None => {
                            return Err(format!("field '{}': stop_words needs a language", s.name));
                        }
                        Some(c) if !analyzers::has_stop_words(c) => {
                            return Err(format!(
                                "field '{}': no stop word list for language '{c}'",
                                s.name
                            ));
                        }
                        Some(_) => {}
                    }
                }
                let mut o: TextOptions =
                    match analyzers::tokenizer_name(code, s.ascii_folding, s.stop_words) {
                        None => TEXT,
                        Some(tok) => TextOptions::default().set_indexing_options(
                            TextFieldIndexing::default()
                                .set_tokenizer(&tok)
                                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
                        ),
                    };
                if s.stored {
                    o = o.set_stored();
                }
                b.add_text_field(&s.name, o);
            }
            "string" => {
                // FAST: sortable (column storage next to the inverted index).
                let o: TextOptions = if s.stored {
                    STRING | STORED | FAST
                } else {
                    STRING | FAST
                };
                b.add_text_field(&s.name, o);
            }
            "date" => {
                let o = DateOptions::default().set_indexed().set_fast();
                b.add_date_field(&s.name, if s.stored { o.set_stored() } else { o });
            }
            "u64" => {
                b.add_u64_field(&s.name, numeric());
            }
            "i64" => {
                b.add_i64_field(&s.name, numeric());
            }
            "f64" => {
                b.add_f64_field(&s.name, numeric());
            }
            other => {
                return Err(format!(
                    "field '{}': unknown type '{other}' (expected text, string, u64, i64, f64 or date)",
                    s.name
                ));
            }
        }
    }
    Ok(b.build())
}

/// Dates as tantivy's JSON parser wants them (RFC 3339), from the forms Perl
/// code tends to have: epoch seconds, "2024-05-01", "2024-05-01 12:30:00"
/// (both taken as UTC), or RFC 3339 already.
fn normalize_date(v: &Value) -> Option<Value> {
    let rfc = |dt: DateTime| {
        let secs = dt.into_timestamp_secs();
        let odt = tantivy::time::OffsetDateTime::from_unix_timestamp(secs).ok()?;
        odt.format(&tantivy::time::format_description::well_known::Rfc3339)
            .ok()
            .map(Value::String)
    };
    match v {
        Value::Number(n) => rfc(DateTime::from_timestamp_secs(n.as_i64()?)),
        Value::String(s) => {
            let s = s.trim();
            if let Ok(secs) = s.parse::<i64>() {
                return rfc(DateTime::from_timestamp_secs(secs));
            }
            let b = s.as_bytes();
            let looks_like_date = b.len() >= 10
                && b[..10].iter().enumerate().all(|(i, c)| {
                    if i == 4 || i == 7 {
                        *c == b'-'
                    } else {
                        c.is_ascii_digit()
                    }
                });
            if !looks_like_date {
                return None;
            }
            match b.len() {
                10 => Some(Value::String(format!("{s}T00:00:00Z"))),
                19 if b[10] == b' ' || b[10] == b'T' => {
                    Some(Value::String(format!("{}T{}Z", &s[..10], &s[11..])))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Perl hands numbers over as strings whenever a scalar was last used as a
/// string; accept "1999" for numeric fields instead of failing. Dates get
/// normalized too.
fn coerce_numbers(schema: &Schema, doc: &mut Map<String, Value>) {
    fn coerce(v: &mut Value, ft: &FieldType) {
        if matches!(ft, FieldType::Date(_)) && !v.is_array() {
            if let Some(d) = normalize_date(v) {
                *v = d;
            }
            return;
        }
        match v {
            Value::Array(items) => items.iter_mut().for_each(|i| coerce(i, ft)),
            Value::String(s) => {
                let s = s.trim();
                let n = match ft {
                    FieldType::U64(_) => s.parse::<u64>().ok().map(Value::from),
                    FieldType::I64(_) => s.parse::<i64>().ok().map(Value::from),
                    FieldType::F64(_) => s.parse::<f64>().ok().map(Value::from),
                    _ => None,
                };
                if let Some(n) = n {
                    *v = n;
                }
            }
            _ => {}
        }
    }
    for (name, v) in doc.iter_mut() {
        if let Ok(f) = schema.get_field(name) {
            coerce(v, schema.get_field_entry(f).field_type());
        }
    }
}

/// tantivy returns every field as an array; give Perl a plain scalar when
/// there is exactly one value.
fn flatten_single_values(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .map(|(k, v)| match v {
                    Value::Array(mut a) if a.len() == 1 => (k, a.pop().expect("len 1")),
                    other => (k, other),
                })
                .collect(),
        ),
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Exported API
// ---------------------------------------------------------------------------

/// Last error message on this thread, or NULL. Owned by the library.
#[unsafe(no_mangle)]
pub extern "C" fn st_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ref().map_or(ptr::null(), |s| s.as_ptr()))
}

/// Create a new index. `path` NULL/"" creates an in-memory index;
/// otherwise the directory must exist and must not already hold an index.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn st_index_create(
    path: *const c_char,
    schema_json: *const c_char,
) -> *mut StIndex {
    guard(ptr::null_mut(), || {
        let schema = build_schema(unsafe { req_str(schema_json, "schema") }?)?;
        let index = match unsafe { opt_str(path) }? {
            None => Index::create_in_ram(schema),
            Some(p) => Index::create_in_dir(p, schema).map_err(err("cannot create index"))?,
        };
        Ok(Box::into_raw(Box::new(StIndex::new(index)?)))
    })
}

/// Open an existing on-disk index.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn st_index_open(path: *const c_char) -> *mut StIndex {
    guard(ptr::null_mut(), || {
        let p = unsafe { req_str(path, "path") }?;
        let index = Index::open_in_dir(p).map_err(err("cannot open index"))?;
        Ok(Box::into_raw(Box::new(StIndex::new(index)?)))
    })
}

/// Release an index handle (uncommitted documents are discarded).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn st_index_free(h: *mut StIndex) {
    if !h.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let mut h = unsafe { Box::from_raw(h) };
            // Let background merges finish so no segment file is still open
            // when we return: on Windows an open file blocks renaming or
            // deleting the index directory right after the object is gone.
            match h.writer.take() {
                Some((w, pid)) if pid == process::id() => {
                    let _ = w.wait_merging_threads();
                }
                // Inherited through fork(): its threads live in the parent.
                // Dropping it here would wait for them forever and could
                // remove the parent's lock file, so leak it instead.
                Some((w, _)) => mem::forget(w),
                None => {}
            }
            drop(h);
        }));
    }
}

fn add_value(h: &mut StIndex, schema: &Schema, v: Value) -> Res<()> {
    let Value::Object(mut map) = v else {
        return Err("document must be a JSON object".into());
    };
    coerce_numbers(schema, &mut map);
    let normalized = Value::Object(map).to_string();
    let doc = TantivyDocument::parse_json(schema, &normalized).map_err(err("invalid document"))?;
    h.writer()?
        .add_document(doc)
        .map_err(err("cannot add document"))?;
    Ok(())
}

/// Add one document given as a JSON object. Returns 0 on success, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn st_index_add_json(h: *mut StIndex, doc_json: *const c_char) -> i32 {
    guard(-1, || {
        let h = unsafe { handle(h) }?;
        let json = unsafe { req_str(doc_json, "document") }?;
        let v: Value = serde_json::from_str(json).map_err(err("document must be a JSON object"))?;
        add_value(h, &h.index.schema(), v)?;
        Ok(0)
    })
}

/// Add every document of a JSON array of objects. Returns the number added,
/// or -1 (documents before the bad one stay added, uncommitted).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn st_index_add_many_json(h: *mut StIndex, docs_json: *const c_char) -> i64 {
    guard(-1, || {
        let h = unsafe { handle(h) }?;
        let json = unsafe { req_str(docs_json, "documents") }?;
        let docs: Vec<Value> =
            serde_json::from_str(json).map_err(err("documents must be a JSON array"))?;
        let schema = h.index.schema();
        let n = docs.len();
        for (i, v) in docs.into_iter().enumerate() {
            add_value(h, &schema, v).map_err(|e| format!("document {i}: {e}"))?;
        }
        Ok(n as i64)
    })
}

/// Commit pending documents and make them visible to searches. 0 / -1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn st_index_commit(h: *mut StIndex) -> i32 {
    guard(-1, || {
        let h = unsafe { handle(h) }?;
        h.writer()?.commit().map_err(err("commit failed"))?;
        h.reader.reload().map_err(err("cannot reload reader"))?;
        Ok(0)
    })
}

/// See commits made by other processes (or other handles) since the last
/// reload. Returns 0 / -1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn st_index_reload(h: *mut StIndex) -> i32 {
    guard(-1, || {
        let h = unsafe { handle(h) }?;
        h.reader.reload().map_err(err("cannot reload reader"))?;
        Ok(0)
    })
}

/// Number of committed documents, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn st_index_num_docs(h: *mut StIndex) -> i64 {
    guard(-1, || {
        let h = unsafe { handle(h) }?;
        Ok(h.reader.searcher().num_docs() as i64)
    })
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SearchOptions {
    /// Default fields for terms without a `field:` prefix.
    fields: Option<Vec<String>>,
    limit: usize,
    /// Number of top hits to skip (pagination).
    offset: usize,
    /// Also count all matching documents.
    count: bool,
    /// Edit distance (1 or 2) for typo-tolerant matching of query terms.
    fuzzy: u8,
    /// Order by this fast field instead of relevance; "-field" = descending.
    sort: Option<String>,
    /// Stored fields to return in each hit (default: all).
    return_fields: Option<Vec<String>>,
    /// Stored text fields to build highlighted snippets for.
    snippets: Vec<String>,
    snippet_chars: Option<usize>,
    /// Markup around highlighted terms, default `<b>` / `</b>`.
    highlight: Option<(String, String)>,
    /// "OR" (default: any term may match) or "AND" (all terms must).
    default_operator: Option<String>,
    /// Score multiplier per field, e.g. {"title": 3.0}.
    boost: BTreeMap<String, f32>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            fields: None,
            limit: 10,
            offset: 0,
            count: false,
            fuzzy: 0,
            sort: None,
            return_fields: None,
            snippets: Vec::new(),
            snippet_chars: None,
            highlight: None,
            default_operator: None,
            boost: Default::default(),
        }
    }
}

fn field_by_name(schema: &Schema, name: &str) -> Res<Field> {
    schema
        .get_field(name)
        .map_err(|_| format!("unknown field '{name}'"))
}

/// Top documents with their sort key (as JSON: the score, or the sort
/// field's value) and optionally the total number of matches.
type Collected = (Vec<(Value, DocAddress)>, Option<usize>);

fn collect<K, C>(
    searcher: &Searcher,
    q: &dyn Query,
    top: C,
    count: bool,
    key: impl Fn(K) -> Value,
) -> Res<Collected>
where
    C: Collector<Fruit = Vec<(K, DocAddress)>>,
{
    let (top, total) = if count {
        let (n, top) = searcher
            .search(q, &(Count, top))
            .map_err(err("search failed"))?;
        (top, Some(n))
    } else {
        (
            searcher.search(q, &top).map_err(err("search failed"))?,
            None,
        )
    };
    Ok((top.into_iter().map(|(k, a)| (key(k), a)).collect(), total))
}

fn date_value(d: DateTime) -> Value {
    normalize_date(&Value::from(d.into_timestamp_secs())).unwrap_or(Value::Null)
}

/// Run the query with the requested order. Hits carry their sort key.
fn top_docs(
    searcher: &Searcher,
    schema: &Schema,
    q: &dyn Query,
    opts: &SearchOptions,
) -> Res<Collected> {
    if opts.limit == 0 {
        let total = if opts.count {
            Some(searcher.search(q, &Count).map_err(err("search failed"))?)
        } else {
            None
        };
        return Ok((Vec::new(), total));
    }
    let top = TopDocs::with_limit(opts.limit).and_offset(opts.offset);
    let Some(spec) = &opts.sort else {
        return collect(searcher, q, top.order_by_score(), opts.count, |s: f32| {
            Value::from(s)
        });
    };
    let (name, order) = match spec.strip_prefix('-') {
        Some(n) => (n, Order::Desc),
        None => (spec.as_str(), Order::Asc),
    };
    let entry = schema.get_field_entry(field_by_name(schema, name)?);
    if !entry.is_fast() {
        return Err(format!(
            "cannot sort by '{name}': only numeric, date and string fields are sortable \
             (string fields of indexes made before version 0.05 are not; rebuild the index)"
        ));
    }
    let c = opts.count;
    match entry.field_type() {
        FieldType::U64(_) => collect(
            searcher,
            q,
            top.order_by_fast_field::<u64>(name, order),
            c,
            |v| v.map_or(Value::Null, Value::from),
        ),
        FieldType::I64(_) => collect(
            searcher,
            q,
            top.order_by_fast_field::<i64>(name, order),
            c,
            |v| v.map_or(Value::Null, Value::from),
        ),
        FieldType::F64(_) => collect(
            searcher,
            q,
            top.order_by_fast_field::<f64>(name, order),
            c,
            |v| v.map_or(Value::Null, Value::from),
        ),
        FieldType::Date(_) => collect(
            searcher,
            q,
            top.order_by_fast_field::<DateTime>(name, order),
            c,
            |v| v.map_or(Value::Null, date_value),
        ),
        FieldType::Str(_) => collect(
            searcher,
            q,
            top.order_by_string_fast_field(name, order),
            c,
            |v| v.map_or(Value::Null, Value::String),
        ),
        _ => Err(format!("cannot sort by '{name}'")),
    }
}

/// Text fields the query parser would make fuzzy (tokenized or raw strings).
fn is_text(schema: &Schema, f: Field) -> bool {
    matches!(schema.get_field_entry(f).field_type(), FieldType::Str(_))
}

/// Index terms within `distance` edits of `term` (excluding `term` itself),
/// across all segments, at most `MAX_FUZZY_VARIANTS`.
fn fuzzy_variants(
    searcher: &tantivy::Searcher,
    term: &Term,
    distance: u8,
) -> Res<BTreeSet<Vec<u8>>> {
    let automaton = FuzzyTermQuery::new(term.clone(), distance, true)
        .automaton()
        .map_err(err("fuzzy query"))?;
    let own = term.serialized_value_bytes();
    let mut out = BTreeSet::new();
    for segment in searcher.segment_readers() {
        let inverted = segment
            .inverted_index(term.field())
            .map_err(err("cannot read terms"))?;
        let mut stream = inverted
            .terms()
            .search(&automaton)
            .into_stream()
            .map_err(err("cannot read terms"))?;
        while stream.advance() {
            if stream.key() != own {
                out.insert(stream.key().to_vec());
                if out.len() >= MAX_FUZZY_VARIANTS {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

/// Typo-tolerant version of `exact`. tantivy's fuzzy term queries match but
/// give every hit the same score and report no terms to highlight, so:
/// documents must match `fuzzy` (the same query with fuzzy terms), and are
/// scored by `exact` plus the real index terms the fuzzy terms expand to.
fn fuzzy_query(
    searcher: &tantivy::Searcher,
    schema: &Schema,
    exact: Box<dyn Query>,
    fuzzy: Box<dyn Query>,
    distance: u8,
    boost: &BTreeMap<String, f32>,
) -> Res<Box<dyn Query>> {
    let mut terms = Vec::new();
    exact.query_terms(&mut |t, _| terms.push(t.clone()));
    let mut clauses: Vec<(Occur, Box<dyn Query>)> =
        vec![(Occur::Must, fuzzy), (Occur::Should, exact)];
    for term in terms.iter().filter(|t| is_text(schema, t.field())) {
        let field_boost = boost
            .get(schema.get_field_name(term.field()))
            .copied()
            .unwrap_or(1.0);
        for bytes in fuzzy_variants(searcher, term, distance)? {
            let Ok(text) = std::str::from_utf8(&bytes) else {
                continue;
            };
            let q = TermQuery::new(
                Term::from_field_text(term.field(), text),
                IndexRecordOption::WithFreqs,
            );
            clauses.push((
                Occur::Should,
                Box::new(BoostQuery::new(
                    Box::new(q),
                    FUZZY_VARIANT_BOOST * field_boost,
                )),
            ));
        }
    }
    Ok(Box::new(BooleanQuery::new(clauses)))
}

/// Run a query. `options_json` is an optional JSON object (see
/// `SearchOptions`). Returns a JSON object
/// `{"hits": [{"score": f, "doc": {...}, "snippets": {...}}], "total": n}`
/// (`total` only with the `count` option) to be freed with
/// `st_string_free`, or NULL.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn st_index_search(
    h: *mut StIndex,
    query: *const c_char,
    options_json: *const c_char,
) -> *mut c_char {
    guard(ptr::null_mut(), || {
        let h = unsafe { handle(h) }?;
        let query = unsafe { req_str(query, "query") }?;
        let opts: SearchOptions = match unsafe { opt_str(options_json) }? {
            Some(j) => serde_json::from_str(j).map_err(err("invalid search options"))?,
            None => SearchOptions::default(),
        };
        let schema = h.index.schema();

        let fields: Vec<Field> = match &opts.fields {
            Some(names) => names
                .iter()
                .map(|n| field_by_name(&schema, n))
                .collect::<Res<_>>()?,
            None => schema
                .fields()
                .filter(|(_, e)| matches!(e.field_type(), FieldType::Str(_)) && e.is_indexed())
                .map(|(f, _)| f)
                .collect(),
        };

        let mut parser = QueryParser::for_index(&h.index, fields.clone());
        match opts
            .default_operator
            .as_deref()
            .map(str::to_ascii_uppercase)
            .as_deref()
        {
            None | Some("OR") => {}
            Some("AND") => parser.set_conjunction_by_default(),
            Some(other) => {
                return Err(format!("default_operator must be AND or OR, got '{other}'"));
            }
        }
        for (name, factor) in &opts.boost {
            if !factor.is_finite() || *factor <= 0.0 {
                return Err(format!("boost for '{name}' must be a positive number"));
            }
            parser.set_field_boost(field_by_name(&schema, name)?, *factor);
        }
        let exact = parser.parse_query(query).map_err(err("invalid query"))?;
        let searcher = h.reader.searcher();
        let q = match opts.fuzzy {
            0 => exact,
            d @ (1 | 2) => {
                for f in fields.iter().filter(|f| is_text(&schema, **f)) {
                    parser.set_field_fuzzy(*f, false, d, true);
                }
                let fuzzy = parser.parse_query(query).map_err(err("invalid query"))?;
                fuzzy_query(&searcher, &schema, exact, fuzzy, d, &opts.boost)?
            }
            other => return Err(format!("fuzzy must be 0, 1 or 2, got {other}")),
        };

        let mut generators = Vec::with_capacity(opts.snippets.len());
        for name in &opts.snippets {
            let field = field_by_name(&schema, name)?;
            let entry = schema.get_field_entry(field);
            if !matches!(entry.field_type(), FieldType::Str(_)) || !entry.is_stored() {
                return Err(format!(
                    "snippets need a stored text field, '{name}' is not one"
                ));
            }
            let mut g = SnippetGenerator::create(&searcher, &*q, field)
                .map_err(err("cannot create snippet generator"))?;
            if let Some(n) = opts.snippet_chars {
                g.set_max_num_chars(n);
            }
            generators.push((name.as_str(), g));
        }

        let (top, total) = top_docs(&searcher, &schema, &*q, &opts)?;
        let keep: Option<BTreeSet<&str>> = match &opts.return_fields {
            None => None,
            Some(names) => {
                for n in names {
                    field_by_name(&schema, n)?;
                }
                Some(names.iter().map(String::as_str).collect())
            }
        };

        let mut hits = Vec::with_capacity(top.len());
        let sort_key = if opts.sort.is_some() { "sort" } else { "score" };
        for (key, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr).map_err(err("cannot load document"))?;
            let mut doc_val: Value = serde_json::from_str(&doc.to_json(&schema))
                .map_err(err("cannot encode document"))?;
            if let (Some(keep), Value::Object(m)) = (&keep, &mut doc_val) {
                m.retain(|k, _| keep.contains(k.as_str()));
            }
            let mut hit = serde_json::json!({ "doc": flatten_single_values(doc_val) });
            hit[sort_key] = key;
            if !generators.is_empty() {
                let mut snippets = Map::new();
                for (name, g) in &generators {
                    let mut snippet = g.snippet_from_doc(&doc);
                    if let Some((pre, post)) = &opts.highlight {
                        snippet.set_snippet_prefix_postfix(pre, post);
                    }
                    snippets.insert((*name).to_string(), Value::String(snippet.to_html()));
                }
                hit["snippets"] = Value::Object(snippets);
            }
            hits.push(hit);
        }
        let mut out = serde_json::json!({ "hits": hits });
        if let Some(n) = total {
            out["total"] = Value::from(n);
        }
        into_c_string(out.to_string())
    })
}

/// Build an exact-match term for `field` from a JSON scalar. Only `string`
/// (untokenized) and numeric fields qualify: a term on a tokenized text field
/// would match every document containing that word.
fn exact_term(schema: &Schema, name: &str, value: &Value) -> Res<Term> {
    let field = field_by_name(schema, name)?;
    let entry = schema.get_field_entry(field);
    let as_text = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => {
            return Err(format!(
                "field '{name}': key value must be a string or number, got {other}"
            ));
        }
    };
    let bad_num = |t: &str| format!("field '{name}': '{as_text}' is not a valid {t}");
    match entry.field_type() {
        FieldType::Str(opts) => {
            let raw = opts
                .get_indexing_options()
                .is_some_and(|i| i.tokenizer() == "raw");
            if !raw {
                return Err(format!(
                    "field '{name}' is a tokenized text field; delete/update need a 'string' or numeric key field"
                ));
            }
            Ok(Term::from_field_text(field, &as_text))
        }
        FieldType::U64(_) => as_text
            .trim()
            .parse()
            .map(|v| Term::from_field_u64(field, v))
            .map_err(|_| bad_num("u64")),
        FieldType::I64(_) => as_text
            .trim()
            .parse()
            .map(|v| Term::from_field_i64(field, v))
            .map_err(|_| bad_num("i64")),
        FieldType::F64(_) => as_text
            .trim()
            .parse()
            .map(|v| Term::from_field_f64(field, v))
            .map_err(|_| bad_num("f64")),
        _ => Err(format!("field '{name}' cannot be used as a key")),
    }
}

/// Delete every document whose `field` equals the JSON scalar `value_json`.
/// Takes effect on the next commit. Returns 0 / -1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn st_index_delete_term(
    h: *mut StIndex,
    field: *const c_char,
    value_json: *const c_char,
) -> i32 {
    guard(-1, || {
        let h = unsafe { handle(h) }?;
        let field = unsafe { req_str(field, "field") }?;
        let value: Value = serde_json::from_str(unsafe { req_str(value_json, "value") }?)
            .map_err(err("invalid key value"))?;
        let term = exact_term(&h.index.schema(), field, &value)?;
        h.writer()?.delete_term(term);
        Ok(0)
    })
}

/// Free a string returned by this library.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn st_string_free(s: *mut c_char) {
    if !s.is_null() {
        drop(unsafe { CString::from_raw(s) });
    }
}
