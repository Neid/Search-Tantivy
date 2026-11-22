use strict;
use warnings;
use Test2::V0;

use Search::Tantivy;

my $idx = Search::Tantivy->create( fields => [ id => 'u64', title => 'text', body => 'text' ] );
$idx->add( { id => $_, title => "Perl note $_", body => 'perl ' x $_ } ) for 1 .. 25;
$idx->add( { id => 100, title => 'WWW::Mechanize', body => 'mechanize a browser session' } );
$idx->add( { id => 101, title => 'Mechanism', body => 'a mechanism of some kind' } );
$idx->commit;

subtest 'offset and search_page' => sub {
    my @all  = map { $_->{doc}{id} } $idx->search( 'perl', limit => 25 );
    my @page = map { $_->{doc}{id} } $idx->search( 'perl', limit => 10, offset => 10 );
    is \@page, [ @all[ 10 .. 19 ] ], 'offset skips the first hits';
    is [ $idx->search( 'perl', offset => 100 ) ], [], 'offset past the end';

    my $res = $idx->search_page( 'perl', limit => 10, offset => 20 );
    is $res->{total}, 25, 'total counts every match';
    is scalar @{ $res->{hits} }, 5, 'last page is short';
    is [ map { $_->{doc}{id} } @{ $res->{hits} } ], [ @all[ 20 .. 24 ] ], 'last page hits';

    is $idx->count('perl'),               25, 'count';
    is $idx->count('nothing-like-this'),  0,  'count with no match';
    is $idx->count( 'perl note', default_operator => 'AND' ), 25, 'count takes query options';
    is [ $idx->search( 'perl', limit => 0 ) ], [], 'limit 0 returns no hits';

    like dies { $idx->count( 'perl', limit => 5 ) }, qr/not used for counting: limit/, 'count rejects paging options';
    like dies { $idx->search( 'perl', offset => -1 ) }, qr/offset must be a non-negative integer/, 'bad offset';
};

subtest 'fuzzy' => sub {
    is [ $idx->search('mechanzie') ], [], 'typo finds nothing without fuzzy';

    my @hits = $idx->search( 'mechanzie', fuzzy => 1 );
    is [ map { $_->{doc}{id} } @hits ], [100], 'transposition is one edit';

    @hits = $idx->search( 'mechanize', fuzzy => 2 );
    is $hits[0]{doc}{id}, 100, 'exact match ranks first';
    ok( ( grep { $_->{doc}{id} == 101 } @hits ), 'distance 2 also reaches mechanism' );

    my ($hit) = $idx->search( 'mechanzie', fuzzy => 1, snippets => ['body'] );
    like $hit->{snippets}{body}, qr{<b>mechanize</b>}, 'snippet highlights the real word';

    is $idx->count( 'mechanzie browsr', fuzzy => 1, default_operator => 'AND' ), 1, 'fuzzy with AND';
    is $idx->count( 'title:mechanzie', fuzzy => 1 ), 1, 'fuzzy with a field prefix';
    like dies { $idx->search( 'x', fuzzy => 3 ) }, qr/fuzzy must be 0, 1 or 2/, 'bad distance';
};

done_testing;
