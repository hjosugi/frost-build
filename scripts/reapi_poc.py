#!/usr/bin/env python3
"""Run FrostBuild's bounded REAPI interoperability certificate.

The script intentionally depends on a REAPI client's generated Python modules
and grpcio instead of adding those packages to FrostBuild's runtime. It works
inside the official BuildGrid image and with conventional ``build.*`` generated
modules.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import stat
import sys
import time
from pathlib import Path

import grpc

try:
    from build.bazel.remote.execution.v2 import remote_execution_pb2 as repb
    from build.bazel.remote.execution.v2 import remote_execution_pb2_grpc as regrpc
except ModuleNotFoundError:
    from buildgrid._protos.build.bazel.remote.execution.v2 import (  # type: ignore[no-redef]
        remote_execution_pb2 as repb,
    )
    from buildgrid._protos.build.bazel.remote.execution.v2 import (  # type: ignore[no-redef]
        remote_execution_pb2_grpc as regrpc,
    )


def digest(data: bytes):
    return repb.Digest(hash=hashlib.sha256(data).hexdigest(), size_bytes=len(data))


def digest_key(value) -> str:
    return f"{value.hash}/{value.size_bytes}"


def serialize(message) -> tuple[object, bytes]:
    data = message.SerializeToString(deterministic=True)
    return digest(data), data


def collect_input_root(root: Path) -> tuple[object, dict[str, tuple[object, bytes]]]:
    blobs: dict[str, tuple[object, bytes]] = {}

    def add(data: bytes):
        value = digest(data)
        blobs[digest_key(value)] = (value, data)
        return value

    def visit(directory: Path):
        message = repb.Directory()
        for path in sorted(directory.iterdir(), key=lambda item: item.name.encode()):
            mode = path.lstat().st_mode
            if stat.S_ISLNK(mode):
                raise ValueError(f"the certificate rejects symlink input: {path}")
            if stat.S_ISREG(mode):
                message.files.add(
                    name=path.name,
                    digest=add(path.read_bytes()),
                    is_executable=bool(mode & stat.S_IXUSR),
                )
            elif stat.S_ISDIR(mode):
                message.directories.add(name=path.name, digest=visit(path))
            else:
                raise ValueError(f"unsupported input type: {path}")
        return add(message.SerializeToString(deterministic=True))

    return visit(root), blobs


def upload(cas, instance: str, blobs: list[tuple[object, bytes]], timeout: float) -> None:
    if not blobs:
        return
    response = cas.BatchUpdateBlobs(
        repb.BatchUpdateBlobsRequest(
            instance_name=instance,
            requests=[
                repb.BatchUpdateBlobsRequest.Request(digest=value, data=data)
                for value, data in blobs
            ],
        ),
        timeout=timeout,
    )
    failures = [
        f"{digest_key(item.digest)}: {item.status.code} {item.status.message}"
        for item in response.responses
        if item.status.code
    ]
    if failures:
        raise RuntimeError("BatchUpdateBlobs failed: " + "; ".join(failures))


def read_blob(cas, instance: str, value, timeout: float) -> bytes:
    response = cas.BatchReadBlobs(
        repb.BatchReadBlobsRequest(instance_name=instance, digests=[value]),
        timeout=timeout,
    )
    if len(response.responses) != 1:
        raise RuntimeError(f"missing BatchReadBlobs response for {digest_key(value)}")
    item = response.responses[0]
    if item.status.code:
        raise RuntimeError(
            f"BatchReadBlobs {digest_key(value)}: "
            f"{item.status.code} {item.status.message}"
        )
    if digest_key(digest(item.data)) != digest_key(value):
        raise RuntimeError(f"downloaded blob failed SHA-256: {digest_key(value)}")
    return item.data


def execute(execution, instance: str, action_digest, timeout: float, skip_cache: bool):
    started = time.perf_counter()
    last_operation = None
    try:
        for operation in execution.Execute(
            repb.ExecuteRequest(
                instance_name=instance,
                action_digest=action_digest,
                skip_cache_lookup=skip_cache,
            ),
            timeout=timeout,
        ):
            last_operation = operation
            if operation.done:
                break
    except grpc.RpcError as error:
        return {
            "ok": False,
            "grpc_code": error.code().name,
            "message": error.details() or "",
            "wall_ms": round((time.perf_counter() - started) * 1000, 3),
        }

    elapsed = round((time.perf_counter() - started) * 1000, 3)
    if last_operation is None or not last_operation.done:
        return {"ok": False, "message": "execution stream ended before done", "wall_ms": elapsed}
    if last_operation.error.code:
        return {
            "ok": False,
            "status_code": last_operation.error.code,
            "message": last_operation.error.message,
            "operation": last_operation.name,
            "wall_ms": elapsed,
        }

    response = repb.ExecuteResponse()
    if not last_operation.response.Unpack(response):
        return {
            "ok": False,
            "message": f"unexpected response type {last_operation.response.type_url}",
            "operation": last_operation.name,
            "wall_ms": elapsed,
        }
    if response.status.code:
        return {
            "ok": False,
            "status_code": response.status.code,
            "message": response.status.message,
            "operation": last_operation.name,
            "wall_ms": elapsed,
        }
    return {
        "ok": True,
        "operation": last_operation.name,
        "wall_ms": elapsed,
        "response": response,
    }


def output_file(cas, instance: str, result, timeout: float) -> tuple[object, bytes, object]:
    directories = {entry.path: entry.tree_digest for entry in result.output_directories}
    tree_digest = directories.get("result")
    if tree_digest is None:
        raise RuntimeError("ActionResult did not contain declared output directory 'result'")

    tree = repb.Tree()
    tree.ParseFromString(read_blob(cas, instance, tree_digest, timeout))
    by_digest = {
        digest_key(digest(item.SerializeToString(deterministic=True))): item
        for item in [tree.root, *tree.children]
    }
    current = tree.root
    for component in ["nested"]:
        node = next((item for item in current.directories if item.name == component), None)
        if node is None:
            raise RuntimeError(f"output Tree lacks directory {component!r}")
        current = by_digest.get(digest_key(node.digest))
        if current is None:
            raise RuntimeError(f"output Tree lacks child {digest_key(node.digest)}")
    node = next((item for item in current.files if item.name == "output.txt"), None)
    if node is None:
        raise RuntimeError("output Tree lacks nested/output.txt")
    return node.digest, read_blob(cas, instance, node.digest, timeout), tree_digest


def version_dict(value) -> dict[str, int]:
    return {
        "major": value.major,
        "minor": value.minor,
        "patch": value.patch,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--remote", default="http://localhost:50051")
    parser.add_argument("--instance", default="")
    parser.add_argument("--input-root", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=30.0)
    parser.add_argument(
        "--platform",
        action="append",
        default=[],
        metavar="KEY=VALUE",
        help="required REAPI platform property; repeat as needed",
    )
    args = parser.parse_args()

    input_root = args.input_root.resolve()
    if not input_root.is_dir():
        parser.error(f"--input-root is not a directory: {input_root}")
    properties = []
    for raw in args.platform:
        if "=" not in raw:
            parser.error(f"--platform must be KEY=VALUE: {raw!r}")
        name, value = raw.split("=", 1)
        if not name or not value:
            parser.error(f"--platform must be KEY=VALUE: {raw!r}")
        properties.append(repb.Platform.Property(name=name, value=value))
    properties.sort(key=lambda item: (item.name, item.value))

    authority = args.remote.removeprefix("http://").removeprefix("grpc://")
    if args.remote.startswith("https://"):
        parser.error("the certificate only supports a local plaintext endpoint")
    channel = grpc.insecure_channel(authority)
    cas = regrpc.ContentAddressableStorageStub(channel)
    execution = regrpc.ExecutionStub(channel)
    capabilities_client = regrpc.CapabilitiesStub(channel)

    capabilities = capabilities_client.GetCapabilities(
        repb.GetCapabilitiesRequest(instance_name=args.instance),
        timeout=args.timeout,
    )
    input_digest, blobs = collect_input_root(input_root)

    probe_data = f"missing-blob-probe-{time.time_ns()}".encode()
    probe_digest = digest(probe_data)
    root_bytes = blobs[digest_key(input_digest)][1]
    root = repb.Directory()
    root.ParseFromString(root_bytes)
    root.files.add(name=".frost-missing-probe", digest=probe_digest)
    root.files.sort(key=lambda item: item.name.encode())
    probe_root_digest, probe_root_bytes = serialize(root)
    blobs[digest_key(probe_root_digest)] = (probe_root_digest, probe_root_bytes)

    command = repb.Command(
        arguments=["./build.sh"],
        output_paths=["result"],
        output_directory_format=repb.Command.TREE_AND_DIRECTORY,
        platform=repb.Platform(properties=properties),
    )
    command_digest, command_bytes = serialize(command)
    action = repb.Action(
        command_digest=command_digest,
        input_root_digest=probe_root_digest,
        do_not_cache=False,
        platform=repb.Platform(properties=properties),
    )
    action_digest, action_bytes = serialize(action)
    blobs[digest_key(command_digest)] = (command_digest, command_bytes)
    blobs[digest_key(action_digest)] = (action_digest, action_bytes)

    upload(cas, args.instance, list(blobs.values()), args.timeout)
    missing = execute(execution, args.instance, action_digest, args.timeout, True)
    if missing["ok"]:
        raise RuntimeError("missing input blob was accepted by the executor")
    if "missing" not in str(missing.get("message", "")).casefold():
        raise RuntimeError(
            "executor failed the probe without identifying the missing blob: "
            f"{missing}"
        )
    upload(cas, args.instance, [(probe_digest, probe_data)], args.timeout)

    executed = execute(execution, args.instance, action_digest, args.timeout, True)
    if not executed["ok"]:
        raise RuntimeError(f"external execution failed after upload: {executed}")
    response = executed.pop("response")
    if response.result.exit_code:
        raise RuntimeError(f"external command exited {response.result.exit_code}")
    output_digest, output, tree_digest = output_file(
        cas, args.instance, response.result, args.timeout
    )
    expected = (input_root / "input.txt").read_text(encoding="utf-8").upper().encode()
    if output != expected:
        raise RuntimeError(f"output mismatch: expected {expected!r}, got {output!r}")

    cached = execute(execution, args.instance, action_digest, args.timeout, False)
    if not cached["ok"]:
        raise RuntimeError(f"action-cache lookup failed: {cached}")
    cached_response = cached.pop("response")
    if not cached_response.cached_result:
        raise RuntimeError("second Execute did not return cached_result=true")

    report = {
        "schema": "frost-reapi-poc-v1",
        "server": {
            "remote": args.remote,
            "low_api_version": version_dict(capabilities.low_api_version),
            "high_api_version": version_dict(capabilities.high_api_version),
            "digest_functions": [
                repb.DigestFunction.Value.Name(value)
                for value in capabilities.cache_capabilities.digest_functions
            ],
            "execution_enabled": capabilities.execution_capabilities.exec_enabled,
        },
        "translation": {
            "digest_function": "SHA256",
            "input_root": digest_key(probe_root_digest),
            "command": digest_key(command_digest),
            "action": digest_key(action_digest),
            "platform": {item.name: item.value for item in properties},
            "declared_output_directory": "result",
        },
        "missing_blob_probe": {
            key: value for key, value in missing.items() if key != "ok"
        },
        "execution": {
            **executed,
            "exit_code": response.result.exit_code,
            "cached_result": response.cached_result,
            "output_tree": digest_key(tree_digest),
            "output_file": digest_key(output_digest),
            "output_text": output.decode("utf-8"),
        },
        "action_cache": {
            **cached,
            "cached_result": cached_response.cached_result,
            "exit_code": cached_response.result.exit_code,
        },
    }
    print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as error:
        print(f"reapi-poc: {error}", file=sys.stderr)
        raise SystemExit(1)
