/**
 * Root App component — view switching, global navigation, status bar.
 *
 * Keys:
 *   1-8  Switch views
 *   q    Quit TUI (deskd keeps running)
 *   Q    Send shutdown to deskd, then quit
 *   ?    Toggle help overlay
 */

import { useState, useEffect, useCallback } from "react";
import { Box, Text, useApp, useInput } from "ink";
import type { BusClient, ConnectionState, BusMessage } from "./bus.js";
import { colors, viewLabels, symbols } from "./theme.js";
import type { NavigationTarget } from "./views/types.js";
import type { AgentState } from "./hooks/useAgents.js";
import type { TaskState } from "./hooks/useTasks.js";
import type { WorkflowState } from "./hooks/useWorkflows.js";

// Views
import { Dashboard } from "./views/Dashboard.js";
import { BusStream } from "./views/BusStream.js";
import { AgentDetail } from "./views/AgentDetail.js";
import { TaskDetail } from "./views/TaskDetail.js";
import { WorkflowView } from "./views/WorkflowView.js";
import { CostTracker } from "./views/CostTracker.js";
import { TaskQueue } from "./views/TaskQueue.js";
import { Schedules } from "./views/Schedules.js";

interface AppProps {
  bus: BusClient;
}

/** Navigation context for detail views. */
interface NavContext {
  agent: AgentState | null;
  task: TaskState | null;
  workflow: WorkflowState | null;
  previousView: number;
}

export function App({ bus }: AppProps) {
  const { exit } = useApp();
  const [activeView, setActiveView] = useState(1);
  const [connectionState, setConnectionState] =
    useState<ConnectionState>("disconnected");
  const [showHelp, setShowHelp] = useState(false);
  const [debugMessages, setDebugMessages] = useState<BusMessage[]>([]);
  const [navContext, setNavContext] = useState<NavContext>({
    agent: null,
    task: null,
    workflow: null,
    previousView: 1,
  });

  useEffect(() => {
    const onStateChange = (state: ConnectionState) => {
      setConnectionState(state);
    };
    const onMessage = (msg: BusMessage) => {
      setDebugMessages((prev) => {
        const next = [...prev, msg];
        return next.length > 50 ? next.slice(-50) : next;
      });
    };

    bus.on("stateChange", onStateChange);
    bus.on("message", onMessage);
    setConnectionState(bus.state);

    return () => {
      bus.off("stateChange", onStateChange);
      bus.off("message", onMessage);
    };
  }, [bus]);

  const handleQuit = useCallback(() => {
    bus.disconnect();
    exit();
  }, [bus, exit]);

  const handleForceQuit = useCallback(() => {
    bus.sendShutdown();
    setTimeout(() => {
      bus.disconnect();
      exit();
    }, 200);
  }, [bus, exit]);

  const handleNavigate = useCallback(
    (target: NavigationTarget) => {
      switch (target.view) {
        case "agent-detail":
          setNavContext((prev) => ({
            ...prev,
            agent: target.agent,
            previousView: activeView,
          }));
          setActiveView(3);
          break;
        case "task-detail":
          setNavContext((prev) => ({
            ...prev,
            task: target.task,
            previousView: activeView,
          }));
          setActiveView(4);
          break;
        case "workflow-detail":
          setNavContext((prev) => ({
            ...prev,
            workflow: target.workflow,
            previousView: activeView,
          }));
          setActiveView(5);
          break;
        case "dashboard":
          setActiveView(1);
          break;
      }
    },
    [activeView],
  );

  const handleBack = useCallback(() => {
    setActiveView(navContext.previousView);
  }, [navContext.previousView]);

  useInput((input, key) => {
    // View switching: 1-8
    const num = parseInt(input, 10);
    if (num >= 1 && num <= 8) {
      setActiveView(num);
      setShowHelp(false);
      return;
    }

    if (input === "q" && !key.shift) {
      handleQuit();
      return;
    }

    if (input === "Q" || (input === "q" && key.shift)) {
      handleForceQuit();
      return;
    }

    if (input === "?") {
      setShowHelp((prev) => !prev);
      return;
    }
  });

  const renderView = () => {
    const baseProps = {
      bus,
      messages: debugMessages,
      onNavigate: handleNavigate,
    };

    switch (activeView) {
      case 1:
        return <Dashboard {...baseProps} />;
      case 2:
        return <BusStream {...baseProps} />;
      case 3:
        return (
          <AgentDetail
            {...baseProps}
            agent={navContext.agent}
            onBack={handleBack}
          />
        );
      case 4:
        return (
          <TaskDetail
            {...baseProps}
            task={navContext.task}
            onBack={handleBack}
          />
        );
      case 5:
        return (
          <WorkflowView
            {...baseProps}
            workflow={navContext.workflow}
            onBack={handleBack}
          />
        );
      case 6:
        return <CostTracker {...baseProps} />;
      case 7:
        return <TaskQueue {...baseProps} />;
      case 8:
        return <Schedules {...baseProps} />;
      default:
        return <Dashboard {...baseProps} />;
    }
  };

  return (
    <Box flexDirection="column" width="100%" height="100%">
      {/* Tab bar */}
      <Box>
        {Object.entries(viewLabels).map(([num, label]) => {
          const n = parseInt(num, 10);
          const isActive = n === activeView;
          return (
            <Box key={num} marginRight={1}>
              <Text
                color={isActive ? colors.tabActive : colors.tabInactive}
                bold={isActive}
              >
                {num}:{label}
              </Text>
            </Box>
          );
        })}
        <Box flexGrow={1} />
        {/* Connection indicator */}
        <Text
          color={
            connectionState === "connected"
              ? colors.statusConnected
              : connectionState === "connecting"
                ? colors.statusBusy
                : colors.statusDisconnected
          }
        >
          {connectionState === "connected"
            ? symbols.connected
            : symbols.disconnected}{" "}
          {connectionState}
        </Text>
      </Box>

      {/* Separator */}
      <Box>
        <Text color={colors.tabBorder}>
          {colors.tabBorder
            ? symbols.horizontal.repeat(80)
            : symbols.horizontal.repeat(80)}
        </Text>
      </Box>

      {/* Help overlay or active view */}
      {showHelp ? (
        <HelpOverlay />
      ) : (
        <Box flexGrow={1} flexDirection="column">
          {renderView()}
        </Box>
      )}
    </Box>
  );
}

function HelpOverlay() {
  return (
    <Box flexDirection="column" padding={1}>
      <Text bold color={colors.primary}>
        Keyboard Shortcuts
      </Text>
      <Text> </Text>
      <Text>
        <Text bold>1-8</Text> Switch views
      </Text>
      <Text>
        <Text bold>q</Text> {"  "}Quit TUI (deskd keeps running)
      </Text>
      <Text>
        <Text bold>Q</Text> {"  "}Send shutdown to deskd + quit
      </Text>
      <Text>
        <Text bold>?</Text> {"  "}Toggle this help
      </Text>
      <Text> </Text>
      <Text bold color={colors.primary}>
        Dashboard
      </Text>
      <Text>
        <Text bold>Tab</Text> Cycle panel focus
      </Text>
      <Text>
        <Text bold>Enter</Text> Drill into focused item
      </Text>
      <Text> </Text>
      <Text bold color={colors.primary}>
        Detail Views
      </Text>
      <Text>
        <Text bold>Esc</Text> {"  "}Back to previous view
      </Text>
      <Text>
        <Text bold>s</Text> {"    "}Send message (Agent Detail)
      </Text>
      <Text>
        <Text bold>k</Text> {"    "}Kill agent (Agent Detail)
      </Text>
      <Text>
        <Text bold>c</Text> {"    "}Cancel task (Task Detail)
      </Text>
      <Text>
        <Text bold>m</Text> {"    "}Manual move (Workflow)
      </Text>
      <Text>
        <Text bold>x</Text> {"    "}Cancel workflow (Workflow)
      </Text>
      <Text> </Text>
      <Text color={colors.textDim}>Press any key to dismiss</Text>
    </Box>
  );
}
