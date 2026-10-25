import { z } from "zod";

const responsesSchema = z.object({
  id: z.string().min(1),
  object: z.literal("response"),
  status: z.enum(["completed", "incomplete"]),
  output: z.array(z.unknown()),
  error: z.null().optional(),
});
const messagesSchema = z.object({
  id: z.string().min(1),
  type: z.literal("message"),
  role: z.literal("assistant"),
  content: z.array(z.unknown()),
});

/** Validate a small non-streaming probe without retaining or displaying its output. */
export async function acceptedModelResponse(
  response: Response,
  protocol: "responses" | "messages",
): Promise<boolean> {
  const reader = response.body?.getReader();
  if (!reader) return false;
  const chunks: Uint8Array[] = [];
  let length = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      length += value.byteLength;
      if (length > 65536) return false;
      chunks.push(value);
    }
    const schema = protocol === "messages" ? messagesSchema : responsesSchema;
    return schema.safeParse(JSON.parse(Buffer.concat(chunks).toString("utf8"))).success;
  } catch {
    return false;
  } finally {
    await reader.cancel();
  }
}
