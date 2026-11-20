use strict;
use warnings;

use Config;
use Digest::SHA;
use ExtUtils::MakeMaker;
use File::Path ();
use File::Spec;
use HTTP::Tiny;
use POSIX ();

# How the Rust library gets into place, set with SEARCH_TANTIVY_BUILD:
#   auto    (default) prebuilt binary if one exists for this platform, else cargo
#   binary  prebuilt binary only, fail otherwise
#   source  always build with cargo
my $MODE = lc( $ENV{SEARCH_TANTIVY_BUILD} // 'auto' );
$MODE =~ /\A(?:auto|binary|source)\z/
  or die "SEARCH_TANTIVY_BUILD must be auto, binary or source (got '$MODE')\n";

my $VERSION   = MM->parse_version('lib/Search/Tantivy.pm');
my $RELEASES  = $ENV{SEARCH_TANTIVY_RELEASES_URL}    # mirrors/testing; checksums still apply
  // 'https://github.com/Neid/Search-Tantivy/releases/download';
my $CHECKSUMS = 'prebuilt.sha256';    # added to the CPAN tarball by the release workflow
my $PREBUILT  = 'prebuilt';

my ( $LIBNAME, $EXT ) =
    $^O eq 'MSWin32' ? ( 'search_tantivy.dll',      'dll' )
  : $^O eq 'darwin'  ? ( 'libsearch_tantivy.dylib', 'dylib' )
  :                    ( 'libsearch_tantivy.so',    'so' );

my ( $lib_source, $build_rust );

if ( $MODE ne 'source' ) {
    $lib_source = fetch_prebuilt();
    die "SEARCH_TANTIVY_BUILD=binary but no prebuilt library could be installed\n"
      if !$lib_source && $MODE eq 'binary';
}

if ( !$lib_source ) {
    if ( !have_cargo() ) {
        # "OS unsupported" makes CPAN Testers file an NA report instead of FAIL.
        print "OS unsupported: no usable prebuilt Search::Tantivy library for this platform "
          . "and no Rust toolchain to build one. Install Rust from https://rustup.rs and retry.\n";
        exit 0;
    }
    print "Building the Rust library from source with cargo.\n";
    $lib_source = "rust/target/release/$LIBNAME";
    $build_rust = 1;
}

WriteMakefile(
    NAME               => 'Search::Tantivy',
    AUTHOR             => 'Piotr Jackowski',
    VERSION_FROM       => 'lib/Search/Tantivy.pm',
    ABSTRACT_FROM      => 'lib/Search/Tantivy.pm',
    LICENSE            => 'perl_5',
    MIN_PERL_VERSION   => '5.016',
    CONFIGURE_REQUIRES => { 'ExtUtils::MakeMaker' => '6.64', 'Digest::SHA' => 0, 'HTTP::Tiny' => 0 },
    PREREQ_PM          => { 'FFI::Platypus' => '2.00', 'JSON::PP' => 0 },
    TEST_REQUIRES      => { 'Test2::V0' => 0 },
    clean              => { FILES => "rust/target $PREBUILT" },
    META_MERGE         => {
        'meta-spec' => { version => 2 },
        dynamic_config => 1,
        resources      => {
            repository => {
                type => 'git',
                url  => 'https://github.com/Neid/Search-Tantivy.git',
                web  => 'https://github.com/Neid/Search-Tantivy',
            },
            bugtracker => { web => 'https://github.com/Neid/Search-Tantivy/issues' },
        },
    },
);

# Put the library next to the module's arch files, where
# Search::Tantivy::_find_lib looks for it (auto/Search/Tantivy/).
sub MY::postamble {
    my $cargo = $build_rust ? "\tcargo build --release --manifest-path rust/Cargo.toml\n" : '';
    return <<"MAKE";
pure_all :: search_tantivy_lib

search_tantivy_lib :
$cargo\t\$(MKPATH) \$(INST_ARCHAUTODIR)
\t\$(CP) $lib_source \$(INST_ARCHAUTODIR)/$LIBNAME

.PHONY : search_tantivy_lib
MAKE
}

# ---------------------------------------------------------------------------

sub have_cargo {
    my $null = File::Spec->devnull;
    return system("cargo --version >$null 2>&1") == 0;
}

# Rust target triple of this perl, if we publish a binary for it.
sub target_triple {
    return undef unless $Config{ptrsize} == 8;
    my $machine = ( POSIX::uname() )[4] // '';

    if ( $^O eq 'linux' ) {
        my ($arch) = $Config{archname} =~ /\A(x86_64|aarch64)/;
        $arch //= $machine =~ /\A(x86_64|aarch64)\z/ ? $1 : return undef;
        my $libc = glob('/lib/ld-musl-*.so.1') ? 'musl' : 'gnu';
        return "$arch-unknown-linux-$libc";
    }
    if ( $^O eq 'darwin' ) {
        return 'aarch64-apple-darwin' if $machine eq 'arm64';
        return 'x86_64-apple-darwin'  if $machine eq 'x86_64';
        return undef;
    }
    if ( $^O eq 'MSWin32' ) {
        return $Config{archname} =~ /x64|x86_64/ ? 'x86_64-pc-windows-msvc' : undef;
    }
    return undef;
}

sub expected_sha256 {
    my ($asset) = @_;
    open my $fh, '<', $CHECKSUMS or return undef;
    while (<$fh>) {
        my ( $sum, $name ) = split;
        return lc $sum if defined $name && $name eq $asset;
    }
    return undef;
}

sub download {
    my ( $url, $dest ) = @_;
    my $http = HTTP::Tiny->new( timeout => 120, agent => "Search-Tantivy-Makefile.PL/$VERSION" );
    if ( $http->can_ssl ) {
        my $res = $http->mirror( $url, $dest );
        return 1 if $res->{success};
        warn "  download failed: $res->{status} $res->{reason}\n";
        return 0;
    }
    # HTTPS needs IO::Socket::SSL + Net::SSLeay, which are not in core; try the usual tools.
    my $null = File::Spec->devnull;
    for my $cmd ( [ 'curl', '-fsSL', '-o', $dest, $url ], [ 'wget', '-q', '-O', $dest, $url ] ) {
        return 1 if system("$cmd->[0] --version >$null 2>&1") == 0 && system(@$cmd) == 0;
    }
    warn "  download failed: no HTTPS support (install IO::Socket::SSL, curl or wget)\n";
    return 0;
}

sub fetch_prebuilt {
    -f $CHECKSUMS or return undef;    # git checkout: build from source
    my $target = target_triple() or do {
        print "No prebuilt library for this platform ($^O, $Config{archname}).\n";
        return undef;
    };
    my $asset    = "search_tantivy-$target.$EXT";
    my $expected = expected_sha256($asset) or do {
        print "No prebuilt library published for $target.\n";
        return undef;
    };

    File::Path::make_path($PREBUILT);
    my $dest = File::Spec->catfile( $PREBUILT, $LIBNAME );
    my $url  = "$RELEASES/v$VERSION/$asset";
    print "Downloading prebuilt library $url\n";
    download( $url, $dest ) or return undef;

    my $got = Digest::SHA->new(256)->addfile( $dest, 'b' )->hexdigest;
    if ( $got ne $expected ) {
        unlink $dest;
        warn "  checksum mismatch for $asset (expected $expected, got $got); not using it\n";
        return undef;
    }
    print "  checksum OK\n";
    return "$PREBUILT/$LIBNAME";
}
