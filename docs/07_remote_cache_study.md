# Remote Cache Study

Conclusion: keep the v1 Rust local CAS/action-cache layout close to Bazel REAPI,
but defer the remote wire protocol to v2.

REAPI compatibility requirements for v1:

- Action keys must be computed from a canonical command descriptor, environment
  whitelist, platform/toolchain closure, input digests, and declared outputs.
- CAS objects should be content addressed by digest and materialized separately
  from action-cache metadata.
- Action-cache entries should point from an action digest to output digests,
  exit code, timing metadata, and discovered dependency metadata.
- Writes must be temp-and-rename so a future remote uploader never observes a
  partial local object.
- Large local blobs are split with Bazel-bit-compatible FastCDC 2020 defaults
  (512 KiB average, 128 KiB minimum, 2 MiB maximum). Chunk files are SHA-256
  addressed and a blob manifest retains ordered lengths; reconstruction writes
  a private staging file and verifies the final BLAKE3+mode digest before one
  rename. This maps directly to REAPI SplitBlob/SpliceBlob without making the
  local action descriptor protobuf-dependent.
- A new version of the same output path may attach a level-19 zstd dictionary
  patch to a residual chunk, using the byte-range-overlapping chunk in the
  previous version as its positional base. Patches smaller than a normal
  level-3 compressed full chunk are retained. Restore order is exact blob,
  exact chunk, verified delta chunk, then miss/rebuild; base choice can only
  affect cost because patch, reconstructed chunk and final blob are verified.

Current gap:

- Frost uses canonical BLAKE3 descriptors instead of REAPI protobuf messages.
- The local action result is a binary journal record, not REAPI ActionResult.
- There is no ByteStream service, batch update API, compressor negotiation, or
  remote execution platform properties.
- The local positional delta path is not a claim of remote-cache speed: CPU vs
  bandwidth calibration and protocol negotiation still require external
  measurements.

Decision:

- Adopt the REAPI separation of ActionCache and ContentAddressableStorage in
  the local data model.
- Defer wire compatibility until the Rust engine freezes its v1 action schema.
- File a v2 requirement before remote work: freeze a protobuf-compatible action
  descriptor and output tree format so local action keys can be translated
  without rebuilding the cache.

## Implemented client (v0.5.0)

`--remote-cache=<endpoint>` consults a shared cache when the local journal
misses, and `--remote-upload` publishes what the build produced. Two backends
answer the same semantics: a directory (a shared volume, NFS/SMB, or a
bind-mounted CI cache) and plain HTTP `GET`/`PUT` on `<prefix>/{ac,cas}/<name>`.

The lookup key is the action key over **declared** inputs only. frost discovers
some inputs by running the action, so a cold workspace cannot compute the full
input set in advance; the entry therefore records the inputs the producing run
discovered with their digests, and a consumer accepts it only when every one of
those paths currently holds the recorded digest. That makes an entry a
constructive trace rather than a guess, and it is what allows a workspace with no
journal to reuse a compile whose real inputs include headers it has never read.

Soundness follows the local rules exactly:

- a response is verified against the digest that was asked for, and the mode is
  recovered from the digest — frost digests cover the executable bit, so the
  matching mode is the mode, and a blob matching neither is corrupt
- verified bytes are published through the ordinary `LocalCas` boundary, so
  restoration, chunking and GC behave as they do for locally produced output
- an entry whose recorded output set is not exactly what the action declares is
  refused, as the local journal refuses it (#64)
- every failure — miss, unreadable entry, unverifiable blob, unreachable
  endpoint, timeout — falls back to executing the action. The build cannot fail
  because of the remote cache, and the per-build summary line reports hits,
  misses, bytes moved, rejections and errors so a silently failing cache does not
  look like a cold one
- names are restricted to hex/underscore tokens, so a hostile entry cannot
  address a path outside the cache

Measured on the bundled C sample: a producing build published 5 actions
(19.93 KiB), and a second workspace with no local cache built entirely from the
shared cache in 2 ms with 5 hits and nothing executed.

Deliberately not implemented yet:

- REAPI protobuf/gRPC, ByteStream, `FindMissingBlobs` batching and compressor
  negotiation. The layout is digest-addressed so it translates, but the wire
  format is not REAPI yet
- HTTPS. Rather than pretend to verify a certificate, `https://` is refused with
  the suggestion to terminate TLS locally
- chunk-level transfer (#82). Whole blobs move today; the chunk layer belongs on
  top of a calibrated cost model rather than under an uncalibrated one
- remote execution (#64), which needs this data plane first

