"use strict";

(function exposeChatRecovery(root, factory) {
  const recovery = factory();
  if (typeof module === "object" && module.exports) module.exports = recovery;
  if (root) root.CarobaChatRecovery = recovery;
})(typeof globalThis === "undefined" ? null : globalThis, () => {
  const recoveryDelays = [1_000, 2_500, 5_000, 10_000, 20_000, 30_000];
  const maximumRecoveryMs = 15 * 60 * 1_000;

  function reconcileHistory(messages) {
    const reconciled = [];
    const positions = new Map();
    for (const message of Array.isArray(messages) ? messages : []) {
      if (!message || !["user", "assistant"].includes(message.role) || typeof message.text !== "string") continue;
      const id = typeof message.id === "string" ? message.id : "";
      if (id && id !== "unknown" && positions.has(id)) {
        const position = positions.get(id);
        reconciled[position] = { ...reconciled[position], ...message };
        continue;
      }
      if (id && id !== "unknown") positions.set(id, reconciled.length);
      reconciled.push({ ...message });
    }
    return reconciled;
  }

  function messageIds(messages) {
    return (Array.isArray(messages) ? messages : [])
      .map((message) => message && message.id)
      .filter((id) => typeof id === "string" && id && id !== "unknown");
  }

  function isNewMessage(message, baselineIds) {
    return typeof message.id === "string"
      && message.id
      && message.id !== "unknown"
      && !baselineIds.includes(message.id);
  }

  function hasRecoveredResponse(messages, baselineIds, promptText = null) {
    const history = Array.isArray(messages) ? messages : [];
    let requestIndex = -1;
    if (promptText !== null) {
      history.forEach((message, index) => {
        if (message
            && message.role === "user"
            && message.text === promptText
            && isNewMessage(message, baselineIds)) requestIndex = index;
      });
    }
    if (promptText !== null && requestIndex < 0) return false;
    return history.slice(requestIndex + 1).some((message) => (
      message
      && message.role === "assistant"
      && !message.pending
      && typeof message.text === "string"
      && message.text.trim()
      && isNewMessage(message, baselineIds)
    ));
  }

  function hasRemoteUserMessage(messages, pending) {
    if (!pending) return false;
    return (Array.isArray(messages) ? messages : []).some((message) => (
      message
      && message.role === "user"
      && message.text === pending.text
      && isNewMessage(message, pending.baselineIds)
    ));
  }

  function shouldReconcile(eventType, status = "") {
    if (["stream.open", "events.missed", "post.settled", "post.failed", "session.idle"].includes(eventType)) {
      return true;
    }
    return eventType === "session.status" && ["idle", "ready", "completed"].includes(status);
  }

  function recoveryDelay(attempt) {
    return recoveryDelays[attempt] ?? null;
  }

  function canContinueRecovery(startedAt, now = Date.now()) {
    return Number.isFinite(startedAt) && now - startedAt < maximumRecoveryMs;
  }

  return {
    canContinueRecovery,
    hasRecoveredResponse,
    hasRemoteUserMessage,
    messageIds,
    reconcileHistory,
    recoveryDelay,
    shouldReconcile,
  };
});
