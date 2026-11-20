# Search::Tantivy

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

for my $hit ($index->search('camel OR title:perl', limit => 5)) {
    printf "%.3f %s\n", $hit->{score}, $hit->{doc}{title};
}
```

Status: early, API may change.

## Building

Needs a Rust toolchain (`cargo`) and `FFI::Platypus` 2.x.

```sh
perl Makefile.PL && make && make test
```

During development: `cargo build --release --manifest-path rust/Cargo.toml && prove -l t`.

## How it works

`rust/` is a `cdylib` exposing a small C API (`st_index_create`, `st_index_add_json`,
`st_index_search`, ...). `lib/Search/Tantivy.pm` binds it with FFI::Platypus. Schemas,
documents and results cross the boundary as JSON; Rust panics are caught and turned
into Perl exceptions.

## Roadmap

- delete / update documents by term
- snippets with highlighted matches
- facets and sorting by fast fields
- custom tokenizers and stemming (language support)
- prebuilt binaries so installing does not require Rust

## License

Same terms as Perl itself.
