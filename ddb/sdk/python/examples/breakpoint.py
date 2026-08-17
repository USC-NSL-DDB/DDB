from _config import new_client


with new_client() as client:
    snapshot = client.get_snapshot(
        {"sections": ["SNAPSHOT_SECTION_TOPOLOGY", "SNAPSHOT_SECTION_EXECUTION"]}
    ).get("snapshot", {})
    thread = next(
        (item for item in snapshot.get("threads", []) if item.get("state") == "THREAD_STATE_STOPPED"),
        None,
    )
    location = thread.get("location", {}) if thread else {}
    if not thread or not thread.get("sessionId") or not location.get("path") or not location.get("line"):
        raise RuntimeError("a stopped source-backed thread is required")
    admitted = client.create_breakpoint(
        {
            "target": {"session": {"sessionId": thread["sessionId"]}},
            "breakpoint": {
                "source": {"source": location["path"], "line": location["line"]},
                "enabled": True,
            },
        }
    )
    operation = client.wait_operation(admitted.get("operation", {}).get("operationId", ""))
    breakpoint_id = operation.get("result", {}).get("breakpoint", {}).get("breakpointId")
    if not breakpoint_id:
        raise RuntimeError("DDB omitted the breakpoint result")
    print("created", breakpoint_id)
    deleted = client.delete_breakpoint(
        {
            "breakpointId": breakpoint_id,
            "target": {"session": {"sessionId": thread["sessionId"]}},
        }
    )
    client.wait_operation(deleted.get("operation", {}).get("operationId", ""))
