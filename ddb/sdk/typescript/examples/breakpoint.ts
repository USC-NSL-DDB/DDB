import { newClient } from "./config.js";

const client = newClient();
try {
  const snapshot = (await client.call("DebuggerService.GetSnapshot", {
    sections: ["SNAPSHOT_SECTION_TOPOLOGY", "SNAPSHOT_SECTION_EXECUTION"],
  })).snapshot;
  const thread = snapshot?.threads?.find((candidate) => candidate.state === "THREAD_STATE_STOPPED");
  if (!thread?.sessionId || !thread.location?.path || !thread.location.line) {
    throw new Error("a stopped source-backed thread is required");
  }
  const admitted = await client.call("DebuggerControlService.CreateBreakpoint", {
    target: { session: { sessionId: thread.sessionId } },
    breakpoint: {
      source: { source: thread.location.path, line: thread.location.line },
      enabled: true,
    },
  });
  const created = await client.waitOperation(admitted.operation?.operationId ?? "");
  const breakpointId = created.result?.breakpoint?.breakpointId;
  if (!breakpointId) throw new Error("DDB omitted the breakpoint result");
  console.log("created", breakpointId);

  const deleted = await client.call("DebuggerControlService.DeleteBreakpoint", {
    breakpointId,
    target: { session: { sessionId: thread.sessionId } },
  });
  await client.waitOperation(deleted.operation?.operationId ?? "");
} finally {
  client.close();
}
