package Search::Tantivy;

use strict;
use warnings;

our $VERSION = '0.02';

use Carp qw(croak);
use File::Basename ();
use File::Spec ();
use FFI::Platypus 2.00;
use JSON::PP ();

my $JSON = JSON::PP->new->utf8->canonical->allow_nonref;

my $ffi = FFI::Platypus->new( api => 2 );
$ffi->lib( _find_lib() );
$ffi->type( 'object(Search::Tantivy)' => 'st_index' );

$ffi->attach( [ st_last_error     => '_last_error' ]  => []                                     => 'string' );
$ffi->attach( [ st_index_create   => '_create' ]      => [ 'string', 'string' ]                 => 'st_index' );
$ffi->attach( [ st_index_open     => '_open' ]        => ['string']                             => 'st_index' );
$ffi->attach( [ st_index_add_json => '_add_json' ]    => [ 'st_index', 'string' ]               => 'int' );
$ffi->attach( [ st_index_commit   => '_commit' ]      => ['st_index']                           => 'int' );
$ffi->attach( [ st_index_reload   => '_reload' ]      => ['st_index']                           => 'int' );
$ffi->attach( [ st_index_num_docs => '_num_docs' ]    => ['st_index']                           => 'sint64' );
$ffi->attach( [ st_index_search   => '_search' ]      => [ 'st_index', 'string', 'string' ]     => 'opaque' );
$ffi->attach( [ st_index_delete_term => '_delete_term' ] => [ 'st_index', 'string', 'string' ] => 'int' );
$ffi->attach( [ st_string_free    => '_string_free' ] => ['opaque']                             => 'void' );
$ffi->attach( [ st_index_free     => 'DESTROY' ]      => ['st_index']                           => 'void' );

sub _find_lib {
    return $ENV{SEARCH_TANTIVY_LIB} if $ENV{SEARCH_TANTIVY_LIB};

    my $name =
        $^O eq 'MSWin32' ? 'search_tantivy.dll'
      : $^O eq 'darwin'  ? 'libsearch_tantivy.dylib'
      :                    'libsearch_tantivy.so';

    # Installed (or blib/ during "make test"): auto/Search/Tantivy/<lib>
    for my $dir ( grep { !ref } @INC ) {
        my $path = File::Spec->catfile( $dir, qw(auto Search Tantivy), $name );
        return $path if -f $path;
    }

    # Development checkout: lib/Search/Tantivy.pm -> rust/target/release/<lib>
    my $root = File::Spec->catdir( File::Basename::dirname(__FILE__), File::Spec->updir, File::Spec->updir );
    for my $profile (qw(release debug)) {
        my $path = File::Spec->catfile( $root, 'rust', 'target', $profile, $name );
        return $path if -f $path;
    }

    croak "Search::Tantivy: cannot find $name (run 'cargo build --release' in rust/ or set SEARCH_TANTIVY_LIB)";
}

sub _fail { croak 'Search::Tantivy: ' . ( _last_error() // 'unknown error' ) }

sub create {
    my ( $class, %args ) = @_;
    my $fields = $args{fields} or croak 'create: "fields" is required';
    ref $fields eq 'ARRAY' or croak 'create: "fields" must be an array ref';

    my @spec;
    my @list = @$fields;
    while (@list) {
        my $item = shift @list;
        my %f;
        if ( ref $item eq 'HASH' ) {
            %f = %$item;
        }
        else {
            @list or croak "create: field '$item' has no type";
            %f = ( name => $item, type => shift @list );
        }
        defined $f{type} or croak "create: field '" . ( $f{name} // '?' ) . "' has no type";
        # shorthand: 'text:polish', 'text:en'
        ( $f{type}, $f{language} ) = ( $1, $2 ) if $f{type} =~ /\A(text):(\w+)\z/;
        my @unknown = grep { !/\A(?:name|type|stored|language|ascii_folding)\z/ } sort keys %f;
        croak "create: field '$f{name}': unknown option(s): @unknown" if @unknown;

        push @spec, {
            name          => $f{name},
            type          => $f{type},
            stored        => ( !exists $f{stored} || $f{stored} ) ? JSON::PP::true : JSON::PP::false,
            ascii_folding => $f{ascii_folding} ? JSON::PP::true : JSON::PP::false,
            ( defined $f{language} ? ( language => $f{language} ) : () ),
        };
    }

    my $self = _create( $args{path}, $JSON->encode( \@spec ) ) or _fail();
    return $self;
}

sub open {
    my ( $class, $path ) = @_;
    defined $path or croak 'open: path is required';
    my $self = _open($path) or _fail();
    return $self;
}

sub add {
    my ( $self, $doc ) = @_;
    ref $doc eq 'HASH' or croak 'add: document must be a hash ref';
    _add_json( $self, $JSON->encode($doc) ) == 0 or _fail();
    return $self;
}

sub delete {
    my ( $self, $field, $value ) = @_;
    defined $field && defined $value or croak 'delete: field and value are required';
    _delete_term( $self, $field, $JSON->encode($value) ) == 0 or _fail();
    return $self;
}

sub update {
    my ( $self, $key, $doc ) = @_;
    defined $key or croak 'update: key field is required';
    ref $doc eq 'HASH' or croak 'update: document must be a hash ref';
    defined $doc->{$key} or croak "update: document has no value for key field '$key'";
    return $self->delete( $key, $doc->{$key} )->add($doc);
}

sub commit {
    my ($self) = @_;
    _commit($self) == 0 or _fail();
    return $self;
}

sub reload {
    my ($self) = @_;
    _reload($self) == 0 or _fail();
    return $self;
}

sub num_docs {
    my ($self) = @_;
    my $n = _num_docs($self);
    $n >= 0 or _fail();
    return $n;
}

sub search {
    my ( $self, $query, %opt ) = @_;
    defined $query or croak 'search: query is required';
    my %known = map { $_ => 1 } qw(limit fields snippets snippet_chars highlight);
    my @unknown = grep { !$known{$_} } sort keys %opt;
    croak "search: unknown option(s): @unknown" if @unknown;
    $opt{limit} += 0 if defined $opt{limit};
    $opt{snippet_chars} += 0 if defined $opt{snippet_chars};

    my $ptr = _search( $self, $query, $JSON->encode( \%opt ) ) or _fail();
    my $json = $ffi->cast( 'opaque' => 'string', $ptr );
    _string_free($ptr);
    return @{ $JSON->decode($json) };
}

1;

__END__

=encoding utf8

=head1 NAME

Search::Tantivy - fast full-text search for Perl, powered by the Rust tantivy engine

=head1 SYNOPSIS

    use Search::Tantivy;

    my $index = Search::Tantivy->create(
        path   => '/var/lib/myapp/index',   # omit for an in-memory index
        fields => [
            title => 'text',
            body  => 'text',
            url   => 'string',
            year  => 'u64',
        ],
    );

    $index->add({ title => 'Programming Perl', body => 'The camel book', url => '/perl', year => 1991 });
    $index->add({ title => 'The Rust Book',    body => 'Ownership and borrowing', url => '/rust', year => 2018 });
    $index->commit;

    for my $hit ( $index->search('perl OR camel', limit => 5) ) {
        printf "%.3f  %s\n", $hit->{score}, $hit->{doc}{title};
    }

    # later, from another process
    my $again = Search::Tantivy->open('/var/lib/myapp/index');

=head1 DESCRIPTION

Search::Tantivy embeds L<tantivy|https://github.com/quickwit-oss/tantivy>, a
full-text search engine library written in Rust (similar in spirit to Lucene),
and exposes it through L<FFI::Platypus>. No server to run: the index is a
directory on disk, or lives in memory.

This is an early release; the API may change.

=head1 METHODS

=head2 create(%args)

Creates a new index. C<path> is an existing, empty directory; leave it out for
an in-memory index. C<fields> is an array ref of C<< name => type >> pairs, or
hash refs C<< { name => ..., type => ..., stored => 0|1 } >>. Fields are stored
(returned in search results) unless C<stored> is false.

Types: C<text> (tokenized, full-text searchable), C<string> (indexed as one
exact token, e.g. ids, URLs, tags), C<u64>, C<i64>, C<f64>.

=head3 Language-aware text fields

By default a C<text> field matches words exactly (case-insensitively). Give it
a C<language> to match inflected forms too - C<run> finds I<running>, C<książka>
finds I<książki>:

    fields => [
        title => 'text:en',                     # shorthand for language => 'en'
        { name => 'body', type => 'text', language => 'polish', ascii_folding => 1 },
    ]

C<language> takes an ISO 639-1 code or English name: ar (arabic), da (danish),
de (german), el (greek), en (english), es (spanish), fi (finnish), fr (french),
hu (hungarian), it (italian), nl (dutch), no (norwegian), pl (polish),
pt (portuguese), ro (romanian), ru (russian), sv (swedish), ta (tamil),
tr (turkish). Polish uses the Yarovoy stemmer, the rest Snowball.

C<ascii_folding> also maps accented letters to ASCII, so a query typed without
diacritics finds the original (C<zolw> finds I<żółw>, C<creme> finds
I<crème>). It works with or without C<language>; for Polish it switches to a
stemmer designed for unaccented text.

The analysis is fixed when the index is created; to change it, build a new
index.

=head2 open($path)

Opens an existing on-disk index.

=head2 add(\%doc)

Adds a document. Numeric fields accept numbers or numeric strings. Documents
are not searchable until L</commit>.

=head2 delete($field, $value)

Deletes every document whose C<$field> equals C<$value>. The field must be a
C<string> or numeric field (an exact key such as an id or URL); tokenized
C<text> fields are rejected. Takes effect on L</commit>.

=head2 update($key_field, \%doc)

Replaces the document(s) whose C<$key_field> equals C<< $doc->{$key_field} >>
with C<\%doc> (or just adds it if there was none). Same rules as L</delete>;
takes effect on L</commit>.

    $index->update( url => { url => '/perl', title => 'Programming Perl, 4th ed.' } )->commit;

=head2 commit

Writes pending documents and makes them visible to L</search>.

=head2 search($query, %opt)

Runs a query in tantivy's query language (C<perl AND camel>, C<title:rust>,
C<"exact phrase">, C<year:[2000 TO 2020]>). Options: C<limit> (default 10),
C<fields> (array ref of fields searched by terms without a C<field:> prefix;
default: all text and string fields), C<snippets> (array ref of stored text
fields to build highlighted excerpts for), C<snippet_chars> (maximum excerpt
length, default 150) and C<highlight> (array ref C<< [$before, $after] >>,
default C<< ['<b>', '</b>'] >>).

Returns a list of hash refs C<< { score => $float, doc => \%stored_fields } >>,
best first. With C<snippets>, each hit also has C<< snippets => { $field => $html } >>:
an excerpt with the matching terms wrapped in the highlight markup and the rest
HTML-escaped; empty if the field did not match.

    my ($hit) = $index->search( 'camel', snippets => ['body'] );
    print $hit->{snippets}{body};   # The <b>camel</b> book about Perl 5 A field with one value is a plain scalar; a field with several
values is an array ref.

=head2 reload

Picks up commits made by other processes (or other objects) on the same
on-disk index since this one was opened or last reloaded. Your own
L</commit> reloads automatically. A long-running searcher can call it
periodically, or before each search when freshness matters:

    # cron job:  $writer->update( id => $_ )->commit for @changed_rows;
    # web app:   $searcher->reload; my @hits = $searcher->search($q);

Only one process can write to an index at a time; any number can search it.

=head2 num_docs

Number of committed documents.

=head1 ERRORS

All methods C<croak> with a message from tantivy on failure.

=head1 ENVIRONMENT

C<SEARCH_TANTIVY_LIB> - full path of the compiled Rust library, overriding the
built-in lookup.

=head1 AUTHOR

Piotr Jackowski

=head1 LICENSE

This library is free software; you can redistribute it and/or modify it under
the same terms as Perl itself. The bundled Rust code links against tantivy
(MIT license).

=cut
