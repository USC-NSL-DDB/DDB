import {
  METHODS,
  PAGINATED_METHODS,
  type PaginatedItemOf,
  type PaginatedMethodName,
  type RequestOf,
  type ResponseOf,
  type StreamingMethodName,
  type UnaryMethodName,
} from "./generated/contract.js";
import type {
  Capabilities,
  DdbError,
  Operation,
  OutputEvent,
  ServerInfo,
  Snapshot,
  StateEvent,
  SubscribeOutputRequest,
  SubscribeStateEventsRequest,
} from "./generated/types.js";

const TERMINAL_OPERATION_STATES = new Set([
  "OPERATION_STATE_COMPLETED",
  "OPERATION_STATE_FAILED",
  "OPERATION_STATE_CANCELLED",
]);
const REHYDRATE_CODES = new Set([
  "DDB_ERROR_CODE_REPLAY_GAP",
  "DDB_ERROR_CODE_EXPIRED",
]);
const RETRYABLE_CODES = new Set([
  "DDB_ERROR_CODE_NOT_READY",
  "DDB_ERROR_CODE_UNAVAILABLE",
]);

export interface DdbClientConfig {
  endpoint: string;
  bearerToken?: string | undefined;
  connectTimeoutMs?: number | undefined;
  requestTimeoutMs?: number | undefined;
  maxRequestBytes?: number | undefined;
  maxResponseBytes?: number | undefined;
  maxStreamLineBytes?: number | undefined;
  fetch?: typeof globalThis.fetch | undefined;
}

export interface CallOptions {
  signal?: AbortSignal | undefined;
  timeoutMs?: number | undefined;
}

export interface RetryPolicy {
  initialBackoffMs?: number | undefined;
  maxBackoffMs?: number | undefined;
  maxAttempts?: number | undefined;
}

export interface Handshake {
  serverInfo: ServerInfo;
  capabilities: Capabilities;
}

export type StateSyncItem =
  | { type: "snapshot"; snapshot: Snapshot }
  | { type: "event"; event: StateEvent };

export class DdbClientError extends Error {
  override readonly name: string = "DdbClientError";
}

export class DdbApiError extends DdbClientError {
  override readonly name: string = "DdbApiError";

  constructor(
    readonly status: number,
    readonly detail: DdbError,
  ) {
    super(detail.message ?? `DDB returned HTTP ${status}`);
  }
}

export class DdbHttpError extends DdbClientError {
  override readonly name: string = "DdbHttpError";

  constructor(
    readonly status: number,
    readonly body: string,
  ) {
    super(`DDB returned HTTP ${status} without a valid v2 error envelope`);
  }

  isApiVersionUnavailable(): boolean {
    return this.status === 404;
  }
}

export class DdbTransportError extends DdbClientError {
  override readonly name: string = "DdbTransportError";

  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
  }
}

export class DdbProtocolError extends DdbClientError {
  override readonly name: string = "DdbProtocolError";
}

export class DdbClosedError extends DdbClientError {
  override readonly name: string = "DdbClosedError";

  constructor() {
    super("DDB client is closed");
  }
}

export class DdbStreamEndedError extends DdbClientError {
  override readonly name: string = "DdbStreamEndedError";

  constructor() {
    super("DDB event stream ended");
  }
}

interface Lifecycle {
  controller: AbortController;
  didTimeout(): boolean;
  clearTimer(): void;
  finish(): void;
}

export class DdbClient {
  readonly #endpoint: URL;
  readonly #bearerToken: string | undefined;
  readonly #connectTimeoutMs: number;
  readonly #requestTimeoutMs: number;
  readonly #maxRequestBytes: number;
  readonly #maxResponseBytes: number;
  readonly #maxStreamLineBytes: number;
  readonly #fetch: typeof globalThis.fetch;
  readonly #active = new Set<AbortController>();
  #closed = false;

  constructor(config: DdbClientConfig) {
    let endpoint: URL;
    try {
      endpoint = new URL(config.endpoint);
    } catch (error) {
      throw new DdbProtocolError("invalid DDB endpoint", { cause: error });
    }
    if (endpoint.protocol !== "http:" && endpoint.protocol !== "https:") {
      throw new DdbProtocolError("DDB endpoint must use http or https");
    }
    if (endpoint.username || endpoint.password) {
      throw new DdbProtocolError("DDB endpoint must not contain credentials");
    }
    endpoint.search = "";
    endpoint.hash = "";
    if (!endpoint.pathname.endsWith("/")) endpoint.pathname += "/";
    this.#endpoint = endpoint;
    if (config.bearerToken !== undefined && config.bearerToken.trim() === "") {
      throw new DdbProtocolError("bearerToken must not be empty");
    }
    this.#bearerToken = config.bearerToken;
    this.#connectTimeoutMs = positive(config.connectTimeoutMs ?? 3_000, "connectTimeoutMs");
    this.#requestTimeoutMs = positive(config.requestTimeoutMs ?? 10_000, "requestTimeoutMs");
    this.#maxRequestBytes = positive(config.maxRequestBytes ?? 4 * 1024 * 1024, "maxRequestBytes");
    this.#maxResponseBytes = positive(config.maxResponseBytes ?? 16 * 1024 * 1024, "maxResponseBytes");
    this.#maxStreamLineBytes = positive(config.maxStreamLineBytes ?? 4 * 1024 * 1024, "maxStreamLineBytes");
    const fetchImplementation = config.fetch ?? globalThis.fetch;
    if (typeof fetchImplementation !== "function") {
      throw new DdbProtocolError("this runtime does not provide fetch");
    }
    this.#fetch = fetchImplementation;
  }

  get closed(): boolean {
    return this.#closed;
  }

  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    for (const controller of this.#active) controller.abort(new DdbClosedError());
    this.#active.clear();
  }

  async call<K extends UnaryMethodName>(
    method: K,
    request: RequestOf<K>,
    options: CallOptions = {},
  ): Promise<ResponseOf<K>> {
    this.#ensureOpen();
    const spec = METHODS[method];
    if (spec.serverStreaming) throw new DdbProtocolError(`${method} is streaming`);
    const timeoutMs = positive(options.timeoutMs ?? this.#requestTimeoutMs, "timeoutMs");
    const body = this.#encode(method, request, timeoutMs);
    const lifecycle = this.#lifecycle(options.signal, timeoutMs);
    try {
      const response = await this.#post(spec.path, body, lifecycle.controller.signal);
      const bytes = await readBounded(response, this.#maxResponseBytes);
      if (!response.ok) throw decodeError(response.status, bytes);
      return parseObject(bytes, `${method} response`) as ResponseOf<K>;
    } catch (error) {
      throw this.#transportError(error, lifecycle);
    } finally {
      lifecycle.finish();
    }
  }

  async *stream<K extends StreamingMethodName>(
    method: K,
    request: RequestOf<K>,
    options: CallOptions = {},
  ): AsyncGenerator<ResponseOf<K>> {
    this.#ensureOpen();
    const spec = METHODS[method];
    if (!spec.serverStreaming) throw new DdbProtocolError(`${method} is not streaming`);
    const body = this.#encode(method, request, this.#requestTimeoutMs);
    const lifecycle = this.#lifecycle(
      options.signal,
      positive(options.timeoutMs ?? this.#connectTimeoutMs, "timeoutMs"),
    );
    try {
      const response = await this.#post(spec.path, body, lifecycle.controller.signal);
      lifecycle.clearTimer();
      if (!response.ok) {
        const bytes = await readBounded(response, this.#maxResponseBytes);
        throw decodeError(response.status, bytes);
      }
      for await (const value of ndjson(response, this.#maxStreamLineBytes)) {
        yield value as ResponseOf<K>;
      }
    } catch (error) {
      throw this.#transportError(error, lifecycle);
    } finally {
      lifecycle.finish();
    }
  }

  async handshake(options: CallOptions = {}): Promise<Handshake> {
    const serverResponse = await this.call("DebuggerService.GetServerInfo", {}, options);
    const capabilitiesResponse = await this.call("DebuggerService.GetCapabilities", {}, options);
    const serverInfo = serverResponse.serverInfo;
    const capabilities = capabilitiesResponse.capabilities;
    if (!serverInfo?.serverInstanceId || !serverInfo.apiVersions?.includes("v2")) {
      throw new DdbProtocolError("server does not advertise API v2");
    }
    if (
      capabilities?.apiVersion !== "v2" ||
      !capabilities.schemaVersion ||
      capabilities.serverInstanceId !== serverInfo.serverInstanceId
    ) {
      throw new DdbProtocolError("capabilities do not match the negotiated v2 server");
    }
    return { serverInfo, capabilities };
  }

  async collect<K extends PaginatedMethodName>(
    method: K,
    request: RequestOf<K>,
    maxItems = 10_000,
  ): Promise<PaginatedItemOf<K>[]> {
    positive(maxItems, "maxItems");
    const itemsField = PAGINATED_METHODS[method].itemsField;
    const original = request as Record<string, unknown>;
    const originalPage = isRecord(original.page) ? original.page : {};
    const pageSize = originalPage.pageSize;
    let nextToken = typeof originalPage.pageToken === "string" ? originalPage.pageToken : undefined;
    const seen = new Set<string>();
    const result: unknown[] = [];
    for (;;) {
      const page: Record<string, unknown> = {};
      if (typeof pageSize === "number") page.pageSize = pageSize;
      if (nextToken !== undefined) page.pageToken = nextToken;
      const current = { ...original, page } as RequestOf<K>;
      const response = (await this.call(
        method as UnaryMethodName,
        current as RequestOf<UnaryMethodName>,
      )) as unknown;
      if (!isRecord(response)) throw new DdbProtocolError(`${method} returned a non-object`);
      const pageItems = response[itemsField];
      if (!Array.isArray(pageItems)) {
        throw new DdbProtocolError(`${method} omitted ${itemsField}`);
      }
      if (result.length + pageItems.length > maxItems) {
        throw new DdbProtocolError(`${method} exceeded the ${maxItems}-item bound`);
      }
      result.push(...pageItems);
      const info = response.page;
      const token = isRecord(info) ? info.nextPageToken : undefined;
      if (token === undefined || token === null) break;
      if (typeof token !== "string" || token === "" || seen.has(token)) {
        throw new DdbProtocolError(`${method} returned an invalid continuation token`);
      }
      seen.add(token);
      nextToken = token;
    }
    return result as PaginatedItemOf<K>[];
  }

  async waitOperation(
    operationId: string,
    timeoutMs = 10_000,
    pollIntervalMs = 50,
  ): Promise<Operation> {
    if (!operationId) throw new DdbProtocolError("operationId must not be empty");
    positive(timeoutMs, "timeoutMs");
    positive(pollIntervalMs, "pollIntervalMs");
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const response = await this.call("DebuggerService.GetOperation", { operationId });
      const operation = response.operation;
      if (!operation?.operationId) throw new DdbProtocolError("GetOperation omitted operation");
      if (operation.state && TERMINAL_OPERATION_STATES.has(operation.state)) return operation;
      if (Date.now() >= deadline) {
        throw new DdbProtocolError(`operation ${operationId} did not complete before timeout`);
      }
      await this.#sleep(Math.min(pollIntervalMs, Math.max(1, deadline - Date.now())));
    }
  }

  async *subscribeStateEvents(
    request: SubscribeStateEventsRequest = {},
    policy: RetryPolicy = {},
  ): AsyncGenerator<StateEvent> {
    const retry = normalizedRetry(policy);
    let afterCursor = request.afterCursor;
    let attempts = 0;
    while (!this.#closed) {
      try {
        for await (const event of this.stream("DdbEventService.SubscribeStateEvents", {
          ...request,
          ...(afterCursor === undefined ? {} : { afterCursor }),
        })) {
          attempts = 0;
          if (event.cursor) afterCursor = event.cursor;
          yield event;
        }
        throw new DdbStreamEndedError();
      } catch (error) {
        if (this.#closed) return;
        if (requiresRehydration(error) || !isRetryable(error)) throw error;
        attempts += 1;
        if (retry.maxAttempts !== undefined && attempts > retry.maxAttempts) throw error;
        await this.#sleep(backoff(retry, attempts));
      }
    }
  }

  async *subscribeOutput(
    request: SubscribeOutputRequest = {},
    policy: RetryPolicy = {},
  ): AsyncGenerator<OutputEvent> {
    const retry = normalizedRetry(policy);
    let afterCursor = request.afterCursor;
    let attempts = 0;
    while (!this.#closed) {
      try {
        for await (const event of this.stream("DdbEventService.SubscribeOutput", {
          ...request,
          ...(afterCursor === undefined ? {} : { afterCursor }),
        })) {
          attempts = 0;
          if (event.cursor) afterCursor = event.cursor;
          yield event;
        }
        throw new DdbStreamEndedError();
      } catch (error) {
        if (this.#closed) return;
        if (requiresRehydration(error) || !isRetryable(error)) throw error;
        attempts += 1;
        if (retry.maxAttempts !== undefined && attempts > retry.maxAttempts) throw error;
        await this.#sleep(backoff(retry, attempts));
      }
    }
  }

  async *stateSync(
    snapshotRequest: RequestOf<"DebuggerService.GetSnapshot"> = {},
    policy: RetryPolicy = {},
  ): AsyncGenerator<StateSyncItem> {
    while (!this.#closed) {
      const response = await this.call("DebuggerService.GetSnapshot", snapshotRequest);
      const snapshot = response.snapshot;
      if (!snapshot?.serverInstanceId || !snapshot.stateEventCursor) {
        throw new DdbProtocolError("GetSnapshot omitted synchronization metadata");
      }
      yield { type: "snapshot", snapshot };
      try {
        for await (const event of this.subscribeStateEvents(
          { afterCursor: snapshot.stateEventCursor },
          policy,
        )) {
          yield { type: "event", event };
        }
        return;
      } catch (error) {
        if (!requiresRehydration(error)) throw error;
      }
    }
  }

  #ensureOpen(): void {
    if (this.#closed) throw new DdbClosedError();
  }

  #encode(method: string, request: unknown, timeoutMs: number): string {
    if (!isRecord(request)) throw new DdbProtocolError(`${method} request must be an object`);
    const context = isRecord(request.context) ? { ...request.context } : {};
    if (context.deadline === undefined) {
      context.deadline = new Date(Date.now() + timeoutMs).toISOString();
    }
    if (isMutation(method) && (typeof context.idempotencyKey !== "string" || !context.idempotencyKey)) {
      context.idempotencyKey = uuid();
    }
    const body = JSON.stringify({ ...request, context });
    const bytes = new TextEncoder().encode(body);
    if (bytes.byteLength > this.#maxRequestBytes) {
      throw new DdbProtocolError(`request exceeds the ${this.#maxRequestBytes}-byte bound`);
    }
    return body;
  }

  async #post(path: string, body: string, signal: AbortSignal): Promise<Response> {
    const url = new URL(path.replace(/^\//, ""), this.#endpoint);
    const headers = new Headers({
      accept: "application/json, application/x-ndjson",
      "content-type": "application/json",
    });
    if (this.#bearerToken !== undefined) headers.set("authorization", `Bearer ${this.#bearerToken}`);
    return this.#fetch(url, { method: "POST", headers, body, signal, redirect: "error" });
  }

  #lifecycle(external: AbortSignal | undefined, timeoutMs: number): Lifecycle {
    const controller = new AbortController();
    this.#active.add(controller);
    let timedOut = false;
    let timer: ReturnType<typeof setTimeout> | undefined = setTimeout(() => {
      timedOut = true;
      controller.abort(new DdbTransportError("DDB request timed out"));
    }, timeoutMs);
    const abort = (): void => controller.abort(external?.reason);
    if (external?.aborted) abort();
    else external?.addEventListener("abort", abort, { once: true });
    const clearTimer = (): void => {
      if (timer !== undefined) {
        clearTimeout(timer);
        timer = undefined;
      }
    };
    return {
      controller,
      didTimeout: () => timedOut,
      clearTimer,
      finish: () => {
        clearTimer();
        external?.removeEventListener("abort", abort);
        this.#active.delete(controller);
      },
    };
  }

  #transportError(error: unknown, lifecycle: Lifecycle): unknown {
    if (error instanceof DdbClientError) return error;
    if (this.#closed) return new DdbClosedError();
    if (lifecycle.didTimeout()) return new DdbTransportError("DDB request timed out", { cause: error });
    return new DdbTransportError("DDB transport failed", { cause: error });
  }

  async #sleep(milliseconds: number): Promise<void> {
    if (this.#closed) return;
    const controller = new AbortController();
    this.#active.add(controller);
    try {
      await new Promise<void>((resolve) => {
        const timer = setTimeout(resolve, milliseconds);
        controller.signal.addEventListener(
          "abort",
          () => {
            clearTimeout(timer);
            resolve();
          },
          { once: true },
        );
      });
    } finally {
      this.#active.delete(controller);
    }
  }
}

function positive(value: number, name: string): number {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new DdbProtocolError(`${name} must be a positive safe integer`);
  }
  return value;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isMutation(method: string): boolean {
  return method.startsWith("DebuggerControlService.") || method === "DdbAdminService.Shutdown";
}

function uuid(): string {
  if (typeof globalThis.crypto?.randomUUID === "function") return globalThis.crypto.randomUUID();
  const bytes = new Uint8Array(16);
  if (typeof globalThis.crypto?.getRandomValues !== "function") {
    throw new DdbProtocolError("this runtime cannot generate secure idempotency keys");
  }
  globalThis.crypto.getRandomValues(bytes);
  bytes[6] = ((bytes[6] ?? 0) & 0x0f) | 0x40;
  bytes[8] = ((bytes[8] ?? 0) & 0x3f) | 0x80;
  const hex = [...bytes].map((value) => value.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

async function readBounded(response: Response, limit: number): Promise<Uint8Array> {
  const declared = response.headers.get("content-length");
  if (declared !== null) {
    const length = Number(declared);
    if (!Number.isSafeInteger(length) || length < 0) {
      throw new DdbProtocolError("response has an invalid Content-Length");
    }
    if (length > limit) {
      throw new DdbProtocolError(`response exceeds the ${limit}-byte bound`);
    }
  }
  if (response.body === null) return new Uint8Array();
  const chunks: Uint8Array[] = [];
  let length = 0;
  const reader = response.body.getReader();
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    if (value === undefined) continue;
    length += value.byteLength;
    if (length > limit) {
      await reader.cancel();
      throw new DdbProtocolError(`response exceeds the ${limit}-byte bound`);
    }
    chunks.push(value);
  }
  return concatenate(chunks, length);
}

function parseObject(bytes: Uint8Array, label: string): Record<string, unknown> {
  try {
    const value: unknown = bytes.byteLength === 0 ? {} : JSON.parse(new TextDecoder().decode(bytes));
    if (!isRecord(value)) throw new Error("not an object");
    return value;
  } catch (error) {
    throw new DdbProtocolError(`${label} is not a JSON object`, { cause: error });
  }
}

function decodeError(status: number, bytes: Uint8Array): DdbClientError {
  const body = new TextDecoder().decode(bytes).slice(0, 512);
  try {
    const value: unknown = JSON.parse(new TextDecoder().decode(bytes));
    if (
      isRecord(value) &&
      typeof value.code === "string" &&
      value.code !== "DDB_ERROR_CODE_UNSPECIFIED" &&
      typeof value.message === "string" &&
      value.message.trim() !== ""
    ) {
      return new DdbApiError(status, value as DdbError);
    }
  } catch {
    // The untyped HTTP error below intentionally retains only bounded text.
  }
  return new DdbHttpError(status, body);
}

async function* ndjson(response: Response, maxLineBytes: number): AsyncGenerator<Record<string, unknown>> {
  if (response.body === null) throw new DdbProtocolError("stream response has no body");
  const reader = response.body.getReader();
  try {
  const pending: Uint8Array[] = [];
  let pendingLength = 0;
  const emit = (): Record<string, unknown> | undefined => {
    if (pendingLength === 0) return undefined;
    const bytes = concatenate(pending, pendingLength);
    pending.length = 0;
    pendingLength = 0;
    const length = bytes.at(-1) === 13 ? bytes.byteLength - 1 : bytes.byteLength;
    return parseObject(bytes.subarray(0, length), "NDJSON stream line");
  };
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    if (value === undefined) continue;
    let start = 0;
    for (let index = 0; index < value.byteLength; index += 1) {
      if (value[index] !== 10) continue;
      const part = value.subarray(start, index);
      if (pendingLength + part.byteLength > maxLineBytes) {
        await reader.cancel();
        throw new DdbProtocolError(`stream line exceeds the ${maxLineBytes}-byte bound`);
      }
      if (part.byteLength > 0) pending.push(part);
      pendingLength += part.byteLength;
      const parsed = emit();
      if (parsed !== undefined) yield parsed;
      start = index + 1;
    }
    const tail = value.subarray(start);
    if (pendingLength + tail.byteLength > maxLineBytes) {
      await reader.cancel();
      throw new DdbProtocolError(`stream line exceeds the ${maxLineBytes}-byte bound`);
    }
    if (tail.byteLength > 0) pending.push(tail);
    pendingLength += tail.byteLength;
  }
  const parsed = emit();
  if (parsed !== undefined) yield parsed;
  } finally {
    try {
      await reader.cancel();
    } catch {
      // The connection may already have closed; releasing the reader is enough.
    }
    reader.releaseLock();
  }
}

function concatenate(chunks: readonly Uint8Array[], length: number): Uint8Array {
  const result = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    result.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return result;
}

interface NormalizedRetry {
  initialBackoffMs: number;
  maxBackoffMs: number;
  maxAttempts?: number;
}

function normalizedRetry(policy: RetryPolicy): NormalizedRetry {
  const initialBackoffMs = positive(policy.initialBackoffMs ?? 100, "initialBackoffMs");
  const maxBackoffMs = positive(policy.maxBackoffMs ?? 5_000, "maxBackoffMs");
  if (initialBackoffMs > maxBackoffMs) {
    throw new DdbProtocolError("initialBackoffMs must not exceed maxBackoffMs");
  }
  if (policy.maxAttempts !== undefined) positive(policy.maxAttempts, "maxAttempts");
  return policy.maxAttempts === undefined
    ? { initialBackoffMs, maxBackoffMs }
    : { initialBackoffMs, maxBackoffMs, maxAttempts: policy.maxAttempts };
}

function backoff(policy: NormalizedRetry, attempts: number): number {
  return Math.min(policy.maxBackoffMs, policy.initialBackoffMs * 2 ** Math.min(attempts - 1, 20));
}

export function requiresRehydration(error: unknown): boolean {
  return error instanceof DdbApiError && typeof error.detail.code === "string" && REHYDRATE_CODES.has(error.detail.code);
}

export function isRetryable(error: unknown): boolean {
  if (error instanceof DdbTransportError || error instanceof DdbStreamEndedError) return true;
  if (error instanceof DdbApiError) {
    return error.detail.retryable === true ||
      (typeof error.detail.code === "string" && RETRYABLE_CODES.has(error.detail.code));
  }
  return error instanceof DdbHttpError && [429, 502, 503, 504].includes(error.status);
}
