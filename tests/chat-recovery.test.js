"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const recovery = require("../web/chat-recovery.js");

test("history reconciliation updates messages without duplicating IDs", () => {
  const history = [
    { id: "msg_user", role: "user", text: "status", pending: false },
    { id: "msg_agent", role: "assistant", text: "partial", pending: true },
    { id: "msg_agent", role: "assistant", text: "complete", pending: false },
  ];
  const once = recovery.reconcileHistory(history);
  const twice = recovery.reconcileHistory(once);

  assert.deepEqual(twice, [
    { id: "msg_user", role: "user", text: "status", pending: false },
    { id: "msg_agent", role: "assistant", text: "complete", pending: false },
  ]);
});

test("a completed assistant message after the submitted prompt recovers the response", () => {
  const history = [
    { id: "old_agent", role: "assistant", text: "old", pending: false },
    { id: "new_user", role: "user", text: "status", pending: false },
    { id: "new_agent", role: "assistant", text: "healthy", pending: false },
  ];

  assert.equal(recovery.hasRecoveredResponse(history, ["old_agent"], "status"), true);
  assert.equal(recovery.hasRecoveredResponse(history, ["old_agent", "new_agent"], "status"), false);
  assert.equal(recovery.hasRecoveredResponse(history, ["old_agent"], "different prompt"), false);
});

test("SSE connection and missed-event notifications trigger reconciliation", () => {
  assert.equal(recovery.shouldReconcile("stream.open"), true);
  assert.equal(recovery.shouldReconcile("events.missed"), true);
  assert.equal(recovery.shouldReconcile("message.part.updated"), false);
});

test("ready, idle and completed session states trigger final reconciliation", () => {
  for (const status of ["ready", "idle", "completed"]) {
    assert.equal(recovery.shouldReconcile("session.status", status), true);
  }
  assert.equal(recovery.shouldReconcile("session.status", "busy"), false);
});

test("a POST failure is recoverable when its response already exists remotely", () => {
  const pending = { text: "inspect", baselineIds: ["before"] };
  const history = [
    { id: "before", role: "assistant", text: "previous", pending: false },
    { id: "remote_user", role: "user", text: "inspect", pending: false },
    { id: "remote_agent", role: "assistant", text: "done", pending: false },
  ];

  assert.equal(recovery.shouldReconcile("post.failed"), true);
  assert.equal(recovery.hasRemoteUserMessage(history, pending), true);
  assert.equal(recovery.hasRecoveredResponse(history, pending.baselineIds, pending.text), true);
});

test("recovery retries use bounded non-aggressive backoff", () => {
  assert.deepEqual(
    Array.from({ length: 7 }, (_, attempt) => recovery.recoveryDelay(attempt)),
    [1_000, 2_500, 5_000, 10_000, 20_000, 30_000, null],
  );
  assert.equal(recovery.canContinueRecovery(1_000, 901_000), false);
  assert.equal(recovery.canContinueRecovery(1_000, 900_999), true);
});
