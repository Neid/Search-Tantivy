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

#![allow(clippy::missing_safety_doc)] // covered by the crate-level Safety section

use std::any::Any;
use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use serde::Deserialize;
use serde_json::{Map, Value};
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::document::Document as _;
use tantivy::schema::{
    Field, FieldType, IndexRecordOption, NumericOptions, STORED, STRING, Schema, TEXT,
    TextFieldIndexing, TextOptions,
};
use tantivy::snippet::SnippetGenerator;
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

mod analyzers;

const WRITER_HEAP_BYTES: usize = 50_000_000;

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
    writer: Option<IndexWriter>,
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
        if self.writer.is_none() {
            let w = self
                .index
                .writer(WRITER_HEAP_BYTES)
                .map_err(err("cannot open index writer"))?;
            self.writer = Some(w);
        }
        Ok(self.writer.as_mut().expect("just set"))
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
        if s.kind != "text" && (s.language.is_some() || s.ascii_folding) {
            return Err(format!(
                "field '{}': language and ascii_folding only apply to text fields",
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
                let mut o: TextOptions = match analyzers::tokenizer_name(code, s.ascii_folding) {
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
                let o: TextOptions = if s.stored { STRING | STORED } else { STRING };
                b.add_text_field(&s.name, o);
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
                    "field '{}': unknown type '{other}' (expected text, string, u64, i64 or f64)",
                    s.name
                ));
            }
        }
    }
    Ok(b.build())
}

/// Perl hands numbers over as strings whenever a scalar was last used as a
/// string; accept "1999" for numeric fields instead of failing.
fn coerce_numbers(schema: &Schema, doc: &mut Map<String, Value>) {
    fn coerce(v: &mut Value, ft: &FieldType) {
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
        let _ = catch_unwind(AssertUnwindSafe(|| drop(unsafe { Box::from_raw(h) })));
    }
}

/// Add one document given as a JSON object. Returns 0 on success, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn st_index_add_json(h: *mut StIndex, doc_json: *const c_char) -> i32 {
    guard(-1, || {
        let h = unsafe { handle(h) }?;
        let json = unsafe { req_str(doc_json, "document") }?;
        let schema = h.index.schema();
        let mut map: Map<String, Value> =
            serde_json::from_str(json).map_err(err("document must be a JSON object"))?;
        coerce_numbers(&schema, &mut map);
        let normalized = Value::Object(map).to_string();
        let doc =
            TantivyDocument::parse_json(&schema, &normalized).map_err(err("invalid document"))?;
        h.writer()?
            .add_document(doc)
            .map_err(err("cannot add document"))?;
        Ok(0)
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
    /// Stored text fields to build highlighted snippets for.
    snippets: Vec<String>,
    snippet_chars: Option<usize>,
    /// Markup around highlighted terms, default `<b>` / `</b>`.
    highlight: Option<(String, String)>,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            fields: None,
            limit: 10,
            snippets: Vec::new(),
            snippet_chars: None,
            highlight: None,
        }
    }
}

fn field_by_name(schema: &Schema, name: &str) -> Res<Field> {
    schema
        .get_field(name)
        .map_err(|_| format!("unknown field '{name}'"))
}

/// Run a query. `options_json` is an optional JSON object (see
/// `SearchOptions`). Returns a JSON array of
/// `{"score": f, "doc": {...}, "snippets": {...}}` to be freed with
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

        let parser = QueryParser::for_index(&h.index, fields);
        let q = parser.parse_query(query).map_err(err("invalid query"))?;
        let searcher = h.reader.searcher();

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

        let top = searcher
            .search(&q, &TopDocs::with_limit(opts.limit.max(1)).order_by_score())
            .map_err(err("search failed"))?;

        let mut hits = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr).map_err(err("cannot load document"))?;
            let doc_val: Value = serde_json::from_str(&doc.to_json(&schema))
                .map_err(err("cannot encode document"))?;
            let mut hit =
                serde_json::json!({ "score": score, "doc": flatten_single_values(doc_val) });
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
        into_c_string(Value::Array(hits).to_string())
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
