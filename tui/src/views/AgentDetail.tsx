/**
 * Agent Detail view (View 3) — full agent drill-down.
 *
 * Header: name, status, model, PID, uptime, budget gauge.
 * Current task, recent tasks (scrollable), filtered bus messages.
 * Actions: s (send message), k (kill with confirm), Esc (back).
 */

import { useState, useMemo } from "react";
import { Box, Text, useInput } from "ink";
import { colors, symbols } from "../theme.js";
import type { AgentDetailProps } from "./types.js";
import { useTasks } from "../hooks/useTasks.js";
import { useBus, matchesFilter } from "../hooks/useBus.js";
import { ConfirmDialog } from "../components/ConfirmDialog.js";
import { MessageComposer } from "../components/MessageComposer.js";
import type { BusMessage } from "../bus.js";

type Overlay = "none" | "confirm-kill" | "send-message";

const statusColor: Record<string, string> = {
  online: colors.statusConnected,
  busy: colors.statusBusy,
  idle: colors.statusIdle,
  offline: colors.statusDisconnected,
};

const statusBadge: Record<string, string> = {
  online: symbols.connected,
  busy: symbols.connected,
  idle: symbols.disconnected,
  offline: symbols.disconnected,
};

function formatUptime(lastSeen: number): string {
  const diff = Date.now() - lastSeen;
  if (diff < 60_000) return `${Math.floor(diff / 1000)}s ago`;
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
  return `${Math.floor(diff / 3_600_000)}h ago`;
}

function BudgetGauge({ costUsd, budgetUsd }: { costUsd: number; budgetUsd: number }) {
  const pct = budgetUsd > 0 ? Math.min(costUsd / budgetUsd, 1) : 0;
  const width = 20;
  const filled = Math.round(pct * width);
  const bar = "\u2588".repeat(filled) + "\u2591".repeat(width - filled);
  const barColor = pct > 0.9 ? colors.error : pct > 0.7 ? colors.warning : colors.success;

  return (
    <Text>
      <Text color={colors.textDim}>Budget: </Text>
      <Text color={barColor}>{bar}</Text>
      <Text color={colors.text}> ${costUsd.toFixed(2)}</Text>
      {budgetUsd > 0 ? (
        <Text color={colors.textDim}> / ${budgetUsd.toFixed(2)}</Text>
      ) : null}
    </Text>
  );
}

function formatBusMsg(msg: BusMessage): string {
  const src = msg.source ?? "?";
  const tgt = msg.target ?? "*";
  const payload = msg.payload as Record<string, unknown> | undefined;
  let detail = msg.type;
  if (payload) {
    const action = payload.action as string | undefined;
    const text = payload.text as string | undefined;
    const d = action || text;
    if (d) {
      detail = `${msg.type}: ${typeof d === "string" && d.length > 40 ? d.slice(0, 37) + "..." : d}`;
    }
  }
  return `${src} ${symbols.arrow} ${tgt} ${detail}`;
}

export function AgentDetail({ bus, agent, onBack }: AgentDetailProps) {
  const [overlay, setOverlay] = useState<Overlay>("none");
  const [taskScroll, setTaskScroll] = useState(0);
  const tasks = useTasks(bus);
  const { messages } = useBus(bus);

  const agentName = agent?.name ?? "unknown";

  // Tasks related to this agent
  const agentTasks = useMemo(
    () => tasks.filter((t) => t.assignee === agentName),
    [tasks, agentName],
  );

  // Bus messages to/from this agent
  const agentMessages = useMemo(
    () =>
      messages.filter(
        (m) =>
          matchesFilter(m, { source: agentName }) ||
          matchesFilter(m, { target: `agent:${agentName}` }),
      ),
    [messages, agentName],
  );

  const visibleTasks = useMemo(() => {
    const start = Math.max(0, taskScroll);
    return agentTasks.slice(start, start + 8);
  }, [agentTasks, taskScroll]);

  useInput((input, key) => {
    if (overlay !== "none") return;

    if (key.escape) {
      onBack();
      return;
    }
    if (input === "s") {
      setOverlay("send-message");
      return;
    }
    if (input === "k") {
      setOverlay("confirm-kill");
      return;
    }
    if (key.upArrow) {
      setTaskScroll((prev) => Math.max(prev - 1, 0));
      return;
    }
    if (key.downArrow) {
      setTaskScroll((prev) =>
        Math.min(prev + 1, Math.max(0, agentTasks.length - 8)),
      );
      return;
    }
  });

  const handleKill = () => {
    bus.send({
      type: "message",
      source: "tui",
      target: `agent:${agentName}`,
      payload: { action: "kill" },
    });
    setOverlay("none");
  };

  const handleSend = (text: string) => {
    bus.send({
      type: "message",
      source: "tui",
      target: `agent:${agentName}`,
      payload: { text },
    });
    setOverlay("none");
  };

  if (!agent) {
    return (
      <Box flexDirection="column" padding={1}>
        <Text color={colors.textDim}>
          No agent selected. Select an agent from Dashboard and press Enter.
        </Text>
        <Text color={colors.textDim}>Press Esc to go back.</Text>
      </Box>
    );
  }

  return (
    <Box flexDirection="column" flexGrow={1}>
      {/* Header */}
      <Box
        flexDirection="column"
        borderStyle="single"
        borderColor={colors.primary}
        paddingX={1}
      >
        <Box>
          <Text bold color={colors.primary}>
            Agent: {agentName}
          </Text>
          <Text color={colors.textDim}>{"  "}</Text>
          <Text color={statusColor[agent.status] ?? colors.textDim}>
            {statusBadge[agent.status] ?? symbols.disconnected} {agent.status}
          </Text>
          {agent.model ? (
            <Text color={colors.textDim}>{"  "}model: {agent.model}</Text>
          ) : null}
          <Text color={colors.textDim}>
            {"  "}seen: {formatUptime(agent.lastSeen)}
          </Text>
        </Box>
        <BudgetGauge costUsd={agent.costUsd} budgetUsd={50} />
        {agent.currentTask ? (
          <Text>
            <Text color={colors.accent}>Current task: </Text>
            <Text color={colors.text}>{agent.currentTask}</Text>
          </Text>
        ) : (
          <Text color={colors.textDim}>No active task</Text>
        )}
      </Box>

      {/* Body: tasks + bus messages side by side */}
      <Box flexGrow={1}>
        {/* Recent tasks */}
        <Box
          flexDirection="column"
          borderStyle="single"
          borderColor={colors.tabBorder}
          paddingX={1}
          flexGrow={1}
          flexBasis="50%"
        >
          <Text bold color={colors.secondary}>
            Recent Tasks ({agentTasks.length})
          </Text>
          {visibleTasks.length === 0 ? (
            <Text color={colors.textDim}>No tasks</Text>
          ) : (
            visibleTasks.map((t) => {
              const icon =
                t.status === "done"
                  ? "\u2713"
                  : t.status === "failed"
                    ? "\u2717"
                    : t.status === "running"
                      ? symbols.connected
                      : symbols.disconnected;
              const col =
                t.status === "done"
                  ? colors.success
                  : t.status === "failed"
                    ? colors.error
                    : t.status === "running"
                      ? colors.statusBusy
                      : colors.textDim;
              return (
                <Text key={t.id} wrap="truncate">
                  <Text color={col}>{icon}</Text> <Text color={colors.text}>{t.title}</Text>
                </Text>
              );
            })
          )}
          {agentTasks.length > 8 ? (
            <Text color={colors.textDim}>
              {symbols.arrow}/{symbols.arrow} scroll ({taskScroll + 1}-
              {Math.min(taskScroll + 8, agentTasks.length)} of{" "}
              {agentTasks.length})
            </Text>
          ) : null}
        </Box>

        {/* Filtered bus messages */}
        <Box
          flexDirection="column"
          borderStyle="single"
          borderColor={colors.tabBorder}
          paddingX={1}
          flexGrow={1}
          flexBasis="50%"
        >
          <Text bold color={colors.secondary}>
            Bus Messages
          </Text>
          {agentMessages.length === 0 ? (
            <Text color={colors.textDim}>No messages for {agentName}</Text>
          ) : (
            agentMessages.slice(-12).map((msg, i) => (
              <Text key={i} color={colors.text} wrap="truncate">
                {formatBusMsg(msg)}
              </Text>
            ))
          )}
        </Box>
      </Box>

      {/* Overlay: confirm dialog or message composer */}
      {overlay === "confirm-kill" ? (
        <ConfirmDialog
          message={`Kill agent "${agentName}"? This will terminate the agent process.`}
          onConfirm={handleKill}
          onCancel={() => setOverlay("none")}
        />
      ) : overlay === "send-message" ? (
        <MessageComposer
          target={agentName}
          onSend={handleSend}
          onCancel={() => setOverlay("none")}
        />
      ) : null}

      {/* Status bar */}
      <Box
        borderStyle="single"
        borderColor={colors.tabBorder}
        borderTop
        borderBottom={false}
        borderLeft={false}
        borderRight={false}
        paddingX={1}
      >
        <Text color={colors.textDim}>
          s:send  k:kill  {symbols.arrow}/{symbols.arrow}:scroll tasks  Esc:back
        </Text>
      </Box>
    </Box>
  );
}
