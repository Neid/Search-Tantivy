use strict;
use warnings;
use Test2::V0;

use Search::Tantivy;

my $idx = Search::Tantivy->create(
    fields => [ id => 'u64', title => 'text', price => 'f64', delta => 'i64', sku => 'string', published => 'date' ] );
$idx->add_many( [
    { id => 1, title => 'perl book',   price => 30.5, delta => -3, sku => 'b-200', published => '2024-05-01' },
    { id => 2, title => 'perl course', price => 99,   delta => 7,  sku => 'a-100', published => '2023-01-15 09:30:00' },
    { id => 3, title => 'perl shirt',  price => 12,   delta => 0,  sku => 'c-300', published => 1735689600 },    # 2025-01-01
    { id => 4, title => 'rust book',   price => 40,   delta => 1,  sku => 'd-400', published => '2022-12-31T23:59:59Z' },
] );
$idx->commit;
is $idx->num_docs, 4, 'add_many added all';

sub ids { [ map { $_->{doc}{id} } @_ ] }

subtest 'sort' => sub {
    is ids( $idx->search( 'perl', sort => 'price' ) ),      [ 3, 1, 2 ], 'f64 ascending';
    is ids( $idx->search( 'perl', sort => '-price' ) ),     [ 2, 1, 3 ], 'f64 descending';
    is ids( $idx->search( 'perl', sort => 'delta' ) ),      [ 1, 3, 2 ], 'i64 with negatives';
    is ids( $idx->search( 'perl', sort => 'sku' ) ),        [ 2, 1, 3 ], 'string';
    is ids( $idx->search( 'perl', sort => '-published' ) ), [ 3, 1, 2 ], 'newest first';
    is ids( $idx->search( '*',    sort => 'id', limit => 2, offset => 1 ) ), [ 2, 3 ], 'with offset, all docs';

    my ($hit) = $idx->search( 'perl', sort => '-published' );
    is $hit->{sort}, '2025-01-01T00:00:00Z', 'hit carries the sort value';
    ok !exists $hit->{score}, 'no score when sorted by a field';

    my $page = $idx->search_page( 'perl', sort => 'id', limit => 1 );
    is [ $page->{total}, ids( @{ $page->{hits} } ) ], [ 3, [1] ], 'search_page with sort';

    like dies { $idx->search( 'perl', sort => 'title' ) }, qr/cannot sort by 'title'/, 'text field not sortable';
    like dies { $idx->search( 'perl', sort => 'nope' ) },  qr/unknown field 'nope'/,   'unknown sort field';
};

subtest 'dates' => sub {
    my %pub = map { $_->{doc}{id} => $_->{doc}{published} } $idx->search( '*', limit => 10 );
    is \%pub, {
        1 => '2024-05-01T00:00:00Z',
        2 => '2023-01-15T09:30:00Z',
        3 => '2025-01-01T00:00:00Z',
        4 => '2022-12-31T23:59:59Z',
    }, 'date forms normalized to RFC 3339 UTC';

    is ids( $idx->search( 'published:[2023-01-01T00:00:00Z TO 2024-12-31T00:00:00Z]', sort => 'id' ) ), [ 1, 2 ],
      'date range query';
    like dies { $idx->add( { id => 9, published => 'last tuesday' } ) }, qr/invalid document/, 'bad date';
};

subtest 'return_fields' => sub {
    my ($hit) = $idx->search( 'shirt', return_fields => [ 'id', 'sku' ] );
    is $hit->{doc}, { id => 3, sku => 'c-300' }, 'only the requested fields';
    ($hit) = $idx->search( 'shirt', return_fields => [] );
    is $hit->{doc}, {}, 'no fields';
    like dies { $idx->search( 'shirt', return_fields => ['nope'] ) }, qr/unknown field 'nope'/, 'unknown field';
};

subtest 'JSON backends' => sub {
    require File::Temp;
    my $script = File::Temp->new( SUFFIX => '.pl' );
    binmode $script, ':encoding(UTF-8)';    # Test2::V0 enables 'use utf8'
    print {$script} <<'PERL';
use utf8;
use Search::Tantivy;
my $i = Search::Tantivy->create( fields => [ t => 'text', d => 'date' ] );
$i->add( { t => 'zażółć', d => 0 } )->commit;
my ($h) = $i->search('zażółć');
print $h->{doc}{d}, ' ', length $h->{doc}{t};
PERL
    close $script;
    for my $class (qw(JSON::PP JSON::XS Cpanel::JSON::XS)) {
        next unless eval "require $class; 1";
        local $ENV{SEARCH_TANTIVY_JSON} = $class;
        my $inc = join ' ', map {qq{"-I$_"}} grep { !ref } @INC;
        my $out = `"$^X" $inc "$script"`;
        is $out, '1970-01-01T00:00:00Z 6', "$class round trip (UTF-8, dates)";
    }
};

subtest 'add_many errors' => sub {
    my $i = Search::Tantivy->create( fields => [ id => 'u64' ] );
    like dies { $i->add_many( [ { id => 1 }, { id => 'x' } ] ) }, qr/document 1: invalid document/, 'error names the document';
    like dies { $i->add_many( { id => 1 } ) }, qr/array ref/, 'needs an array ref';
    like dies { $i->add_many( [ { id => 1 }, 'x' ] ) }, qr/every document must be a hash ref/, 'needs hash refs';
    $i->commit;
    is $i->num_docs, 1, 'documents before the bad one were added';
};

done_testing;
