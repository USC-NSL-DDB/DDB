from _config import new_client


with new_client() as client:
    server, capabilities = client.handshake()
    response = client.get_snapshot(
        {
            "sections": [
                "SNAPSHOT_SECTION_TOPOLOGY",
                "SNAPSHOT_SECTION_SELECTION",
                "SNAPSHOT_SECTION_EXECUTION",
            ]
        }
    )
    print(server.get("version"), capabilities.get("schemaVersion"))
    for thread in response.get("snapshot", {}).get("threads", []):
        print(thread.get("threadId"), thread.get("state"), thread.get("location"))
