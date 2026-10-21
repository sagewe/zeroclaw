import assert from "node:assert/strict";
import test from "node:test";
import { groupRooms, canSwitch, displayValue } from "./home.logic.ts";
import type { HomeDevice, HomeLight } from "../lib/home";
const device = (
  did: string,
  room_id: string,
  room_name: string,
): HomeDevice => ({
  did,
  room_id,
  room_name,
  name: "Lamp",
  model: "example.light",
  online: true,
  reported_at_ms: 1,
  urn: "",
});
test("room identity survives duplicate display names and search", () => {
  const rooms = groupRooms(
    [device("1", "a", "Bedroom"), device("2", "b", "Bedroom")],
    "",
  );
  assert.equal(rooms.length, 2);
  assert.equal(groupRooms([device("1", "a", "Bedroom")], "missing").length, 0);
  assert.equal(groupRooms([device("1", "a", "Bedroom")], "BEDROOM").length, 1);
});
test("unknown, unavailable, stale or revoked lights cannot be controlled", () => {
  const light: HomeLight = {
    light: "test",
    device_id: "1",
    entity_id: "light.test",
    state: "on",
  };
  assert.equal(canSwitch(light, true, true, false), true);
  for (const state of ["unknown", "unavailable", "unexpected"])
    assert.equal(canSwitch({ ...light, state }, true, true, false), false);
  assert.equal(canSwitch(light, false, true, false), false);
  assert.equal(canSwitch(light, true, false, false), false);
  assert.equal(canSwitch(light, true, true, true), false);
});
test("values retain false and zero, with bounded display", () => {
  assert.equal(displayValue(false), "false");
  assert.equal(displayValue(0), "0");
  assert.equal(displayValue(null), "—");
  assert.ok(displayValue("a".repeat(1000)).length <= 181);
});
