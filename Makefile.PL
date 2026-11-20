use strict;
use warnings;
use ExtUtils::MakeMaker;

my $lib =
    $^O eq 'MSWin32' ? 'search_tantivy.dll'
  : $^O eq 'darwin'  ? 'libsearch_tantivy.dylib'
  :                    'libsearch_tantivy.so';

system( 'cargo', '--version' ) == 0
  or die "Search::Tantivy needs a Rust toolchain (cargo) to build: https://rustup.rs\n";

WriteMakefile(
    NAME             => 'Search::Tantivy',
    AUTHOR           => 'Piotr Jackowski',
    VERSION_FROM     => 'lib/Search/Tantivy.pm',
    ABSTRACT_FROM    => 'lib/Search/Tantivy.pm',
    LICENSE          => 'perl_5',
    MIN_PERL_VERSION => '5.016',
    PREREQ_PM        => { 'FFI::Platypus' => '2.00', 'JSON::PP' => 0 },
    TEST_REQUIRES    => { 'Test2::V0' => 0 },
    clean            => { FILES => 'rust/target' },
    META_MERGE       => {
        'meta-spec' => { version => 2 },
        resources   => {
            repository => {
                type => 'git',
                url  => 'https://github.com/Neid/Search-Tantivy.git',
                web  => 'https://github.com/Neid/Search-Tantivy',
            },
        },
    },
);

# Build the Rust cdylib and install it next to the module's arch files,
# where Search::Tantivy::_find_lib looks for it (auto/Search/Tantivy/).
sub MY::postamble {
    return <<"MAKE";
pure_all :: rust_lib

rust_lib :
\tcargo build --release --manifest-path rust/Cargo.toml
\t\$(MKPATH) \$(INST_ARCHAUTODIR)
\t\$(CP) rust/target/release/$lib \$(INST_ARCHAUTODIR)/$lib

.PHONY : rust_lib
MAKE
}
