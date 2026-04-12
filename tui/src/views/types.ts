import type { BusClient, BusMessage } from "../bus.js";
import type { AgentState } from "../hooks/useAgents.js";
import type { TaskState } from "../hooks/useTasks.js";
import type { WorkflowState } from "../hooks/useWorkflows.js";

/** Common props passed to all view components. */
export interface ViewProps {
  bus: BusClient;
  messages: BusMessage[];
  /** Navigate to a detail view with context. */
  onNavigate?: (target: NavigationTarget) => void;
}

export type NavigationTarget =
  | { view: "agent-detail"; agent: AgentState }
  | { view: "task-detail"; task: TaskState }
  | { view: "workflow-detail"; workflow: WorkflowState }
  | { view: "dashboard" };

/** Props for detail views that receive a selected item. */
export interface AgentDetailProps extends ViewProps {
  agent: AgentState | null;
  onBack: () => void;
}

export interface TaskDetailProps extends ViewProps {
  task: TaskState | null;
  onBack: () => void;
}

export interface WorkflowDetailProps extends ViewProps {
  workflow: WorkflowState | null;
  onBack: () => void;
}
