import assert from "node:assert/strict";
import test from "node:test";

import {
  DdbApiError,
  DdbClient,
  DdbClosedError,
  DdbHttpError,
} from "../dist/index.js";

test("preserves endpoint prefixes and prepares mutation policy", async () => {
  let observed;
  const client = new DdbClient({
    endpoint: "https://debug.example/team/a",
    bearerToken: "control-token",
    fetch: async (input, init) => {
      observed = { url: String(input), init, body: JSON.parse(init.body) };
      return Response.json({ operation: { operationId: "op_1" } });
    },
  });

  await client.call("DebuggerControlService.Execute", {
    target: { currentThread: {} },
    action: "EXECUTION_ACTION_NEXT",
  });

  assert.equal(
    observed.url,
    "https://debug.example/team/a/api/v2/rpc/ddb.api.v2.DebuggerControlService/Execute",
  );
  assert.equal(observed.init.headers.get("authorization"), "Bearer control-token");
  assert.match(observed.body.context.idempotencyKey, /^[0-9a-f-]{36}$/);
  assert.ok(Date.parse(observed.body.context.deadline) > Date.now());
  client.close();
});

test("only an untyped HTTP 404 authorizes explicit migration fallback", async () => {
  const missing = new DdbClient({
    endpoint: "http://127.0.0.1:1",
    fetch: async () => Response.json({ apiVersion: "v1" }, { status: 404 }),
  });
  await assert.rejects(
    missing.call("DebuggerService.GetServerInfo", {}),
    (error) => error instanceof DdbHttpError && error.isApiVersionUnavailable(),
  );

  const typed = new DdbClient({
    endpoint: "http://127.0.0.1:1",
    fetch: async () =>
      Response.json(
        { code: "DDB_ERROR_CODE_NOT_FOUND", message: "thread not found" },
        { status: 404 },
      ),
  });
  await assert.rejects(
    typed.call("DebuggerService.GetThread", { threadId: "thr_missing" }),
    (error) => error instanceof DdbApiError && error.detail.code === "DDB_ERROR_CODE_NOT_FOUND",
  );
});

test("collect follows bounded pages without duplicating tokens", async () => {
  const tokens = [];
  const client = new DdbClient({
    endpoint: "http://127.0.0.1:1",
    fetch: async (_input, init) => {
      const request = JSON.parse(init.body);
      tokens.push(request.page.pageToken ?? null);
      return request.page.pageToken === undefined
        ? Response.json({ sessions: [{ sessionId: "ses_1" }], page: { nextPageToken: "next" } })
        : Response.json({ sessions: [{ sessionId: "ses_2" }], page: {} });
    },
  });
  const sessions = await client.collect(
    "DebuggerService.ListSessions",
    { page: { pageSize: 1 } },
    2,
  );
  assert.deepEqual(tokens, [null, "next"]);
  assert.deepEqual(sessions.map((session) => session.sessionId), ["ses_1", "ses_2"]);
});

test("stream decodes fragmented NDJSON and ignores heartbeats", async () => {
  const encoder = new TextEncoder();
  const chunks = [
    encoder.encode('{"text":"one"'),
    encoder.encode('}\n\n{"text":"two"}\n'),
  ];
  const client = new DdbClient({
    endpoint: "http://127.0.0.1:1",
    fetch: async () =>
      new Response(
        new ReadableStream({
          start(controller) {
            for (const chunk of chunks) controller.enqueue(chunk);
            controller.close();
          },
        }),
        { headers: { "content-type": "application/x-ndjson" } },
      ),
  });
  const events = [];
  for await (const event of client.stream("DdbEventService.SubscribeOutput", {})) {
    events.push(event.text);
  }
  assert.deepEqual(events, ["one", "two"]);
});

test("returning an event iterator cancels its response reader", async () => {
  let cancelled = false;
  const encoder = new TextEncoder();
  const client = new DdbClient({
    endpoint: "http://127.0.0.1:1",
    fetch: async () =>
      new Response(
        new ReadableStream({
          start(controller) {
            controller.enqueue(encoder.encode('{"text":"one"}\n'));
          },
          cancel() {
            cancelled = true;
          },
        }),
      ),
  });
  const events = client.stream("DdbEventService.SubscribeOutput", {});
  assert.equal((await events.next()).value?.text, "one");
  await events.return();
  assert.equal(cancelled, true);
});

test("close aborts active calls", async () => {
  const client = new DdbClient({
    endpoint: "http://127.0.0.1:1",
    fetch: async (_input, init) =>
      new Promise((_resolve, reject) => {
        init.signal.addEventListener("abort", () => reject(init.signal.reason), { once: true });
      }),
  });
  const request = client.call("DebuggerService.GetServerInfo", {});
  client.close();
  await assert.rejects(request, DdbClosedError);
});
