export interface Cursor {
  serverInstanceId: string;
  sequence: string;
}

export interface StateEvent {
  cursor: Cursor;
  kind: string;
  resourceKind: string;
  resourceId: string;
  resourceRevision: string;
  [field: string]: unknown;
}

export interface SnapshotResponse {
  snapshot: {
    stateEventCursor: Cursor;
    [field: string]: unknown;
  };
}

export interface DdbStateConnection {
  snapshot: SnapshotResponse["snapshot"];
  events: AsyncGenerator<StateEvent, void, void>;
}

export async function connectDdbState(
  baseUrl: string,
  bearerToken: string | undefined,
  signal?: AbortSignal,
): Promise<DdbStateConnection> {
  const rpcRoot = `${baseUrl.replace(/\/$/, "")}/api/v2/rpc`;
  const headers = new Headers({ "content-type": "application/json" });
  if (bearerToken) headers.set("authorization", `Bearer ${bearerToken}`);

  const post = async (service: string, method: string, body: unknown) => {
    const response = await fetch(
      `${rpcRoot}/ddb.api.v2.${service}/${method}`,
      { method: "POST", headers, body: JSON.stringify(body), signal },
    );
    if (!response.ok) throw await ddbError(response);
    return response;
  };

  await (await post("DebuggerService", "GetCapabilities", {})).json();
  const snapshotResponse = await post("DebuggerService", "GetSnapshot", {
    sections: [
      "SNAPSHOT_SECTION_TOPOLOGY",
      "SNAPSHOT_SECTION_EXECUTION",
      "SNAPSHOT_SECTION_BREAKPOINTS",
      "SNAPSHOT_SECTION_PENDING_OPERATIONS",
      "SNAPSHOT_SECTION_CAPABILITIES",
    ],
  });
  const snapshot = (await snapshotResponse.json()) as SnapshotResponse;
  const stream = await post("DdbEventService", "SubscribeStateEvents", {
    afterCursor: snapshot.snapshot.stateEventCursor,
  });
  if (!stream.body) throw new Error("DDB state stream has no response body");

  return {
    snapshot: snapshot.snapshot,
    events: decodeNdjson(stream.body, signal),
  };
}

async function* decodeNdjson(
  body: ReadableStream<Uint8Array>,
  signal?: AbortSignal,
): AsyncGenerator<StateEvent, void, void> {
  const reader = body.pipeThrough(new TextDecoderStream()).getReader();
  let buffered = "";
  try {
    while (!signal?.aborted) {
      const { value, done } = await reader.read();
      if (done) break;
      buffered += value;
      for (;;) {
        const newline = buffered.indexOf("\n");
        if (newline < 0) break;
        const line = buffered.slice(0, newline).trim();
        buffered = buffered.slice(newline + 1);
        if (!line) continue;
        const event = JSON.parse(line) as StateEvent;
        if (event.kind === "STATE_EVENT_KIND_REQUIRED_RESYNC") {
          throw new Error("DDB state history requires snapshot rehydration");
        }
        yield event;
      }
    }
    if (buffered.trim()) yield JSON.parse(buffered) as StateEvent;
  } finally {
    reader.releaseLock();
  }
}

async function ddbError(response: Response): Promise<Error> {
  const body = await response.json().catch(() => undefined) as
    | { code?: string; message?: string; requestId?: string }
    | undefined;
  const detail = body?.message ?? response.statusText;
  const error = new Error(`DDB request failed (${response.status}): ${detail}`);
  error.name = body?.code ?? "DdbHttpError";
  return error;
}
