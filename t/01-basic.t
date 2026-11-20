use strict;
use warnings;
use Test2::V0;
use File::Temp qw(tempdir);

use Search::Tantivy;

my @books = (
    { title => 'Programming Perl',   body => 'The camel book about Perl 5',          url => '/perl',  year => 1991 },
    { title => 'The Rust Book',      body => 'Ownership, borrowing and lifetimes',    url => '/rust',  year => 2018 },
    { title => 'Higher-Order Perl',  body => 'Functional programming in Perl',        url => '/hop',   year => '2005' },
);

my @fields = ( title => 'text', body => 'text', url => 'string', year => 'u64' );

subtest 'in-memory index' => sub {
    my $idx = Search::Tantivy->create( fields => \@fields );
    isa_ok $idx, 'Search::Tantivy';

    $idx->add($_) for @books;
    is $idx->num_docs, 0, 'nothing visible before commit';
    $idx->commit;
    is $idx->num_docs, 3, 'three documents after commit';

    my @hits = $idx->search('perl');
    is scalar @hits, 2, 'two Perl books';
    ok $hits[0]{score} >= $hits[1]{score}, 'sorted by score';
    is [ sort map { $_->{doc}{url} } @hits ], [ '/hop', '/perl' ], 'stored fields returned';

    is [ map { $_->{doc}{title} } $idx->search('title:rust') ], ['The Rust Book'], 'field query';
    is [ map { $_->{doc}{year} } $idx->search('year:[2000 TO 2010]') ], [2005], 'numeric range, string coerced';
    is [ map { $_->{doc}{url} } $idx->search('borrowing', fields => ['title']) ], [], 'restricted default fields';
    is scalar( my @one = $idx->search( 'perl', limit => 1 ) ), 1, 'limit';
};

subtest 'on-disk index survives reopen' => sub {
    my $dir = tempdir( CLEANUP => 1 );
    {
        my $idx = Search::Tantivy->create( path => $dir, fields => \@fields );
        $idx->add($_) for @books;
        $idx->commit;
    }
    my $idx = Search::Tantivy->open($dir);
    is $idx->num_docs, 3, 'documents persisted';
    is [ map { $_->{doc}{title} } $idx->search('camel') ], ['Programming Perl'], 'searchable after reopen';
};

subtest 'unstored fields' => sub {
    my $idx = Search::Tantivy->create( fields => [ { name => 'body', type => 'text', stored => 0 }, url => 'string' ] );
    $idx->add( { body => 'secret words', url => '/x' } )->commit;
    my ($hit) = $idx->search('secret');
    is $hit->{doc}, { url => '/x' }, 'body indexed but not returned';
};

subtest 'errors croak with tantivy messages' => sub {
    my $idx = Search::Tantivy->create( fields => \@fields );
    like dies { Search::Tantivy->create( fields => [ x => 'nope' ] ) }, qr/unknown type 'nope'/, 'bad schema type';
    like dies { $idx->search('title:(') }, qr/invalid query/, 'bad query';
    like dies { $idx->search( 'x', fields => ['missing'] ) }, qr/unknown field 'missing'/, 'unknown field';
    like dies { Search::Tantivy->open('/nonexistent/dir') }, qr/cannot open index/, 'missing index dir';
    like dies { $idx->add('not a hash') }, qr/must be a hash ref/, 'bad document';
};

done_testing;
