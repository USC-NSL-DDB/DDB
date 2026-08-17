import { newClient } from "./config.js";

const client = newClient();
try {
  for await (const update of client.stateSync()) {
    if (update.type === "snapshot") {
      console.log("hydrated", update.snapshot.stateEventCursor);
    } else {
      console.log("event", update.event.kind, update.event.resourceId);
    }
  }
} finally {
  client.close();
}
