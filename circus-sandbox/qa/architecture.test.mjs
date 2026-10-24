import assert from "node:assert/strict";
import test from "node:test";

import { ArchitectureError, FACADE, analyzeDot } from "./architecture.mjs";

const CRATE = "circus_sandbox";

function body(id) {
  return id === CRATE ? CRATE : id.slice(CRATE.length + 2);
}

function nodeStatement(id, kind = "struct") {
  const label = kind === "crate" ? `crate|${CRATE}` : `pub ${kind}|${body(id)}`;
  return `    "${id}" [label="${label}", fillcolor="#000000"];`;
}

function edgeStatement(from, to) {
  return `    "${from}" -> "${to}" [label="uses", color="#7f7f7f", style="dashed"] [constraint=false];`;
}

function dotGraph(nodes, edges = []) {
  return `digraph {\n    graph [pad=0.4, overlap=false, fontsize="36"];\n\n${nodes.join("\n")}\n\n${edges.join("\n")}\n}`;
}

test("same-domain getter recursion is accepted", () => {
  const limits = `${CRATE}::runtime::ResourceLimits`;
  const getter = `${limits}::memory_bytes`;
  const constructor = `${limits}::new`;
  const result = analyzeDot(
    dotGraph(
      [
        nodeStatement(CRATE, "crate"),
        nodeStatement(limits),
        nodeStatement(getter, "fn"),
        nodeStatement(constructor, "fn"),
      ],
      [edgeStatement(getter, limits), edgeStatement(constructor, getter)],
    ),
  );
  assert.equal(result.violations.length, 0);
  assert.equal(result.intraDomainEdges, 2);
  assert.deepEqual(result.domains, ["facade", "runtime"]);
});

test("the matrix allows workspace to runtime but rejects the reverse", () => {
  const workspace = `${CRATE}::workspace::Workspace`;
  const limits = `${CRATE}::runtime::ResourceLimits`;
  const nodes = [
    nodeStatement(CRATE, "crate"),
    nodeStatement(workspace),
    nodeStatement(limits),
  ];
  assert.equal(
    analyzeDot(dotGraph(nodes, [edgeStatement(workspace, limits)])).violations
      .length,
    0,
  );
  const reverse = analyzeDot(
    dotGraph(nodes, [edgeStatement(limits, workspace)]),
  );
  assert.equal(reverse.violations.length, 1);
  assert.equal(reverse.violations[0].from, "runtime");
  assert.equal(reverse.violations[0].to, "workspace");
});

test("an inter-domain cycle is rejected through its forbidden edge", () => {
  const environment = `${CRATE}::environment::Environment`;
  const infrastructure = `${CRATE}::infrastructure::Backend`;
  const result = analyzeDot(
    dotGraph(
      [
        nodeStatement(CRATE, "crate"),
        nodeStatement(environment),
        nodeStatement(infrastructure),
      ],
      [
        edgeStatement(environment, infrastructure),
        edgeStatement(infrastructure, environment),
      ],
    ),
  );
  assert.equal(result.violations.length, 1);
  assert.equal(result.violations[0].from, "infrastructure");
  assert.equal(result.violations[0].to, "environment");
  assert.equal(result.intraDomainEdges, 0);
});

test("malformed graphs, wrong crates, and unknown namespaces fail closed", () => {
  assert.throws(
    () =>
      analyzeDot(
        'digraph {\n    "other_crate" [label="crate|other_crate"];\n}',
      ),
    ArchitectureError,
  );
  assert.throws(() => analyzeDot("digraph {"), ArchitectureError);
  assert.throws(
    () =>
      analyzeDot(
        dotGraph([
          nodeStatement(CRATE, "crate"),
          nodeStatement(`${CRATE}::mystery::Thing`),
        ]),
      ),
    ArchitectureError,
  );
  assert.throws(
    () =>
      analyzeDot(
        dotGraph([
          nodeStatement(CRATE, "crate"),
          nodeStatement(`${CRATE}::mystery`, "mod"),
        ]),
      ),
    ArchitectureError,
  );
  assert.throws(
    () =>
      analyzeDot(
        dotGraph(
          [
            nodeStatement(CRATE, "crate"),
            nodeStatement(`${CRATE}::workspace::Workspace`),
          ],
          [
            edgeStatement(
              `${CRATE}::workspace::Workspace`,
              `${CRATE}::runtime::ResourceLimits`,
            ),
          ],
        ),
      ),
    ArchitectureError,
  );
  assert.throws(
    () =>
      analyzeDot(
        dotGraph([
          nodeStatement(CRATE, "crate"),
          `    "${CRATE}::runtime::ResourceLimits" [label="pub struct|runtime::Other"];`,
        ]),
      ),
    ArchitectureError,
  );
});

test("root facade symbols and methods use semantic ids and kinds", () => {
  const rootType = `${CRATE}::FacadeType`;
  const rootMethod = `${rootType}::run`;
  const infrastructure = `${CRATE}::infrastructure::Backend`;
  const result = analyzeDot(
    dotGraph(
      [
        nodeStatement(CRATE, "crate"),
        nodeStatement(rootType),
        nodeStatement(rootMethod, "fn"),
        nodeStatement(infrastructure),
      ],
      [
        edgeStatement(rootMethod, rootType),
        edgeStatement(CRATE, infrastructure),
      ],
    ),
  );
  assert.equal(result.violations.length, 1);
  assert.equal(result.violations[0].from, FACADE);
  assert.equal(result.violations[0].to, "infrastructure");
  assert.equal(result.violations[0].source, CRATE);
});

test("a root type named like a domain stays in the facade", () => {
  const namedRuntime = `${CRATE}::runtime`;
  const method = `${namedRuntime}::describe`;
  const result = analyzeDot(
    dotGraph(
      [
        nodeStatement(CRATE, "crate"),
        nodeStatement(namedRuntime),
        nodeStatement(method, "fn"),
      ],
      [edgeStatement(method, namedRuntime)],
    ),
  );
  assert.equal(result.violations.length, 0);
  assert.deepEqual(result.domains, [FACADE]);
  assert.equal(result.intraDomainEdges, 1);
});
