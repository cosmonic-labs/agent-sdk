import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { declaration, packageInfo } from "./lib.mjs";
import { featureSets, readVersion } from "./build.mjs";

const writeWit = (dir, name, id) => writeFileSync(join(dir, name), `package ${id};\ninterface types {}\n`);

test("package identity controls declaration and registry namespace", () => {
  assert.equal(packageInfo("agent").id, "cosmonic:agent");
  assert.equal(packageInfo("cosmonic:kafka").repository, "cosmonic-labs/cosmonic/kafka");
  assert.equal(packageInfo("cosmonic:notifications").repository, "cosmonic-labs/cosmonic/notifications");
  assert.equal(declaration("cosmonic:notifications", "0.3.0"), "package cosmonic:notifications@0.3.0;");
  for (const id of ["../notify", "cosmonic:notify/extra", "WasmCloud:notify", "cosmonic:bad--name"]) {
    assert.throws(() => packageInfo(id), /invalid package id/);
  }
});

test("build validation rejects namespace mismatches and mixed declarations", () => {
  const dir = mkdtempSync(join(tmpdir(), "wit-build-test-"));
  try {
    writeWit(dir, "types.wit", "cosmonic:notifications@0.3.0");
    writeWit(dir, "world.wit", "cosmonic:notifications@0.3.0");
    assert.equal(readVersion(dir, "cosmonic:notifications"), "0.3.0");
    assert.throws(() => readVersion(dir, "example:notifications"), /expected example:notifications/);
    writeWit(dir, "world.wit", "cosmonic:notifications@0.4.0");
    assert.throws(() => readVersion(dir, "cosmonic:notifications"), /files disagree/);
    writeWit(dir, "types.wit", "cosmonic:agent@0.3.0");
    writeWit(dir, "world.wit", "cosmonic:agent@0.3.0");
    assert.equal(readVersion(dir, "agent"), "0.3.0");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("notifications uses the existing cosmonic registry mapping", () => {
  const config = readFileSync(new URL("../../../wit/wkg-registries.toml", import.meta.url), "utf8");
  assert.match(config, /\[namespace_registries\.cosmonic\]/);
  assert.match(config, /namespacePrefix = "cosmonic-labs\/"/);
});

test("feature sets must list every @unstable feature and what it needs", () => {
  const dir = mkdtempSync(join(tmpdir(), "wit-features-test-"));
  const wit = (body) => writeFileSync(join(dir, "a.wit"), `package cosmonic:agent@0.4.0;\n${body}\n`);
  const sets = (needs) => writeFileSync(join(dir, "features.json"), JSON.stringify(needs));
  try {
    wit("interface types {}");
    assert.deepEqual(featureSets(dir), []);
    wit("@unstable(feature = one)\ninterface a {}\n@unstable(feature = two)\ninterface b {}");
    assert.throws(() => featureSets(dir), /no features.json/);
    sets({ one: [] });
    assert.throws(() => featureSets(dir), /does not list two/);
    sets({ one: [], two: ["one"], three: [] });
    assert.throws(() => featureSets(dir), /lists three, which no item uses/);
    sets({ one: [], two: ["zero"] });
    assert.throws(() => featureSets(dir), /needs zero, which is not listed/);
    sets({ one: [], two: ["one"] });
    assert.deepEqual(featureSets(dir), [["one"], ["two", "one"]]);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("the agent package lists its feature sets", () => {
  const dir = new URL("../../../wit/cosmonic-agent", import.meta.url).pathname;
  assert.ok(featureSets(dir).length > 0);
});
