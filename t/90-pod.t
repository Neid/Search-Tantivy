use strict;
use warnings;
use Test2::V0;

# POD syntax and internal links (L</...> must name a real heading), with the
# core Pod::Checker, so it runs everywhere.
use Pod::Checker ();

for my $file ('lib/Search/Tantivy.pm') {
    my $report  = '';
    open my $out, '>', \$report or die;
    my $checker = Pod::Checker->new( -warnings => 1 );
    $checker->parse_from_file( $file, $out );
    is $checker->num_errors,   0, "$file: no POD errors"   or diag $report;
    is $checker->num_warnings, 0, "$file: no POD warnings" or diag $report;
}

done_testing;
