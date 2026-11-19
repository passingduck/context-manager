import type { Format } from "../gen/Format";

/** The format the scan reports for a file, or one guessed from the extension for a file it has not seen. */
export function guessFormat(path: string, known?: Format): Format {
  if (known) return known;
  if (path.endsWith(".md")) return "markdown";
  if (path.endsWith(".jsonc")) return "jsonc";
  if (path.endsWith(".json")) return "json";
  if (path.endsWith(".toml")) return "toml";
  return "text";
}
