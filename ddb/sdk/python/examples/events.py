from _config import new_client


with new_client() as client:
    for kind, update in client.state_sync():
        if kind == "snapshot":
            print("hydrated", update.get("stateEventCursor"))
        else:
            print("event", update.get("kind"), update.get("resourceId"))
