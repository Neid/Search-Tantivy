use strict;
use warnings;
use utf8;
use Test2::V0;
use File::Temp qw(tempdir);

use Search::Tantivy;


sub titles { [ sort map { $_->{doc}{id} } @_ ] }

subtest 'default text fields match exact words only' => sub {
    my $idx = Search::Tantivy->create( fields => [ id => 'string', body => 'text' ] );
    $idx->add( { id => 'a', body => 'She runs every morning' } )->commit;
    is titles( $idx->search('running') ), [], 'no stemming by default';
};

subtest 'english stemming' => sub {
    my $idx = Search::Tantivy->create( fields => [ id => 'string', body => 'text:en' ] );
    $idx->add( { id => 'a', body => 'She runs every morning' } );
    $idx->add( { id => 'b', body => 'Running shoes for sale' } );
    $idx->add( { id => 'c', body => 'A quiet evening' } );
    $idx->commit;
    is titles( $idx->search('run') ),     [ 'a', 'b' ], 'run finds runs and running';
    is titles( $idx->search('mornings') ), ['a'],        'plural query finds singular';
};

subtest 'polish stemming' => sub {
    my $idx = Search::Tantivy->create( fields => [ id => 'string', { name => 'body', type => 'text', language => 'polish' } ] );
    $idx->add( { id => 'a', body => 'Kupiłem nowe książki o programowaniu' } );
    $idx->add( { id => 'b', body => 'Żółwie pływają w stawie' } );
    $idx->commit;
    is titles( $idx->search('książka') ), ['a'], 'książka finds książki';
    is titles( $idx->search('zolwie') ),  [],    'no folding: diacritics required';

    my ($hit) = $idx->search( 'książka', snippets => ['body'] );
    like $hit->{snippets}{body}, qr{<b>książki</b>}, 'snippet highlights the inflected form';
};

subtest 'polish with ascii folding' => sub {
    my $idx = Search::Tantivy->create(
        fields => [ id => 'string', { name => 'body', type => 'text', language => 'pl', ascii_folding => 1 } ] );
    $idx->add( { id => 'a', body => 'Żółwie pływają w stawie' } )->commit;
    is titles( $idx->search('zolwie') ), ['a'], 'query without diacritics';
    is titles( $idx->search('Żółwie') ), ['a'], 'query with diacritics';
};

subtest 'folding without stemming' => sub {
    my $idx = Search::Tantivy->create( fields => [ id => 'string', { name => 'body', type => 'text', ascii_folding => 1 } ] );
    $idx->add( { id => 'a', body => 'Crème brûlée recipe' } )->commit;
    is titles( $idx->search('creme brulee') ), ['a'], 'accents folded';
};

subtest 'language survives reopen' => sub {
    my $dir = tempdir( CLEANUP => 1 );
    Search::Tantivy->create( path => $dir, fields => [ id => 'string', body => 'text:pl' ] )
      ->add( { id => 'a', body => 'Kupiłem nowe książki' } )->commit;
    is titles( Search::Tantivy->open($dir)->search('książka') ), ['a'], 'stemming after reopen';
};

subtest 'reload sees commits from another handle' => sub {
    my $dir    = tempdir( CLEANUP => 1 );
    my $writer = Search::Tantivy->create( path => $dir, fields => [ id => 'string', body => 'text' ] );
    $writer->add( { id => 'a', body => 'first' } )->commit;

    my $reader = Search::Tantivy->open($dir);
    is $reader->num_docs, 1, 'reader sees first commit';

    $writer->add( { id => 'b', body => 'second' } )->commit;
    is $reader->num_docs, 1, 'not visible before reload';
    is $reader->reload->num_docs, 2, 'visible after reload';
};

subtest 'option errors' => sub {
    like dies { Search::Tantivy->create( fields => [ body => 'text:klingon' ] ) }, qr/unsupported language 'klingon'.*pl \(polish\)/, 'unknown language lists supported';
    like dies { Search::Tantivy->create( fields => [ { name => 'n', type => 'u64', language => 'en' } ] ) }, qr/only apply to text fields/, 'language on numeric field';
    like dies { Search::Tantivy->create( fields => [ { name => 'b', type => 'text', langauge => 'en' } ] ) }, qr/unknown option\(s\): langauge/, 'typo in option name';
};

subtest 'stop words' => sub {
    my $idx = Search::Tantivy->create( fields => [
        id => 'u64',
        { name => 'en', type => 'text', language => 'en', stop_words => 1 },
        { name => 'pl', type => 'text', language => 'pl', stop_words => 1, ascii_folding => 1 },
    ] );
    $idx->add( { id => 1, en => 'The history of the camel', pl => 'Kot się nie boi psa' } );
    $idx->add( { id => 2, en => 'A camel in the desert',    pl => 'Pies i kot' } );
    $idx->commit;

    is $idx->count('en:the'), 0, 'English stop word is not indexed';
    is $idx->count( 'en:"history of the camel"' ), 1, 'phrases across stop words still match';
    is $idx->count('pl:sie'), 0, 'Polish stop word, also without diacritics';
    is $idx->count( 'kot', fields => ['pl'] ), 2, 'content words still indexed';

    like dies { Search::Tantivy->create( fields => [ { name => 'b', type => 'text', stop_words => 1 } ] ) },
      qr/stop_words needs a language/, 'needs a language';
    like dies { Search::Tantivy->create( fields => [ { name => 'b', type => 'text', language => 'ta', stop_words => 1 } ] ) },
      qr/no stop word list for language 'ta'/, 'language without a list';
};

done_testing;
