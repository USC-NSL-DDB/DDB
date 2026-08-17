import { DdbClient } from "../dist/index.js";

const [endpoint, token] = process.argv.slice(2);
if (!endpoint || !token) throw new Error("usage: live-smoke.mjs ENDPOINT CONTROL_TOKEN");

const client = new DdbClient({ endpoint, bearerToken: token });
try {
  const { serverInfo, capabilities } = await client.handshake();
  if (capabilities.apiVersion !== "v2") throw new Error("v2 was not negotiated");
  const sessions = await client.collect(
    "DebuggerService.ListSessions",
    { page: { pageSize: 1 } },
    16,
  );
  if (sessions.length !== 1) throw new Error(`expected one session, got ${sessions.length}`);

  const snapshot = (await client.call("DebuggerService.GetSnapshot", {
    sections: ["SNAPSHOT_SECTION_TOPOLOGY", "SNAPSHOT_SECTION_EXECUTION"],
  })).snapshot;
  const thread = snapshot?.threads?.find((candidate) => candidate.state === "THREAD_STATE_STOPPED");
  if (!thread?.threadId || !thread.sessionId) throw new Error("no stopped thread");
  const frames = await client.collect(
    "DebuggerService.ListFrames",
    { threadId: thread.threadId, page: { pageSize: 1 } },
    32,
  );
  const location = frames[0]?.location;
  if (!location?.path || !location.line) throw new Error("frame omitted source location");

  const createdAdmission = await client.call("DebuggerControlService.CreateBreakpoint", {
    target: { session: { sessionId: thread.sessionId } },
    breakpoint: {
      source: { source: location.path, line: location.line },
      enabled: true,
      temporary: true,
    },
  });
  const created = await client.waitOperation(createdAdmission.operation?.operationId ?? "");
  const breakpointId = created.result?.breakpoint?.breakpointId;
  if (!breakpointId) throw new Error("breakpoint result was omitted");
  const deletedAdmission = await client.call("DebuggerControlService.DeleteBreakpoint", {
    breakpointId,
    target: { session: { sessionId: thread.sessionId } },
  });
  await client.waitOperation(deletedAdmission.operation?.operationId ?? "");

  const backtraceAdmission = await client.call(
    "DebuggerControlService.RunDistributedBacktrace",
    {
      target: { thread: { threadId: thread.threadId } },
      maxFrames: 32,
    },
  );
  const backtrace = await client.waitOperation(backtraceAdmission.operation?.operationId ?? "");
  if (!backtrace.result?.distributedBacktrace?.frames?.length) {
    throw new Error("distributed backtrace result was empty");
  }

  const beforeLine = location.line;
  const stateSnapshot = (await client.call("DebuggerService.GetSnapshot", {
    sections: ["SNAPSHOT_SECTION_EXECUTION", "SNAPSHOT_SECTION_PENDING_OPERATIONS"],
  })).snapshot;
  const events = client.subscribeStateEvents(
    { afterCursor: stateSnapshot?.stateEventCursor },
    { maxAttempts: 2 },
  );
  const executionChanged = (async () => {
    for await (const event of events) {
      if (event.kind === "STATE_EVENT_KIND_EXECUTION_CHANGED") return event;
    }
    throw new Error("state stream ended");
  })();
  const nextAdmission = await client.call("DebuggerControlService.Execute", {
    target: { thread: { threadId: thread.threadId } },
    action: "EXECUTION_ACTION_NEXT",
  });
  await client.waitOperation(nextAdmission.operation?.operationId ?? "");
  await Promise.race([
    executionChanged,
    new Promise((_, reject) => setTimeout(() => reject(new Error("execution event timeout")), 5_000)),
  ]);
  await events.return();
  const after = await client.collect(
    "DebuggerService.ListFrames",
    { threadId: thread.threadId, page: { pageSize: 1 } },
    32,
  );
  if (after[0]?.location?.line === beforeLine) throw new Error("step-over did not move source line");

  console.log(JSON.stringify({
    language: "typescript",
    serverInstanceId: serverInfo.serverInstanceId,
    sessions: sessions.length,
    frames: frames.length,
  }));
} finally {
  client.close();
}
