# Remote execution study

Decision: remote execution remains a v2 feature, and the v1 action/CAS model is
REAPI-translatable without a cache-format migration. The external
interoperability gate was completed on 28 July 2026 against BuildGrid 0.8.4 and
a BuildBox worker.

The checked certificate is
[`2026-07-28-buildgrid-reapi-poc.json`](../bench/baselines/2026-07-28-buildgrid-reapi-poc.json).
[`scripts/reapi_poc.py`](../scripts/reapi_poc.py) is the reusable client and
[`tests/fixtures/reapi`](../tests/fixtures/reapi) is its synthetic action input.

## Model translation

| Frost boundary | REAPI representation | Status |
|---|---|---|
| canonical direct argv, environment and working directory | `Command` | v1 canonicalization exists; adapter required |
| profile, platform and toolchain identity | `Command.platform` / executor platform | platform round trip proved |
| sorted path/digest input trace | Merkle `Directory` input root | recursive SHA-256 encoder proved |
| declared files and owned output directories | `output_paths` + `Tree` / `Directory` | nested output tree proved |
| immutable CAS objects and action records | CAS + Action Cache | upload, execution and cache hit proved |
| local BLAKE3 blob identity | REAPI digest function, normally SHA-256 | dual-digest adapter required |

Frost's constructive dependency trace is the largest semantic difference.
Frost can discover compiler inputs during a local execution, whereas REAPI
requires the complete Merkle input root before `Execute`. A remote adapter must
therefore use the last verified trace, preflight it against the current
workspace, and fall back to local discovery whenever evidence is absent or
stale. A guessed input set is never sufficient.

## External executor certificate

The experiment used a clean checkout of BuildGrid 0.8.4 at commit
`ed56c818e162ee5af82f470b09a9bff35816ec5d`, its PostgreSQL/Redis controller,
and the BuildBox image whose pulled digest was
`sha256:14d5c8129023a687d4f0093eb92d3d5ff03b76b84942910b66456549ab5cf295`.
Podman 6.0.1 provided the local container network. BuildGrid was selected as
the external OSS executor for this bounded proof; running a second
implementation such as BuildBarn would add vendor breadth but is not required
to establish that the mapping works.

Run the client in an environment containing generated REAPI Python modules and
`grpcio` (the official BuildGrid image contains both):

```bash
python scripts/reapi_poc.py \
  --remote http://controller:50051 \
  --input-root tests/fixtures/reapi \
  --timeout 45 \
  --platform OSFamily=linux \
  --platform ISA=x86-64
```

The script performs and asserts all of the following:

1. Query `GetCapabilities`; the server reported REAPI 2.0–2.2, SHA-256 and
   execution enabled.
2. Encode the fixture recursively as deterministic `Directory` messages and
   upload its blobs, `Command` and `Action` with `BatchUpdateBlobs`.
3. Deliberately omit one unique input blob. Execution must fail and identify a
   missing blob; accepting the action or returning an unrelated error fails the
   certificate.
4. Upload that blob and execute `./build.sh` on a worker constrained by
   `OSFamily=linux, ISA=x86-64`.
5. Download and digest-check the returned `Tree`, traverse
   `result/nested/output.txt`, and compare its exact contents.
6. Execute the same action with cache lookup enabled and require
   `cached_result=true`.

The measured successful execution was 1487.287 ms and the Action Cache return
was 23.877 ms on the loopback container network. The output `Tree` and file
digests in the certificate are independently reproducible from the fixture.

## Interoperability finding

BuildBox retries the deliberately absent blob and ultimately exposes it through
the operation as status code 13 (`INTERNAL`), even though its message precisely
identifies the missing digest. A Frost client must not couple recovery to one
vendor's gRPC status value. It should use `FindMissingBlobs` before execution,
validate the missing-digest evidence, upload the named objects, and bound any
retry.

The proof uses `TREE_AND_DIRECTORY` and `output_paths`. Legacy
`output_directories` alone did not make this BuildBox configuration capture the
tree, which is another reason for the adapter to negotiate capabilities rather
than infer behavior from a server name.

## v1 requirements and v2 gaps

The v1 compatibility requirements are already enforced:

- immutable digest objects are separate from action results;
- relative paths, argv and environment are canonical;
- platform, profile, toolchain closure and declared outputs participate in
  action identity;
- restoration requires the recorded output set to match exactly;
- output publication is digest-verified and atomic;
- journals are crash-safe and sandbox failures remain diagnostic.

No new v1 corrective issue is required by this experiment. The remaining work
is isolated to the v2 adapter:

- protobuf/gRPC and ByteStream clients with capability negotiation;
- verified trace to SHA-256 Merkle-tree conversion and missing-blob preflight;
- remote toolchain packaging plus executor platform selection;
- `ActionResult` file, directory, symlink, mode and stdout/stderr translation;
- cancellation, deadline and retry policy;
- remote sandbox equivalence and a local fallback on every transport or
  evidence failure.

Remote execution can now be planned as an adapter over proven boundaries, not
as a reason to weaken or redesign v1 correctness.
