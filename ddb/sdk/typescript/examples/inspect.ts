import { newClient } from "./config.js";

const client = newClient();
try {
  const { serverInfo, capabilities } = await client.handshake();
  const response = await client.call("DebuggerService.GetSnapshot", {
    sections: [
      "SNAPSHOT_SECTION_TOPOLOGY",
      "SNAPSHOT_SECTION_SELECTION",
      "SNAPSHOT_SECTION_EXECUTION",
    ],
  });
  console.log(serverInfo.version, capabilities.schemaVersion);
  for (const thread of response.snapshot?.threads ?? []) {
    console.log(thread.threadId, thread.state, thread.location);
  }
} finally {
  client.close();
}
