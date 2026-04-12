/**
 * Workflow Detail view (View 5) — workflow drill-down with ASCII state diagram.
 *
 * ASCII state machine diagram, current state highlighted.
 * Transition history, owned tasks.
 * Actions: m (manual move), x (cancel), Esc (back).
 */

import { useState, useMemo } from "react";
import { Box, Text, useInput } from "ink";
import { colors, symbols } from "../theme.js";
import type { WorkflowDetailProps } from "./types.js";
import { useTasks } from "../hooks/useTasks.js";
import { useBus } from "../hooks/useBus.js";
import { ConfirmDialog } from "../components/ConfirmDialog.js";
import type { BusMessage } from "../bus.js";

type Overlay = "none" | "confirm-cancel" | "manual-move";

/** Known workflow states inferred from bus messages. */
interface InferredState {
  name: string;
  seenAt: number;
}

function inferStatesFromMessages(
  workflowId: string,
  messages: BusMessage[],
): InferredState[] {
  const states: InferredState[] = [];
  const seen = new Set<string>();

  for (const msg of messages) {
    const payload = msg.payload as Record<string, unknown> | undefined;
    if (!payload) continue;
    const id =
      (payload.graph_id as string) ||
      (payload.workflow_id as string) ||
      msg.id;
    if (id !== workflowId) continue;

    const state =
      (payload.state as string) || (payload.node as string);
    if (state && !seen.has(state)) {
      seen.add(state);
      states.push({ name: state, seenAt: Date.now() });
    }
  }

  return states;
}

function AsciiStateDiagram({
  states,
  currentState,
}: {
  states: string[];
  currentState: string;
}) {
  if (states.length === 0) {
    return <Text color={colors.textDim}>No states discovered yet</Text>;
  }

  // Ensure current state is in the list
  const allStates = [...states];
  if (!allStates.includes(currentState)) {
    allStates.push(currentState);
  }

  return (
    <Box flexDirection="column">
      {allStates.map((state, i) => {
        const isCurrent = state === currentState;
        const boxChar = isCurrent ? "\u2588" : "\u2591";
        const label = ` ${state} `;
        const padded = label.length < 20 ? label + " ".repeat(20 - label.length) : label;

        return (
          <Box key={state} flexDirection="column">
            <Box>
              <Text color={isCurrent ? colors.success : colors.textDim}>
                {isCurrent ? symbols.arrow + " " : "  "}
              </Text>
              <Text
                color={isCurrent ? colors.success : colors.muted}
                bold={isCurrent}
              >
                [{boxChar}{boxChar}]
              </Text>
              <Text
                color={isCurrent ? colors.textBright : colors.text}
                bold={isCurrent}
              >
                {padded}
              </Text>
            </Box>
            {i < allStates.length - 1 ? (
              <Text color={colors.textDim}>
                {"     " + symbols.separator}
              </Text>
            ) : null}
          </Box>
        );
      })}
    </Box>
  );
}

export function WorkflowView({ bus, workflow, onBack }: WorkflowDetailProps) {
  const [overlay, setOverlay] = useState<Overlay>("none");
  const [moveTarget, setMoveTarget] = useState("");
  const tasks = useTasks(bus);
  const { messages } = useBus(bus);

  const workflowId = workflow?.id ?? "";

  // Infer states from bus messages
  const inferredStates = useMemo(
    () => inferStatesFromMessages(workflowId, messages),
    [workflowId, messages],
  );

  const stateNames = useMemo(
    () => inferredStates.map((s) => s.name),
    [inferredStates],
  );

  // Transition history from bus messages
  const transitions = useMemo(() => {
    const result: Array<{ from: string; to: string; timestamp: number }> = [];
    let prev: string | null = null;

    for (const msg of messages) {
      const payload = msg.payload as Record<string, unknown> | undefined;
      if (!payload) continue;
      const id =
        (payload.graph_id as string) || (payload.workflow_id as string) || msg.id;
      if (id !== workflowId) continue;

      const state = (payload.state as string) || (payload.node as string);
      if (state && prev && prev !== state) {
        result.push({ from: prev, to: state, timestamp: Date.now() });
      }
      if (state) prev = state;
    }

    return result;
  }, [messages, workflowId]);

  // Tasks linked to this workflow (placeholder for future metadata matching)
  void tasks;

  useInput((input, key) => {
    if (overlay === "manual-move") {
      if (key.escape) {
        setOverlay("none");
        setMoveTarget("");
        return;
      }
      if (key.return && moveTarget.trim()) {
        bus.send({
          type: "message",
          source: "tui",
          target: "broadcast",
          payload: {
            action: "workflow_transition",
            workflow_id: workflowId,
            state: moveTarget.trim(),
          },
        });
        setOverlay("none");
        setMoveTarget("");
        return;
      }
      if (key.backspace || key.delete) {
        setMoveTarget((prev) => prev.slice(0, -1));
        return;
      }
      if (input && !key.ctrl && !key.meta) {
        setMoveTarget((prev) => prev + input);
        return;
      }
      return;
    }

    if (overlay !== "none") return;

    if (key.escape) {
      onBack();
      return;
    }
    if (input === "m") {
      setOverlay("manual-move");
      return;
    }
    if (input === "x") {
      setOverlay("confirm-cancel");
      return;
    }
  });

  const handleCancel = () => {
    bus.send({
      type: "message",
      source: "tui",
      target: "broadcast",
      payload: {
        action: "workflow_complete",
        workflow_id: workflowId,
        error: "cancelled by user",
      },
    });
    setOverlay("none");
  };

  if (!workflow) {
    return (
      <Box flexDirection="column" padding={1}>
        <Text color={colors.textDim}>
          No workflow selected. Select a workflow from Dashboard and press Enter.
        </Text>
        <Text color={colors.textDim}>Press Esc to go back.</Text>
      </Box>
    );
  }

  const uptime =
    workflow.startedAt > 0
      ? `started ${new Date(workflow.startedAt).toLocaleTimeString()}`
      : "unknown start";

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
            Workflow: {workflow.name}
          </Text>
          <Text color={colors.textDim}>{"  "}id: {workflow.id}</Text>
        </Box>
        <Box>
          <Text color={colors.secondary}>
            State: <Text bold>{workflow.currentState}</Text>
          </Text>
          <Text color={colors.textDim}>{"  "}{uptime}</Text>
          {workflow.costUsd > 0 ? (
            <Text color={colors.accent}>{"  "}${workflow.costUsd.toFixed(2)}</Text>
          ) : null}
        </Box>
      </Box>

      {/* Body: state diagram + transitions */}
      <Box flexGrow={1}>
        {/* ASCII state diagram */}
        <Box
          flexDirection="column"
          borderStyle="single"
          borderColor={colors.tabBorder}
          paddingX={1}
          flexGrow={1}
          flexBasis="50%"
        >
          <Text bold color={colors.secondary}>
            State Machine
          </Text>
          <AsciiStateDiagram
            states={stateNames}
            currentState={workflow.currentState}
          />
        </Box>

        {/* Transition history */}
        <Box
          flexDirection="column"
          borderStyle="single"
          borderColor={colors.tabBorder}
          paddingX={1}
          flexGrow={1}
          flexBasis="50%"
        >
          <Text bold color={colors.secondary}>
            Transition History ({transitions.length})
          </Text>
          {transitions.length === 0 ? (
            <Text color={colors.textDim}>No transitions recorded</Text>
          ) : (
            transitions.slice(-10).map((tr, i) => (
              <Text key={i} wrap="truncate">
                <Text color={colors.muted}>{tr.from}</Text>
                <Text color={colors.textDim}> {symbols.arrow} </Text>
                <Text color={colors.text}>{tr.to}</Text>
              </Text>
            ))
          )}
        </Box>
      </Box>

      {/* Overlays */}
      {overlay === "confirm-cancel" ? (
        <ConfirmDialog
          message={`Cancel workflow "${workflow.name}" (${workflow.id})?`}
          onConfirm={handleCancel}
          onCancel={() => setOverlay("none")}
        />
      ) : overlay === "manual-move" ? (
        <Box
          flexDirection="column"
          borderStyle="single"
          borderColor={colors.accent}
          paddingX={2}
          paddingY={1}
        >
          <Text bold color={colors.accent}>
            Manual State Move
          </Text>
          <Text color={colors.textDim}>
            Current state: <Text color={colors.text}>{workflow.currentState}</Text>
          </Text>
          <Box>
            <Text color={colors.accent}>New state: </Text>
            <Text color={colors.text}>{moveTarget}</Text>
            <Text color={colors.accent}>_</Text>
          </Box>
          <Text color={colors.textDim}>
            Enter: move | Esc: cancel
          </Text>
        </Box>
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
          m:manual move  x:cancel workflow  Esc:back
        </Text>
      </Box>
    </Box>
  );
}
