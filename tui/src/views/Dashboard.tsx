/**
 * Dashboard view (View 1) — 2x2 grid layout.
 *
 * Panels: Agents, Tasks, Workflows, Bus Tail.
 * Status bar at bottom with daily aggregates.
 * Tab cycles focus between panels, Enter for drill-down.
 * Up/Down arrows scroll within focused panel.
 */

import { useState, useMemo } from "react";
import { Box, Text, useInput } from "ink";
import { colors, symbols } from "../theme.js";
import type { ViewProps } from "./types.js";
import { useAgents, type AgentState } from "../hooks/useAgents.js";
import { useTasks, type TaskState, type TaskStatus } from "../hooks/useTasks.js";
import { useWorkflows, type WorkflowState } from "../hooks/useWorkflows.js";
import { useBus } from "../hooks/useBus.js";
import type { BusMessage } from "../bus.js";

const PANEL_COUNT = 4;
const BUS_TAIL_COUNT = 15;
const PANEL_MAX_ITEMS = 8;

const statusBadge: Record<AgentState["status"], string> = {
  online: symbols.connected,
  busy: symbols.connected,
  idle: symbols.disconnected,
  offline: symbols.disconnected,
};

const statusColor: Record<AgentState["status"], string> = {
  online: colors.statusConnected,
  busy: colors.statusBusy,
  idle: colors.statusIdle,
  offline: colors.statusDisconnected,
};

const taskIcon: Record<TaskStatus, string> = {
  pending: symbols.disconnected,
  running: symbols.connected,
  done: "\u2713",
  failed: "\u2717",
  cancelled: "\u2620",
};

const taskColor: Record<TaskStatus, string> = {
  pending: colors.textDim,
  running: colors.statusBusy,
  done: colors.success,
  failed: colors.error,
  cancelled: colors.error,
};

function formatTimestamp(ts: number): string {
  const d = new Date(ts);
  const h = d.getHours().toString().padStart(2, "0");
  const m = d.getMinutes().toString().padStart(2, "0");
  const s = d.getSeconds().toString().padStart(2, "0");
  return `${h}:${m}:${s}`;
}

function formatBusMessage(msg: BusMessage): string {
  const src = msg.source ?? "?";
  const tgt = msg.target ?? "*";
  const payload = msg.payload as Record<string, unknown> | undefined;
  let summary = msg.type;
  if (payload) {
    const action = payload.action as string | undefined;
    const task = payload.task as string | undefined;
    const text = payload.text as string | undefined;
    const detail = action || task || text;
    if (detail) {
      const short = typeof detail === "string" && detail.length > 30
        ? detail.slice(0, 27) + "..."
        : detail;
      summary = `${msg.type}: ${short}`;
    }
  }
  return `${src} ${symbols.arrow} ${tgt} ${summary}`;
}

function PanelHeader({ title, focused }: { title: string; focused: boolean }) {
  return (
    <Text bold color={focused ? colors.primary : colors.textDim}>
      {focused ? symbols.arrow + " " : "  "}
      {title}
    </Text>
  );
}

function AgentsPanel({
  agents,
  focused,
  selectedIndex,
}: {
  agents: AgentState[];
  focused: boolean;
  selectedIndex: number;
}) {
  return (
    <Box
      flexDirection="column"
      borderStyle="single"
      borderColor={focused ? colors.primary : colors.tabBorder}
      paddingX={1}
      flexGrow={1}
      flexBasis="50%"
    >
      <PanelHeader title="Agents" focused={focused} />
      {agents.length === 0 ? (
        <Text color={colors.textDim}>No agents connected</Text>
      ) : (
        agents.slice(0, PANEL_MAX_ITEMS).map((a, i) => (
          <Text key={a.name} wrap="truncate">
            <Text color={focused && i === selectedIndex ? colors.accent : statusColor[a.status]}>
              {focused && i === selectedIndex ? symbols.arrow : statusBadge[a.status]}
            </Text>
            {" "}
            <Text
              color={focused && i === selectedIndex ? colors.textBright : colors.text}
              bold={focused && i === selectedIndex}
            >
              {a.name}
            </Text>
            {a.model ? (
              <Text color={colors.textDim}> [{a.model}]</Text>
            ) : null}
            {a.currentTask ? (
              <Text color={colors.muted}> {a.currentTask}</Text>
            ) : null}
            {a.costUsd > 0 ? (
              <Text color={colors.accent}> ${a.costUsd.toFixed(2)}</Text>
            ) : null}
          </Text>
        ))
      )}
    </Box>
  );
}

function TasksPanel({
  tasks,
  focused,
  selectedIndex,
}: {
  tasks: TaskState[];
  focused: boolean;
  selectedIndex: number;
}) {
  return (
    <Box
      flexDirection="column"
      borderStyle="single"
      borderColor={focused ? colors.primary : colors.tabBorder}
      paddingX={1}
      flexGrow={1}
      flexBasis="50%"
    >
      <PanelHeader title="Tasks" focused={focused} />
      {tasks.length === 0 ? (
        <Text color={colors.textDim}>No tasks yet</Text>
      ) : (
        tasks.slice(0, PANEL_MAX_ITEMS).map((t, i) => (
          <Text key={t.id} wrap="truncate">
            <Text color={focused && i === selectedIndex ? colors.accent : taskColor[t.status]}>
              {focused && i === selectedIndex ? symbols.arrow : taskIcon[t.status]}
            </Text>
            {" "}
            <Text
              color={focused && i === selectedIndex ? colors.textBright : colors.text}
              bold={focused && i === selectedIndex}
            >
              {t.title}
            </Text>
            {t.assignee ? (
              <Text color={colors.textDim}> @{t.assignee}</Text>
            ) : null}
          </Text>
        ))
      )}
    </Box>
  );
}

function WorkflowsPanel({
  workflows,
  focused,
  selectedIndex,
}: {
  workflows: WorkflowState[];
  focused: boolean;
  selectedIndex: number;
}) {
  return (
    <Box
      flexDirection="column"
      borderStyle="single"
      borderColor={focused ? colors.primary : colors.tabBorder}
      paddingX={1}
      flexGrow={1}
      flexBasis="50%"
    >
      <PanelHeader title="Workflows" focused={focused} />
      {workflows.length === 0 ? (
        <Text color={colors.textDim}>No active workflows</Text>
      ) : (
        workflows.slice(0, PANEL_MAX_ITEMS).map((w, i) => (
          <Text key={w.id} wrap="truncate">
            <Text color={focused && i === selectedIndex ? colors.accent : colors.secondary}>
              {focused && i === selectedIndex ? symbols.arrow + " " : "  "}
            </Text>
            <Text
              color={focused && i === selectedIndex ? colors.textBright : colors.secondary}
              bold={focused && i === selectedIndex}
            >
              {w.name}
            </Text>
            <Text color={colors.textDim}> [{w.currentState}]</Text>
            {w.costUsd > 0 ? (
              <Text color={colors.accent}> ${w.costUsd.toFixed(2)}</Text>
            ) : null}
          </Text>
        ))
      )}
    </Box>
  );
}

function BusTailPanel({
  messages,
  focused,
}: {
  messages: BusMessage[];
  focused: boolean;
}) {
  const recent = messages.slice(-BUS_TAIL_COUNT);

  return (
    <Box
      flexDirection="column"
      borderStyle="single"
      borderColor={focused ? colors.primary : colors.tabBorder}
      paddingX={1}
      flexGrow={1}
      flexBasis="50%"
    >
      <PanelHeader title="Bus Tail" focused={focused} />
      {recent.length === 0 ? (
        <Text color={colors.textDim}>No messages</Text>
      ) : (
        recent.map((msg, i) => (
          <Text key={i} color={colors.text} wrap="truncate">
            <Text color={colors.textDim}>
              {formatTimestamp(Date.now())}
            </Text>
            {" "}
            {formatBusMessage(msg)}
          </Text>
        ))
      )}
    </Box>
  );
}

function StatusBar({
  tasks,
  agents,
}: {
  tasks: TaskState[];
  agents: AgentState[];
}) {
  const today = useMemo(() => {
    const start = new Date();
    start.setHours(0, 0, 0, 0);
    return start.getTime();
  }, []);

  const dailyTasks = tasks.filter((t) => t.createdAt >= today).length;
  const doneTasks = tasks.filter(
    (t) => t.status === "done" && t.updatedAt >= today,
  ).length;
  const totalCost = agents.reduce((sum, a) => sum + a.costUsd, 0);
  const busyCount = agents.filter((a) => a.status === "busy").length;

  return (
    <Box paddingX={1}>
      <Text color={colors.textDim}>
        Tasks today: <Text color={colors.text}>{dailyTasks}</Text>
        {" "}({doneTasks} done)
        {"  "}|{"  "}
        Agents: <Text color={colors.text}>{agents.length}</Text>
        {" "}({busyCount} busy)
        {"  "}|{"  "}
        Cost: <Text color={colors.accent}>${totalCost.toFixed(2)}</Text>
        {"  "}|{"  "}
        <Text color={colors.textDim}>Tab:focus  {symbols.arrow}/{symbols.arrow}:select  Enter:detail  1-8:views  ?:help</Text>
      </Text>
    </Box>
  );
}

export function Dashboard({ bus, onNavigate }: ViewProps) {
  const [focusedPanel, setFocusedPanel] = useState(0);
  const [selectedIndices, setSelectedIndices] = useState([0, 0, 0, 0]);
  const agents = useAgents(bus);
  const tasks = useTasks(bus);
  const workflows = useWorkflows(bus);
  const { messages } = useBus(bus);

  useInput((input, key) => {
    if (key.tab) {
      setFocusedPanel((prev) => (prev + 1) % PANEL_COUNT);
      return;
    }

    // Up/Down to select items within focused panel
    if (key.upArrow) {
      setSelectedIndices((prev) => {
        const next = [...prev];
        next[focusedPanel] = Math.max(0, (next[focusedPanel] ?? 0) - 1);
        return next;
      });
      return;
    }
    if (key.downArrow) {
      const maxItems = [agents.length, tasks.length, workflows.length, 0];
      setSelectedIndices((prev) => {
        const next = [...prev];
        const max = Math.min((maxItems[focusedPanel] ?? 0) - 1, PANEL_MAX_ITEMS - 1);
        next[focusedPanel] = Math.min((next[focusedPanel] ?? 0) + 1, Math.max(0, max));
        return next;
      });
      return;
    }

    // Enter — drill-down based on focused panel
    if (key.return && onNavigate) {
      const idx = selectedIndices[focusedPanel] ?? 0;
      switch (focusedPanel) {
        case 0: {
          const agent = agents[idx];
          if (agent) {
            onNavigate({ view: "agent-detail", agent });
          }
          break;
        }
        case 1: {
          const task = tasks[idx];
          if (task) {
            onNavigate({ view: "task-detail", task });
          }
          break;
        }
        case 2: {
          const workflow = workflows[idx];
          if (workflow) {
            onNavigate({ view: "workflow-detail", workflow });
          }
          break;
        }
        // Panel 3 (Bus Tail) — no drill-down, switch to Bus Stream
        case 3:
          break;
      }
      return;
    }

    // Suppress unused variable warning
    void input;
  });

  return (
    <Box flexDirection="column" flexGrow={1}>
      {/* Top row: Agents + Tasks */}
      <Box flexGrow={1}>
        <AgentsPanel
          agents={agents}
          focused={focusedPanel === 0}
          selectedIndex={selectedIndices[0] ?? 0}
        />
        <TasksPanel
          tasks={tasks}
          focused={focusedPanel === 1}
          selectedIndex={selectedIndices[1] ?? 0}
        />
      </Box>

      {/* Bottom row: Workflows + Bus Tail */}
      <Box flexGrow={1}>
        <WorkflowsPanel
          workflows={workflows}
          focused={focusedPanel === 2}
          selectedIndex={selectedIndices[2] ?? 0}
        />
        <BusTailPanel messages={messages} focused={focusedPanel === 3} />
      </Box>

      {/* Status bar */}
      <Box borderStyle="single" borderColor={colors.tabBorder} borderTop borderBottom={false} borderLeft={false} borderRight={false}>
        <StatusBar tasks={tasks} agents={agents} />
      </Box>
    </Box>
  );
}
