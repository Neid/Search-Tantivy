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
```

Status: early, API may change.

## Installing

```sh
cpanm https://github.com/Neid/Search-Tantivy/releases/download/v0.02/Search-Tantivy-0.02.tar.gz
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

## How it works

`rust/` is a `cdylib` exposing a small C API (`st_index_create`, `st_index_add_json`,
`st_index_search`, ...). `lib/Search/Tantivy.pm` binds it with FFI::Platypus. Schemas,
documents and results cross the boundary as JSON; Rust panics are caught and turned
into Perl exceptions.

## Roadmap

- facets and sorting by fast fields
- custom tokenizers and stemming (language support)

## AI disclosure

Developed with AI coding tools; see [AI_DISCLOSURE.md](AI_DISCLOSURE.md).

## License

Same terms as Perl itself.
