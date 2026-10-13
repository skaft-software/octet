"""Fault injection around the actual SDK bulk helper and real transfer files."""
from dataclasses import dataclass
import hashlib

from bulk_fixture import LOG, Measured, Publish, Published, Read, ext, log, publish
from octet_extension import RpcError


@dataclass
class Fault:
    mode: str


@ext.typed_tool(name="fault_publish", description="Fault an SDK bulk transfer", summary=lambda value: "Never admitted.")
def fault_publish(args: Fault) -> Published:
    if args.mode not in {"short", "declared_short", "long", "digest", "abandon", "cancel"}:
        raise ValueError("unknown fixture fault")
    original = ext.request
    grant = None

    def request(method, params=None, **kwargs):
        nonlocal grant
        if method == "bulk/commit":
            log("commit_ready", mode=args.mode)
            if args.mode in {"short", "long"}:
                # Corrupt the producer's real scratch AFTER the SDK wrote it,
                # before the production host performs its verified snapshot.
                with ext.bulk._file(grant["locator"], write=True) as stream:
                    if args.mode == "short":
                        stream.truncate(63)
                    else:
                        stream.seek(64)
                        stream.write(b"x")
            elif args.mode == "declared_short":
                params = {**params, "bytes": 63}
            elif args.mode == "digest":
                params = {**params, "digest": {"algorithm": "sha256", "value": "0" * 64}}
            elif args.mode == "abandon":
                raise RpcError(-32000, "fixture_abandon")
            elif args.mode == "cancel":
                ext.cancellation.wait(3)
                ext.cancellation.raise_if_cancelled()
                raise RuntimeError("missing fixture cancellation")
        try:
            result = original(method, params, **kwargs)
        except RpcError as error:
            if method == "bulk/commit":
                log("commit_refused", code=error.code, message=error.message, data=error.data)
            raise
        if method == "bulk/write":
            grant = result
            log("ticket_created", capacity=result["capacity"])
        return result

    ext.request = request
    try:
        return publish(Publish(64))
    finally:
        ext.request = original
        log("fault_settled", mode=args.mode)


@ext.typed_tool(name="measure_small", description="Read with finite fixture bound", summary=lambda value: "Measured bytes.")
def measure_small(args: Read) -> Measured:
    with ext.bulk.read(args.data, max_bytes=64) as stream:
        data = stream.read()
    log("read_closed", closed=stream.closed)
    return Measured(len(data), hashlib.sha256(data).hexdigest())


if __name__ == "__main__":
    log("started")
    ext.run()
