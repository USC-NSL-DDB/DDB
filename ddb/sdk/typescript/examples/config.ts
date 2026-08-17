import { DdbClient } from "../src/index.js";

declare const process: { env: Record<string, string | undefined> };

export function newClient(): DdbClient {
  return new DdbClient({
    endpoint: process.env.DDB_ENDPOINT ?? "http://127.0.0.1:5000",
    bearerToken: process.env.DDB_API_TOKEN,
  });
}
