# Search::Tantivy

[![CI](https://github.com/Neid/Search-Tantivy/actions/workflows/ci.yml/badge.svg)](https://github.com/Neid/Search-Tantivy/actions/workflows/ci.yml)

Fast full-text search for Perl, powered by [tantivy](https://github.com/quickwit-oss/tantivy) (Rust).
Embedded: no search server, the index is a directory (or in memory).

```perl
use Search::Tantivy;

my $index = Search::Tantivy->create(
    path   => 'my-index',                       # omit for in-memory
    fields => [ title => 'text', body => 'text', url => 'string', year => 'u64' ],
);
$index->add({ title => 'Programming Perl', body => 'The camel book', url => '/perl', year => 1991 });
$index->commit;

for my $hit ($index->search('camel OR title:perl', limit => 5, snippets => ['body'])) {
    printf "%.3f %s - %s\n", $hit->{score}, $hit->{doc}{title}, $hit->{snippets}{body};
    # 1.234 Programming Perl - The <b>camel</b> book
}

# replace or remove documents by an exact key ('string' or numeric field)
$index->update(url => { url => '/perl', title => 'Programming Perl, 4th ed.', body => '...' });
$index->delete(url => '/old-page');
$index->commit;

# page 3 of the results, and how many there are in total
my $page = $index->search_page('perl', limit => 10, offset => 20);
print "$page->{total} results\n";

# typo tolerance: "mechanzie" finds "mechanize"
my @hits = $index->search('mechanzie', fuzzy => 1);

# newest first (numeric, date and string fields sort; dates take epoch or ISO)
my @latest = $index->search('perl', sort => '-year', limit => 20);
```

Safe with preforking servers (Starman, mod_perl): open the index in the parent and
search in the children. See *FORK AND THREADS* in the POD for writing.

Status: early, API may change.

### Why

Perl's own full-text engines have aged out: [Lucy](https://metacpan.org/pod/Lucy)
(formerly KinoSearch) was retired by Apache in 2018 and is no longer developed, and
Plucene predates it. The usual alternatives are a search server (Elasticsearch,
OpenSearch, Solr) to run and operate, or SQLite FTS5, which is easy but basic in
ranking, stemming and query syntax. Search::Tantivy embeds
[tantivy](https://github.com/quickwit-oss/tantivy), a maintained engine with
Lucene-style queries, BM25 ranking, stemming in 19 languages, fuzzy matching, sorting
and snippets, and installs as a normal CPAN-style module with a prebuilt library.

### Languages

`text` fields can stem words for 19 languages, including Polish, and fold accents, so a
search for `książka` finds *książki* and `zolw` finds *żółw*:

```perl
fields => [
    title => 'text:en',
    { name => 'body', type => 'text', language => 'polish', ascii_folding => 1 },
]
```

### Example: search your Perl documentation

[`examples/podsearch`](examples/podsearch) indexes the POD of every module in `@INC` and
searches it with highlighted results (matches are bold in a terminal; the time
includes opening the index):

```
$ examples/podsearch --index
Indexed 1587 documents (17.5 MB of POD text) in 0.9s -> /home/you/.cache/podsearch
$ examples/podsearch -n 1 'Data::Dumper'
 1. Data::Dumper - stringified perl data structures, suitable for both printing and eval
    feasible $Data::Dumper::Indent = 0; # turn off all pretty print print Dumper($boo), "\n"; $Data: ...
    perldoc Data::Dumper

1 of 59 matches (1587 documents) in 9.1 ms
```

Run `podsearch --index` from cron to keep it current; searches keep working during a
rebuild.

## Installing

```sh
cpanm https://github.com/Neid/Search-Tantivy/releases/download/v0.05/Search-Tantivy-0.05.tar.gz
```

(Not on CPAN yet; this installs the release tarball straight from GitHub.)

No Rust needed on Linux (x86_64/aarch64, glibc 2.17+ or musl), macOS (Apple Silicon/Intel)
and 64-bit Windows: `Makefile.PL` downloads the prebuilt library for your platform from the
matching GitHub release and checks it against the SHA-256 checksums shipped inside the CPAN
tarball. Elsewhere, or if the download fails, it builds from source with `cargo`.

`SEARCH_TANTIVY_BUILD=source` always builds from source; `SEARCH_TANTIVY_BUILD=binary`
fails rather than falling back.

## Developing

```sh
cargo build --release --manifest-path rust/Cargo.toml && prove -l t
```

Releasing: bump `$VERSION` and `Changes`, push a `vX.YY` tag. The release workflow builds
all platforms, publishes a GitHub release with the CPAN tarball, test-installs it on clean
machines without Rust, then marks the release final. Upload that tarball to PAUSE.

## Benchmark

[`examples/bench`](examples/bench) indexes the same POD corpus with every engine it
finds (Search::Tantivy, SQLite FTS5 via DBD::SQLite, Lucy) and times indexing and
queries (top 10 + total count, all words required):

```
$ examples/bench
Corpus: 1587 documents, 17.7 MB of text (POD in @INC)
engine                              index (s)  size (MB)   query (ms)     hits
Search::Tantivy 0.05                     0.45       15.5        0.289     1652
```

Numbers from one laptop; run it on yours.

## How it works

`rust/` is a `cdylib` exposing a small C API (`st_index_create`, `st_index_add_json`,
`st_index_search`, ...). `lib/Search/Tantivy.pm` binds it with FFI::Platypus. Schemas,
documents and results cross the boundary as JSON; Rust panics are caught and turned
into Perl exceptions.

## Roadmap

- facets (counts per category)
- stop words per language

## AI disclosure

Developed with AI coding tools; see [AI_DISCLOSURE.md](AI_DISCLOSURE.md).

## License

Same terms as Perl itself.
