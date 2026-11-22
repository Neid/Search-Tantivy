use strict;
use warnings;
use Test2::V0;

use File::Temp ();
use Search::Tantivy;

skip_all 'fork() is emulated with threads on Windows' if $^O eq 'MSWin32';

my $dir = File::Temp->newdir;
my $idx = Search::Tantivy->create( path => "$dir", fields => [ id => 'u64', body => 'text' ] );
$idx->add( { id => 1, body => 'before the fork' } )->commit;    # the parent now has a writer

pipe my $r, my $w or die "pipe: $!";
my $pid = fork // die "fork: $!";
if ( !$pid ) {
    close $r;
    my @report;
    my @hits = eval { $idx->search('fork') };
    push @report, 'search=' . scalar(@hits);
    eval { $idx->add( { id => 2, body => 'from the child' } ) };
    push @report, 'add=' . ( $@ =~ /before a fork/ ? 'refused' : "unexpected: $@" );
    print {$w} join( ',', @report ), "\n";
    close $w;
    exit 0;    # runs DESTROY on the inherited handle: must not hang
}
close $w;

my ( $line, $status );
eval {
    local $SIG{ALRM} = sub { die "timeout\n" };
    alarm 30;
    $line = <$r>;
    waitpid $pid, 0;
    $status = $?;
    alarm 0;
    1;
} or do { kill 'KILL', $pid; waitpid $pid, 0; fail "child hung: $@"; done_testing; exit };

chomp( $line //= '' );
is $line,   'search=1,add=refused', 'child can search, writing is refused';
is $status, 0, 'child exited cleanly';

$idx->add( { id => 3, body => 'after the fork' } )->commit;
is $idx->num_docs, 2, 'parent still writes (lock kept)';

my $other = Search::Tantivy->open("$dir");
like dies { $other->add( { id => 4, body => 'second writer' } ) }, qr/lock|busy/i, 'write lock still held by the parent';

done_testing;
