use strict;
use warnings;
use Test2::V0;

use Search::Tantivy;

sub new_index {
    my $idx = Search::Tantivy->create(
        fields => [ id => 'u64', url => 'string', title => 'text', body => 'text', { name => 'secret', type => 'text', stored => 0 } ] );
    $idx->add( { id => 1, url => '/perl', title => 'Programming Perl', body => 'The camel book: a long introduction to Perl 5 and its regular expressions' } );
    $idx->add( { id => 2, url => '/rust', title => 'The Rust Book', body => 'Ownership, borrowing and lifetimes in Rust' } );
    $idx->add( { id => 3, url => '/hop',  title => 'Higher-Order Perl', body => 'Functional programming in Perl & <closures>' } );
    return $idx->commit;
}

subtest 'delete by string and numeric key' => sub {
    my $idx = new_index();
    $idx->delete( url => '/rust' );
    is $idx->num_docs, 3, 'delete is pending until commit';
    $idx->commit;
    is $idx->num_docs, 2, 'deleted after commit';
    is [ $idx->search('rust') ], [], 'no longer found';

    $idx->delete( id => '3' )->commit;    # numeric string is fine
    is [ map { $_->{doc}{url} } $idx->search('perl') ], ['/perl'], 'numeric key delete';

    $idx->delete( url => '/nope' )->commit;
    is $idx->num_docs, 1, 'deleting a missing key is a no-op';
};

subtest 'update replaces by key' => sub {
    my $idx = new_index();
    $idx->update( url => { id => 1, url => '/perl', title => 'Programming Perl, 4th Edition', body => 'Updated camel' } )->commit;
    is $idx->num_docs, 3, 'still three documents';
    my @hits = $idx->search('camel');
    is scalar @hits, 1, 'one camel book';
    is $hits[0]{doc}{title}, 'Programming Perl, 4th Edition', 'new version returned';

    $idx->update( url => { id => 4, url => '/new', title => 'Brand new' } )->commit;
    is $idx->num_docs, 4, 'update of a missing key adds';
};

subtest 'key field rules' => sub {
    my $idx = new_index();
    like dies { $idx->delete( title => 'perl' ) }, qr/tokenized text field/, 'text field rejected as key';
    like dies { $idx->delete( id => 'abc' ) }, qr/not a valid u64/, 'bad numeric key';
    like dies { $idx->delete( missing => 1 ) }, qr/unknown field 'missing'/, 'unknown key field';
    like dies { $idx->update( url => { title => 'x' } ) }, qr/no value for key field 'url'/, 'update without key value';
};

subtest 'snippets' => sub {
    my $idx = new_index();

    my ($hit) = $idx->search( 'camel', snippets => ['body', 'title'] );
    like $hit->{snippets}{body}, qr{<b>camel</b>}, 'match highlighted';
    is $hit->{snippets}{title}, '', 'empty snippet for field without match';

    ($hit) = $idx->search( 'closures', snippets => ['body'] );
    like $hit->{snippets}{body}, qr{Perl &amp; &lt;<b>closures</b>}, 'surrounding text is HTML-escaped';

    ($hit) = $idx->search( 'camel', snippets => ['body'], highlight => [ '[', ']' ] );
    like $hit->{snippets}{body}, qr{\[camel\]}, 'custom highlight markup';

    ($hit) = $idx->search( 'expressions', snippets => ['body'], snippet_chars => 25 );
    ok length( $hit->{snippets}{body} =~ s/<\/?b>//gr ) <= 25, 'snippet_chars limits length';

    ($hit) = $idx->search('camel');
    ok !exists $hit->{snippets}, 'no snippets unless asked';

    like dies { $idx->search( 'x', snippets => ['secret'] ) }, qr/stored text field/, 'unstored field rejected';
    like dies { $idx->search( 'x', snippets => ['id'] ) },     qr/stored text field/, 'numeric field rejected';
    like dies { $idx->search( 'x', colour => 'red' ) },        qr/unknown option\(s\): colour/, 'unknown option';
};

done_testing;
