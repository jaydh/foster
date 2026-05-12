// React kanban — same spec as examples/kanban.
//
// States  : viewing | creating | editing | confirming_delete
// Context : { tasks, draft_title, editing_id, confirm_id }

import { useReducer, useState } from "react";
import type { Column, KanbanAction, KanbanContext, KanbanState, Task } from "./types";

// ── reducer ───────────────────────────────────────────────────────────────────

type Machine = { state: KanbanState; ctx: KanbanContext };

const seed: Task[] = [
  { id: "1", title: "Design state model", column: "done" },
  { id: "2", title: "Build WASM client",  column: "in_progress" },
  { id: "3", title: "Write tests",         column: "todo" },
];

const initial: Machine = {
  state: "viewing",
  ctx: { tasks: seed, draft_title: "", editing_id: "", confirm_id: "" },
};

let nextId = 4;

function reducer(m: Machine, action: KanbanAction): Machine {
  const { state, ctx } = m;

  switch (action.type) {
    case "start_create":
      if (state !== "viewing") return m;
      return { state: "creating", ctx: { ...ctx, draft_title: "" } };

    case "start_edit":
      if (state !== "viewing") return m;
      return { state: "editing", ctx: { ...ctx, editing_id: action.id, draft_title: action.title } };

    case "start_delete":
      if (state !== "viewing") return m;
      return { state: "confirming_delete", ctx: { ...ctx, confirm_id: action.id } };

    case "move_task":
      if (state !== "viewing") return m;
      return {
        state: "viewing",
        ctx: {
          ...ctx,
          tasks: ctx.tasks.map((t) =>
            t.id === action.id ? { ...t, column: action.column } : t
          ),
        },
      };

    case "save_create":
      if (state !== "creating" || !ctx.draft_title.trim()) return m;
      return {
        state: "viewing",
        ctx: {
          ...ctx,
          tasks: [...ctx.tasks, { id: String(nextId++), title: ctx.draft_title, column: "todo" }],
          draft_title: "",
        },
      };

    case "save_edit":
      if (state !== "editing" || !ctx.draft_title.trim()) return m;
      return {
        state: "viewing",
        ctx: {
          ...ctx,
          tasks: ctx.tasks.map((t) =>
            t.id === ctx.editing_id ? { ...t, title: ctx.draft_title } : t
          ),
          editing_id: "",
          draft_title: "",
        },
      };

    case "confirm_delete":
      if (state !== "confirming_delete") return m;
      return {
        state: "viewing",
        ctx: { ...ctx, tasks: ctx.tasks.filter((t) => t.id !== ctx.confirm_id), confirm_id: "" },
      };

    case "cancel":
      if (state === "viewing") return m;
      return { state: "viewing", ctx: { ...ctx, draft_title: "", editing_id: "", confirm_id: "" } };

    case "set_draft":
      return { ...m, ctx: { ...ctx, draft_title: action.value } };

    default:
      return m;
  }
}

// ── component ─────────────────────────────────────────────────────────────────

function Column({ tasks, column, dispatch }: {
  tasks: Task[];
  column: Column;
  dispatch: React.Dispatch<KanbanAction>;
}) {
  const label: Record<Column, string> = { todo: "Todo", in_progress: "In Progress", done: "Done" };
  const filtered = tasks.filter((t) => t.column === column);

  return (
    <div>
      <div>{label[column]}</div>
      {filtered.map((task) => (
        <div key={task.id} data-task-id={task.id}>
          <div>{task.title}</div>
          <button onClick={() => dispatch({ type: "start_edit", id: task.id, title: task.title })}>edit</button>
          <button onClick={() => dispatch({ type: "start_delete", id: task.id })}>del</button>
          {column === "todo"         && <button onClick={() => dispatch({ type: "move_task", id: task.id, column: "in_progress" })}>→ IP</button>}
          {column === "in_progress"  && <button onClick={() => dispatch({ type: "move_task", id: task.id, column: "done"        })}>→ Done</button>}
          {column === "in_progress"  && <button onClick={() => dispatch({ type: "move_task", id: task.id, column: "todo"        })}>← Todo</button>}
          {column === "done"         && <button onClick={() => dispatch({ type: "move_task", id: task.id, column: "in_progress" })}>← IP</button>}
        </div>
      ))}
    </div>
  );
}

export default function App() {
  const [machine, dispatch] = useReducer(reducer, initial);
  const { state, ctx } = machine;

  return (
    <div data-machine-state={state}>
      {/* board */}
      {state === "viewing" && (
        <>
          <button onClick={() => dispatch({ type: "start_create" })}>+ New task</button>
          <div style={{ display: "grid", gridTemplateColumns: "repeat(3,1fr)" }}>
            <Column tasks={ctx.tasks} column="todo"        dispatch={dispatch} />
            <Column tasks={ctx.tasks} column="in_progress" dispatch={dispatch} />
            <Column tasks={ctx.tasks} column="done"        dispatch={dispatch} />
          </div>
        </>
      )}

      {/* create modal */}
      {state === "creating" && (
        <div role="dialog" aria-label="New task">
          <input
            value={ctx.draft_title}
            onChange={(e) => dispatch({ type: "set_draft", value: e.target.value })}
            placeholder="Task title…"
          />
          <button onClick={() => dispatch({ type: "cancel"      })}>Cancel</button>
          <button onClick={() => dispatch({ type: "save_create" })}>Save</button>
        </div>
      )}

      {/* edit modal */}
      {state === "editing" && (
        <div role="dialog" aria-label="Edit task">
          <input
            value={ctx.draft_title}
            onChange={(e) => dispatch({ type: "set_draft", value: e.target.value })}
          />
          <button onClick={() => dispatch({ type: "cancel"   })}>Cancel</button>
          <button onClick={() => dispatch({ type: "save_edit" })}>Save</button>
        </div>
      )}

      {/* delete confirmation */}
      {state === "confirming_delete" && (
        <div role="dialog" aria-label="Delete task?">
          <p>This cannot be undone.</p>
          <button onClick={() => dispatch({ type: "cancel"         })}>Cancel</button>
          <button onClick={() => dispatch({ type: "confirm_delete" })}>Delete</button>
        </div>
      )}
    </div>
  );
}
