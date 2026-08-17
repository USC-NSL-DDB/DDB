from _config import new_client


with new_client() as client:
    snapshot = client.get_snapshot(
        {"sections": ["SNAPSHOT_SECTION_TOPOLOGY", "SNAPSHOT_SECTION_EXECUTION"]}
    ).get("snapshot", {})
    thread = next(
        (item for item in snapshot.get("threads", []) if item.get("state") == "THREAD_STATE_STOPPED"),
        None,
    )
    if not thread or not thread.get("threadId"):
        raise RuntimeError("a stopped thread is required")
    admitted = client.run_distributed_backtrace(
        {
            "target": {"thread": {"threadId": thread["threadId"]}},
            "maxFrames": 64,
        }
    )
    operation = client.wait_operation(admitted.get("operation", {}).get("operationId", ""))
    frames = operation.get("result", {}).get("distributedBacktrace", {}).get("frames", [])
    for frame in frames:
        print(frame.get("index"), frame.get("sessionId"), frame.get("frame", {}).get("functionName"))
