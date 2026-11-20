package Search::Tantivy;

use strict;
use warnings;

our $VERSION = '0.01';

use Carp qw(croak);
use File::Basename ();
use File::Spec ();
use FFI::Platypus 2.00;
use JSON::PP ();

my $JSON = JSON::PP->new->utf8->canonical;

my $ffi = FFI::Platypus->new( api => 2 );
$ffi->lib( _find_lib() );
$ffi->type( 'object(Search::Tantivy)' => 'st_index' );

$ffi->attach( [ st_last_error     => '_last_error' ]  => []                                     => 'string' );
$ffi->attach( [ st_index_create   => '_create' ]      => [ 'string', 'string' ]                 => 'st_index' );
$ffi->attach( [ st_index_open     => '_open' ]        => ['string']                             => 'st_index' );
$ffi->attach( [ st_index_add_json => '_add_json' ]    => [ 'st_index', 'string' ]               => 'int' );
$ffi->attach( [ st_index_commit   => '_commit' ]      => ['st_index']                           => 'int' );
$ffi->attach( [ st_index_num_docs => '_num_docs' ]    => ['st_index']                           => 'sint64' );
$ffi->attach( [ st_index_search   => '_search' ]      => [ 'st_index', 'string', 'string', 'size_t' ] => 'opaque' );
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
        if ( ref $item eq 'HASH' ) {
            push @spec, {
                name   => $item->{name},
                type   => $item->{type},
                stored => ( exists $item->{stored} ? ( $item->{stored} ? JSON::PP::true : JSON::PP::false ) : JSON::PP::true ),
            };
        }
        else {
            @list or croak "create: field '$item' has no type";
            push @spec, { name => $item, type => shift @list, stored => JSON::PP::true };
        }
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

sub commit {
    my ($self) = @_;
    _commit($self) == 0 or _fail();
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
    my $limit  = $opt{limit} // 10;
    my $fields = $opt{fields} ? $JSON->encode( $opt{fields} ) : undef;

    my $ptr = _search( $self, $query, $fields, $limit ) or _fail();
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

=head2 open($path)

Opens an existing on-disk index.

=head2 add(\%doc)

Adds a document. Numeric fields accept numbers or numeric strings. Documents
are not searchable until L</commit>.

=head2 commit

Writes pending documents and makes them visible to L</search>.

=head2 search($query, %opt)

Runs a query in tantivy's query language (C<perl AND camel>, C<title:rust>,
C<"exact phrase">, C<year:[2000 TO 2020]>). Options: C<limit> (default 10),
C<fields> (array ref of fields searched by terms without a C<field:> prefix;
default: all text and string fields).

Returns a list of hash refs C<< { score => $float, doc => \%stored_fields } >>,
best first. A field with one value is a plain scalar; a field with several
values is an array ref.

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
