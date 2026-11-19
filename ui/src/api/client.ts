import type { ApiMap } from "../gen/api-map";
import type { Request } from "../gen/Request";
import { send } from "./transport";

/** Calls one operation. The result type follows from the operation name. */
export async function call<K extends keyof ApiMap>(op: K, args: ApiMap[K]["args"]): Promise<ApiMap[K]["result"]> {
  // The generated `Request` and `ApiMap` come from one table, so a matching pair is a valid request.
  const result = await send({ op, args } as Request);
  return result as ApiMap[K]["result"];
}
