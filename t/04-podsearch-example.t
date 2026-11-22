use strict;
use warnings;
use Test2::V0;
use File::Spec;
use File::Temp qw(tempdir);

# Smoke test for examples/podsearch: index @INC (which includes this
# distribution's lib/, so Search::Tantivy's own POD is in there) and search it.
my $script = File::Spec->catfile( 'examples', 'podsearch' );
my $dir    = File::Spec->catdir( tempdir( CLEANUP => 1 ), 'idx' );
my @perl   = ( $^X, map { "-I$_" } grep { !ref } @INC );

my $out = qx{"$perl[0]" @perl[1..$#perl] $script --dir "$dir" --index 2>&1};
is $?, 0, 'indexing exits 0' or diag $out;
like $out, qr/Indexed \d+ modules, \d+ sections/, 'reports what it indexed';

$out = qx{"$perl[0]" @perl[1..$#perl] $script --dir "$dir" -n 3 "tantivy full-text" 2>&1};
is $?, 0, 'search exits 0' or diag $out;
like $out, qr/Search::Tantivy/, 'finds its own documentation';

$out = qx{"$perl[0]" @perl[1..$#perl] $script --dir "$dir" -n 1 "Search::Tantivy" 2>&1};
like $out, qr/\A 1\. Search::Tantivy - /, 'a module name finds the module first';

$out = qx{"$perl[0]" @perl[1..$#perl] $script --dir "$dir" -n 3 "sorting newest first" 2>&1};
like $out, qr/§ Sorting/, 'finds the right section of a page';

done_testing;
