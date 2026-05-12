export type Column = "todo" | "in_progress" | "done";

export interface Task {
  id: string;
  title: string;
  column: Column;
}

export type KanbanState = "viewing" | "creating" | "editing" | "confirming_delete";

export interface KanbanContext {
  tasks: Task[];
  draft_title: string;
  editing_id: string;
  confirm_id: string;
}

export type KanbanAction =
  | { type: "start_create" }
  | { type: "start_edit"; id: string; title: string }
  | { type: "start_delete"; id: string }
  | { type: "move_task"; id: string; column: Column }
  | { type: "save_create" }
  | { type: "save_edit" }
  | { type: "confirm_delete" }
  | { type: "cancel" }
  | { type: "set_draft"; value: string };
