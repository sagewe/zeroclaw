import { test } from "node:test";
import assert from "node:assert/strict";
import { sourceStatus, parseEntities } from "./events.logic.ts";
test("source status never claims listening from configuration alone", () => {
  assert.equal(sourceStatus(false, 60, []), "paused");
  assert.equal(sourceStatus(true, 60, []), "idle");
  assert.equal(sourceStatus(true, 60, [{ enabled: true }]), "waiting");
  assert.equal(
    sourceStatus(true, 60, [{ enabled: true, last_poll_ms: 10000 }], 11000),
    "healthy",
  );
  assert.equal(
    sourceStatus(true, 60, [{ enabled: true, last_poll_ms: 10000 }], 500000),
    "stale",
  );
  assert.equal(
    sourceStatus(true, 60, [{ enabled: true, last_error: "poll_failed" }]),
    "error",
  );
});
test("entities normalize lines and duplicates without creating wildcards", () => {
  assert.deepEqual(parseEntities("light.a\nlight.b, light.a"), [
    "light.a",
    "light.b",
  ]);
});
