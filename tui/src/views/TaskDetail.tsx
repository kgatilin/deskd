/**
 * Task Detail view (View 4) — full task drill-down.
 *
 * All fields: description, status, assignee, attempts, cost, turns.
 * Timeline: created -> dispatched -> accepted -> completed.
 * Actions: c (cancel with confirm), Esc (back).
 */

import { useState } from "react";
import { Box, Text, useInput } from "ink";
import { colors, symbols } from "../theme.js";
import type { TaskDetailProps } from "./types.js";
import { ConfirmDialog } from "../components/ConfirmDialog.js";
import type { TaskStatus } from "../hooks/useTasks.js";

const statusColor: Record<TaskStatus, string> = {
  pending: colors.textDim,
  running: colors.statusBusy,
  done: colors.success,
  failed: colors.error,
  cancelled: colors.error,
};

const statusIcon: Record<TaskStatus, string> = {
  pending: symbols.disconnected,
  running: symbols.connected,
  done: "\u2713",
  failed: "\u2717",
  cancelled: "\u2620",
};

function formatTimestamp(ts: number): string {
  const d = new Date(ts);
  return d.toLocaleString();
}

function formatDuration(start: number, end: number): string {
  const diff = end - start;
  if (diff < 1000) return `${diff}ms`;
  if (diff < 60_000) return `${(diff / 1000).toFixed(1)}s`;
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ${Math.floor((diff % 60_000) / 1000)}s`;
  return `${Math.floor(diff / 3_600_000)}h ${Math.floor((diff % 3_600_000) / 60_000)}m`;
}

interface TimelineStep {
  label: string;
  timestamp: number;
  active: boolean;
}

function Timeline({ steps }: { steps: TimelineStep[] }) {
  return (
    <Box flexDirection="column">
      {steps.map((step, i) => {
        const isLast = i === steps.length - 1;
        return (
          <Box key={step.label} flexDirection="column">
            <Box>
              <Text color={step.active ? colors.success : colors.textDim}>
                {step.active ? symbols.connected : symbols.disconnected}
              </Text>
              <Text color={step.active ? colors.text : colors.textDim}>
                {" "}{step.label}
              </Text>
              <Text color={colors.textDim}>
                {" "}{formatTimestamp(step.timestamp)}
              </Text>
            </Box>
            {!isLast ? (
              <Text color={colors.textDim}>{"  " + symbols.separator}</Text>
            ) : null}
          </Box>
        );
      })}
    </Box>
  );
}

function Field({ label, value, valueColor }: { label: string; value: string; valueColor?: string }) {
  return (
    <Text>
      <Text color={colors.textDim}>{label}: </Text>
      <Text color={valueColor ?? colors.text}>{value}</Text>
    </Text>
  );
}

export function TaskDetail({ bus, task, onBack }: TaskDetailProps) {
  const [showConfirm, setShowConfirm] = useState(false);

  useInput((_input, key) => {
    if (showConfirm) return;

    if (key.escape) {
      onBack();
      return;
    }
    if (_input === "c") {
      setShowConfirm(true);
      return;
    }
  });

  const handleCancel = () => {
    if (!task) return;
    bus.send({
      type: "message",
      source: "tui",
      target: "broadcast",
      payload: {
        action: "task_status",
        task_id: task.id,
        status: "cancelled",
      },
    });
    setShowConfirm(false);
  };

  if (!task) {
    return (
      <Box flexDirection="column" padding={1}>
        <Text color={colors.textDim}>
          No task selected. Select a task from Dashboard and press Enter.
        </Text>
        <Text color={colors.textDim}>Press Esc to go back.</Text>
      </Box>
    );
  }

  const timelineSteps: TimelineStep[] = [
    {
      label: "Created",
      timestamp: task.createdAt,
      active: true,
    },
    {
      label: "Dispatched",
      timestamp: task.createdAt,
      active: task.status !== "pending",
    },
    {
      label: "Running",
      timestamp: task.updatedAt,
      active: task.status === "running" || task.status === "done" || task.status === "failed",
    },
  ];

  if (task.status === "done" || task.status === "failed" || task.status === "cancelled") {
    timelineSteps.push({
      label: task.status === "done" ? "Completed" : task.status === "failed" ? "Failed" : "Cancelled",
      timestamp: task.updatedAt,
      active: true,
    });
  }

  const duration = formatDuration(task.createdAt, task.updatedAt);

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
            Task: {task.id}
          </Text>
          <Text color={colors.textDim}>{"  "}</Text>
          <Text color={statusColor[task.status]}>
            {statusIcon[task.status]} {task.status}
          </Text>
        </Box>
        <Text bold color={colors.text}>
          {task.title}
        </Text>
      </Box>

      {/* Body: fields + timeline side by side */}
      <Box flexGrow={1}>
        {/* Fields */}
        <Box
          flexDirection="column"
          borderStyle="single"
          borderColor={colors.tabBorder}
          paddingX={1}
          flexGrow={1}
          flexBasis="50%"
        >
          <Text bold color={colors.secondary}>
            Details
          </Text>
          <Field label="Status" value={task.status} valueColor={statusColor[task.status]} />
          {task.assignee ? (
            <Field label="Assignee" value={task.assignee} />
          ) : (
            <Field label="Assignee" value="unassigned" valueColor={colors.textDim} />
          )}
          <Field label="Created" value={formatTimestamp(task.createdAt)} />
          <Field label="Updated" value={formatTimestamp(task.updatedAt)} />
          <Field label="Duration" value={duration} />
        </Box>

        {/* Timeline */}
        <Box
          flexDirection="column"
          borderStyle="single"
          borderColor={colors.tabBorder}
          paddingX={1}
          flexGrow={1}
          flexBasis="50%"
        >
          <Text bold color={colors.secondary}>
            Timeline
          </Text>
          <Timeline steps={timelineSteps} />
        </Box>
      </Box>

      {/* Confirm overlay */}
      {showConfirm ? (
        <ConfirmDialog
          message={`Cancel task "${task.id}"?`}
          onConfirm={handleCancel}
          onCancel={() => setShowConfirm(false)}
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
          c:cancel task  Esc:back
        </Text>
      </Box>
    </Box>
  );
}
