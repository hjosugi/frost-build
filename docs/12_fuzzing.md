# Fuzzing

Nightly CI runs the manifest, depfile, journal and graph-store targets. Reproduce
with `cargo fuzz run TARGET`. On a crash, run `cargo fuzz tmin TARGET artifact`,
add the minimized bytes/scenario as a deterministic regression test, fix the
decoder, then retain the case in `fuzz/corpus/TARGET/`. Corrupt persistent state
must safely miss/recompile; it must never synthesize a cache hit.

Two formats reached in 0.3.x are guarded by property tests in the crates that own
them rather than by a libfuzzer target, because both are read through a
filesystem path rather than from a byte slice, and neither decoder is public:

- the CAS chunk manifest: truncations, single-byte flips at a stride across the
  whole encoding, and unrelated bytes must all fail materialization and leave the
  destination path untouched
- the no-op certificate: the same corruption families must always miss, because
  this record is what allows a build to be skipped entirely

These run on every pull request, unlike the nightly targets. The designed
failure injections for the same paths — bit flips, missing/wrong/truncated
chunks, reordering, parameter mismatch, and the Bazel #29544 final-path
scenario — remain a required CI job (#84). A libfuzzer target for the two
formats above is still worth adding once their decoders are reachable from a
byte slice without widening the public API (#111).
