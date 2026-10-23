#!/usr/bin/env node
import { pathToFileURL } from "node:url";

const LINE_BREAK = /[\r\n\u2028\u2029]/u;
const TITLE = /^[a-z][a-z0-9-]*(?:\([^()]+\))?: \S.*$/u;

export function isValidPrTitle(title) {
  return typeof title === "string" && !LINE_BREAK.test(title) && TITLE.test(title);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  if (isValidPrTitle(process.env.PR_TITLE)) {
    console.log("PR title is valid.");
  } else {
    // Never echo untrusted metadata into GitHub Actions' workflow command channel.
    console.error(
      "PR title must use type: description or type(scope): description, with a lowercase ASCII type, an optional nonempty scope without parentheses, exactly one space after the colon, and a nonblank single-line description.",
    );
    process.exitCode = 1;
  }
}
