import { newClient } from "./config.js";

const client = newClient();
try {
  const snapshot = (await client.call("DebuggerService.GetSnapshot", {
    sections: ["SNAPSHOT_SECTION_TOPOLOGY", "SNAPSHOT_SECTION_EXECUTION"],
  })).snapshot;
  const thread = snapshot?.threads?.find((candidate) => candidate.state === "THREAD_STATE_STOPPED");
  if (!thread?.threadId) throw new Error("a stopped thread is required");
  const admitted = await client.call("DebuggerControlService.RunDistributedBacktrace", {
    target: { thread: { threadId: thread.threadId } },
    maxFrames: 64,
  });
  const operation = await client.waitOperation(admitted.operation?.operationId ?? "");
  for (const frame of operation.result?.distributedBacktrace?.frames ?? []) {
    console.log(frame.index, frame.sessionId, frame.frame?.functionName);
  }
} finally {
  client.close();
}
