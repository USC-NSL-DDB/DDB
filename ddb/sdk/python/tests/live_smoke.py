from __future__ import annotations

import json
import sys

from ddb_api import DdbClient


def operation_id(admission: dict) -> str:
    value = admission.get("operation", {}).get("operationId")
    if not value:
        raise RuntimeError("operation admission omitted operationId")
    return value


def main() -> None:
    if len(sys.argv) != 3:
        raise RuntimeError("usage: live_smoke.py ENDPOINT CONTROL_TOKEN")
    with DdbClient(sys.argv[1], bearer_token=sys.argv[2]) as client:
        server, capabilities = client.handshake()
        if capabilities.get("apiVersion") != "v2":
            raise RuntimeError("v2 was not negotiated")
        sessions = client.collect(
            "DebuggerService.ListSessions", {"page": {"pageSize": 1}}, max_items=16
        )
        if len(sessions) != 1:
            raise RuntimeError(f"expected one session, got {len(sessions)}")
        snapshot = client.get_snapshot(
            {"sections": ["SNAPSHOT_SECTION_TOPOLOGY", "SNAPSHOT_SECTION_EXECUTION"]}
        ).get("snapshot", {})
        thread = next(
            (
                candidate
                for candidate in snapshot.get("threads", [])
                if candidate.get("state") == "THREAD_STATE_STOPPED"
            ),
            None,
        )
        if not thread or not thread.get("threadId") or not thread.get("sessionId"):
            raise RuntimeError("no stopped thread")
        frames = client.collect(
            "DebuggerService.ListFrames",
            {"threadId": thread["threadId"], "page": {"pageSize": 1}},
            max_items=32,
        )
        location = frames[0].get("location", {}) if frames else {}
        if not location.get("path") or not location.get("line"):
            raise RuntimeError("frame omitted source location")

        created = client.wait_operation(
            operation_id(
                client.create_breakpoint(
                    {
                        "target": {"session": {"sessionId": thread["sessionId"]}},
                        "breakpoint": {
                            "source": {
                                "source": location["path"],
                                "line": location["line"],
                            },
                            "enabled": True,
                            "temporary": True,
                        },
                    }
                )
            )
        )
        breakpoint_id = created.get("result", {}).get("breakpoint", {}).get("breakpointId")
        if not breakpoint_id:
            raise RuntimeError("breakpoint result was omitted")
        client.wait_operation(
            operation_id(
                client.delete_breakpoint(
                    {
                        "breakpointId": breakpoint_id,
                        "target": {"session": {"sessionId": thread["sessionId"]}},
                    }
                )
            )
        )

        backtrace = client.wait_operation(
            operation_id(
                client.run_distributed_backtrace(
                    {
                        "target": {"thread": {"threadId": thread["threadId"]}},
                        "maxFrames": 32,
                    }
                )
            )
        )
        if not backtrace.get("result", {}).get("distributedBacktrace", {}).get("frames"):
            raise RuntimeError("distributed backtrace result was empty")
        print(
            json.dumps(
                {
                    "language": "python",
                    "serverInstanceId": server.get("serverInstanceId"),
                    "sessions": len(sessions),
                    "frames": len(frames),
                }
            )
        )


if __name__ == "__main__":
    main()
