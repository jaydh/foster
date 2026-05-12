#!/usr/bin/env bash
set -euo pipefail

# Measure LLM surface area: Foster vs React for the same app specs.
#
# The React implementations are embedded inline below — not committed as source
# files — so they stay readable and measurable without adding dependencies to
# the repo.  A temp dir is created, files are written, measured, then deleted.
#
# Apples-to-apples rules
# ──────────────────────
# 1. CSS is excluded from Foster's index.html (<style> blocks).
#    React implementations also have no styling — equal footing.
# 2. Server setup is excluded from Foster's main.rs (fn main body + tokio::main).
#    That's identical boilerplate across all Foster apps; React doesn't carry it.
# 3. "Implementation" and "tests" are measured separately.
#    Foster generates tests from the machine — test authoring cost is 0.
#    React tests are hand-written — their cost is real.
#
# Metrics
# ───────
# LOC    Non-blank, non-comment lines
# Tokens chars ÷ 4  (within ~15% of tiktoken for code; consistent across runs)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$SCRIPT_DIR/.."

# ── temp dir ──────────────────────────────────────────────────────────────────

TMPDIR_ROOT="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_ROOT"' EXIT

mkdir -p "$TMPDIR_ROOT/counter/react/src"
mkdir -p "$TMPDIR_ROOT/kanban/react/src"

# ── embedded React source ─────────────────────────────────────────────────────

cat > "$TMPDIR_ROOT/counter/react/src/App.tsx" <<'REACT_EOF'
// React counter — same spec as examples/counter.
//
// States  : idle | error
// Context : { count: number }
// Events  : increment | decrement | reset | break_it | recover

type State = "idle" | "error";
type Context = { count: number };

type Action =
  | { type: "increment" }
  | { type: "decrement" }
  | { type: "reset" }
  | { type: "break_it" }
  | { type: "recover" };

type MachineState = { state: State; ctx: Context };

function reducer(m: MachineState, action: Action): MachineState {
  const { state, ctx } = m;
  switch (action.type) {
    case "increment":
      if (state !== "idle") return m;
      return { state: "idle", ctx: { count: ctx.count + 1 } };
    case "decrement":
      if (state !== "idle") return m;
      return { state: "idle", ctx: { count: ctx.count - 1 } };
    case "reset":
      if (state !== "idle") return m;
      return { state: "idle", ctx: { count: 0 } };
    case "break_it":
      if (state !== "idle") return m;
      return { state: "error", ctx };
    case "recover":
      if (state !== "error") return m;
      return { state: "idle", ctx };
    default:
      return m;
  }
}

const initial: MachineState = { state: "idle", ctx: { count: 0 } };

export default function App() {
  const [machine, dispatch] = useReducer(reducer, initial);
  const { state, ctx } = machine;

  return (
    <div data-machine-state={state}>
      <p>state: {state}</p>

      {state === "idle" && (
        <div>
          <p>count: {ctx.count}</p>
          <button onClick={() => dispatch({ type: "increment" })}>+</button>
          <button onClick={() => dispatch({ type: "decrement" })}>−</button>
          <button onClick={() => dispatch({ type: "reset" })}>reset</button>
          <button onClick={() => dispatch({ type: "break_it" })}>break</button>
        </div>
      )}

      {state === "error" && (
        <div>
          <p>something broke</p>
          <button onClick={() => dispatch({ type: "recover" })}>recover</button>
        </div>
      )}
    </div>
  );
}

import { useReducer } from "react";
REACT_EOF

cat > "$TMPDIR_ROOT/counter/react/src/App.test.tsx" <<'REACT_EOF'
// React Testing Library tests — same coverage as generated counter.spec.ts.
// Every state transition and injection-equivalent is covered.
//
// Run: npx vitest (or jest with jsdom)

import { render, screen, fireEvent } from "@testing-library/react";
import App from "./App";

function getState() {
  return document.querySelector("[data-machine-state]")!
    .getAttribute("data-machine-state");
}

describe("counter | idle →[increment]→ idle", () => {
  it("increments the count", () => {
    render(<App />);
    expect(getState()).toBe("idle");
    fireEvent.click(screen.getByText("+"));
    expect(getState()).toBe("idle");
    expect(screen.getByText("count: 1")).toBeInTheDocument();
  });
});

describe("counter | idle →[decrement]→ idle", () => {
  it("decrements the count", () => {
    render(<App />);
    fireEvent.click(screen.getByText("−"));
    expect(screen.getByText("count: -1")).toBeInTheDocument();
  });
});

describe("counter | idle →[reset]→ idle", () => {
  it("resets after incrementing", () => {
    render(<App />);
    fireEvent.click(screen.getByText("+"));
    fireEvent.click(screen.getByText("+"));
    fireEvent.click(screen.getByText("reset"));
    expect(screen.getByText("count: 0")).toBeInTheDocument();
  });
});

describe("counter | idle →[break_it]→ error", () => {
  it("transitions to error", () => {
    render(<App />);
    fireEvent.click(screen.getByText("break"));
    expect(getState()).toBe("error");
    expect(screen.getByText("something broke")).toBeInTheDocument();
  });
});

describe("counter | error →[recover]→ idle", () => {
  it("recovers from error", () => {
    render(<App />);
    fireEvent.click(screen.getByText("break"));
    expect(getState()).toBe("error");
    fireEvent.click(screen.getByText("recover"));
    expect(getState()).toBe("idle");
  });
});

describe("counter | inject state: idle", () => {
  it("renders idle state correctly", () => {
    render(<App />);
    expect(getState()).toBe("idle");
    expect(screen.getByText("+")).toBeInTheDocument();
  });
});

describe("counter | inject state: error", () => {
  it("renders error state when reached via break_it", () => {
    render(<App />);
    fireEvent.click(screen.getByText("break"));
    expect(getState()).toBe("error");
    expect(screen.queryByText("+")).not.toBeInTheDocument();
  });
});
REACT_EOF

cat > "$TMPDIR_ROOT/kanban/react/src/types.ts" <<'REACT_EOF'
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
REACT_EOF

cat > "$TMPDIR_ROOT/kanban/react/src/App.tsx" <<'REACT_EOF'
// React kanban — same spec as examples/kanban.
//
// States  : viewing | creating | editing | confirming_delete
// Context : { tasks, draft_title, editing_id, confirm_id }

import { useReducer } from "react";
import type { Column, KanbanAction, KanbanContext, KanbanState, Task } from "./types";

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

function ColumnView({ tasks, column, dispatch }: {
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
          {column === "todo"        && <button onClick={() => dispatch({ type: "move_task", id: task.id, column: "in_progress" })}>→ IP</button>}
          {column === "in_progress" && <button onClick={() => dispatch({ type: "move_task", id: task.id, column: "done"        })}>→ Done</button>}
          {column === "in_progress" && <button onClick={() => dispatch({ type: "move_task", id: task.id, column: "todo"        })}>← Todo</button>}
          {column === "done"        && <button onClick={() => dispatch({ type: "move_task", id: task.id, column: "in_progress" })}>← IP</button>}
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
      {state === "viewing" && (
        <>
          <button onClick={() => dispatch({ type: "start_create" })}>+ New task</button>
          <div style={{ display: "grid", gridTemplateColumns: "repeat(3,1fr)" }}>
            <ColumnView tasks={ctx.tasks} column="todo"        dispatch={dispatch} />
            <ColumnView tasks={ctx.tasks} column="in_progress" dispatch={dispatch} />
            <ColumnView tasks={ctx.tasks} column="done"        dispatch={dispatch} />
          </div>
        </>
      )}

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

      {state === "editing" && (
        <div role="dialog" aria-label="Edit task">
          <input
            value={ctx.draft_title}
            onChange={(e) => dispatch({ type: "set_draft", value: e.target.value })}
          />
          <button onClick={() => dispatch({ type: "cancel"    })}>Cancel</button>
          <button onClick={() => dispatch({ type: "save_edit" })}>Save</button>
        </div>
      )}

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
REACT_EOF

cat > "$TMPDIR_ROOT/kanban/react/src/App.test.tsx" <<'REACT_EOF'
// RTL tests — same coverage as generated kanban.spec.ts.

import { render, screen, fireEvent } from "@testing-library/react";
import App from "./App";

function getState() {
  return document.querySelector("[data-machine-state]")!
    .getAttribute("data-machine-state");
}

test("viewing →[start_create]→ creating", () => {
  render(<App />);
  fireEvent.click(screen.getByText("+ New task"));
  expect(getState()).toBe("creating");
  expect(screen.getByRole("dialog", { name: "New task" })).toBeInTheDocument();
});

test("creating →[cancel]→ viewing", () => {
  render(<App />);
  fireEvent.click(screen.getByText("+ New task"));
  fireEvent.click(screen.getByText("Cancel"));
  expect(getState()).toBe("viewing");
});

test("creating →[save]→ viewing adds task", () => {
  render(<App />);
  fireEvent.click(screen.getByText("+ New task"));
  fireEvent.change(screen.getByPlaceholderText("Task title…"), {
    target: { value: "New item" },
  });
  fireEvent.click(screen.getByText("Save"));
  expect(getState()).toBe("viewing");
  expect(screen.getByText("New item")).toBeInTheDocument();
});

test("viewing →[start_edit]→ editing", () => {
  render(<App />);
  fireEvent.click(screen.getAllByText("edit")[0]);
  expect(getState()).toBe("editing");
  expect(screen.getByRole("dialog", { name: "Edit task" })).toBeInTheDocument();
});

test("editing →[cancel]→ viewing", () => {
  render(<App />);
  fireEvent.click(screen.getAllByText("edit")[0]);
  fireEvent.click(screen.getByText("Cancel"));
  expect(getState()).toBe("viewing");
});

test("editing →[save]→ viewing updates title", () => {
  render(<App />);
  fireEvent.click(screen.getAllByText("edit")[0]);
  const input = screen.getByRole("dialog", { name: "Edit task" }).querySelector("input")!;
  fireEvent.change(input, { target: { value: "Updated title" } });
  fireEvent.click(screen.getByText("Save"));
  expect(getState()).toBe("viewing");
  expect(screen.getByText("Updated title")).toBeInTheDocument();
});

test("viewing →[start_delete]→ confirming_delete", () => {
  render(<App />);
  fireEvent.click(screen.getAllByText("del")[0]);
  expect(getState()).toBe("confirming_delete");
  expect(screen.getByRole("dialog", { name: "Delete task?" })).toBeInTheDocument();
});

test("confirming_delete →[cancel]→ viewing", () => {
  render(<App />);
  fireEvent.click(screen.getAllByText("del")[0]);
  fireEvent.click(screen.getByText("Cancel"));
  expect(getState()).toBe("viewing");
});

test("confirming_delete →[confirm]→ viewing removes task", () => {
  render(<App />);
  fireEvent.click(screen.getAllByText("del")[0]);
  fireEvent.click(screen.getByText("Delete"));
  expect(getState()).toBe("viewing");
});

test("viewing →[move_task todo→in_progress]→ viewing", () => {
  render(<App />);
  fireEvent.click(screen.getByText("→ IP"));
  expect(getState()).toBe("viewing");
});
REACT_EOF

# ── filters ───────────────────────────────────────────────────────────────────

loc_html() {
    awk '/<style[ >]/{skip=1} skip{if(/<\/style>/){skip=0} next} 1' "$1" \
        | grep -cEv '^\s*(<!--|-->|//|/\*|\*|$)' 2>/dev/null || echo 0
}

tok_html() {
    local chars
    chars=$(awk '/<style[ >]/{skip=1} skip{if(/<\/style>/){skip=0} next} 1' "$1" | wc -c)
    echo $(( chars / 4 ))
}

loc_rust_app() {
    # Stop at server setup boilerplate; exclude imports, tokio attr, fn main decl, blanks, comments
    awk '/^\s*let mut machines = HashMap::new\(\)/{exit} 1' "$1" \
        | grep -cEv '^\s*(use |#\[tokio|async fn main\(\)|//|/\*|\*|$)' 2>/dev/null || echo 0
}

tok_rust_app() {
    local chars
    chars=$(awk '/^\s*let mut machines = HashMap::new\(\)/{exit} 1' "$1" \
        | grep -Ev '^\s*(use |#\[tokio|async fn main\(\)|//|/\*|\*|$)' | wc -c)
    echo $(( chars / 4 ))
}

loc_plain() {
    grep -cEv '^\s*(//|/\*|\*|$|import )' "$1" 2>/dev/null || echo 0
}

tok_plain() {
    local chars; chars=$(wc -c < "$1"); echo $(( chars / 4 ))
}

# ── report helpers ────────────────────────────────────────────────────────────

TOTAL_LOC=0
TOTAL_TOK=0

row() {
    local label="$1" loc="$2" tok="$3"
    printf "    %-52s  %4d loc  ~%5d tokens\n" "$label" "$loc" "$tok"
    TOTAL_LOC=$(( TOTAL_LOC + loc ))
    TOTAL_TOK=$(( TOTAL_TOK + tok ))
}

total_row() {
    printf "    %-52s  %4d loc  ~%5d tokens\n" "TOTAL" "$TOTAL_LOC" "$TOTAL_TOK"
    LAST_LOC=$TOTAL_LOC; LAST_TOK=$TOTAL_TOK
    TOTAL_LOC=0; TOTAL_TOK=0
}

# ── main ──────────────────────────────────────────────────────────────────────

echo ""
echo "═══════════════════════════════════════════════════════════════════"
echo "  Foster vs React — LLM surface area (apples-to-apples)"
echo "  Excludes: CSS, server boilerplate, config, generated test files"
echo "  Token estimate: chars ÷ 4"
echo "═══════════════════════════════════════════════════════════════════"

for app in counter kanban; do
    echo ""
    echo "── $app ──────────────────────────────────────────────────────────────"
    echo ""

    echo "  Foster (implementation — machine + reducers + inline template)"
    row "main.rs (reducers + machine + html! template, no server setup)" \
        "$(loc_rust_app "$REPO/examples/$app/src/main.rs")" \
        "$(tok_rust_app "$REPO/examples/$app/src/main.rs")"
    total_row
    foster_impl_loc=$LAST_LOC; foster_impl_tok=$LAST_TOK

    echo "  Foster (tests — generated from machine definition)"
    printf "    %-52s  %4s loc  ~%5s tokens\n" "*.spec.ts  [generated — not authored]" "0" "0"
    echo ""

    echo "  React (implementation — component + reducer + types)"
    row "App.tsx" \
        "$(loc_plain "$TMPDIR_ROOT/$app/react/src/App.tsx")" \
        "$(tok_plain "$TMPDIR_ROOT/$app/react/src/App.tsx")"
    if [[ "$app" == "kanban" ]]; then
        row "types.ts" \
            "$(loc_plain "$TMPDIR_ROOT/$app/react/src/types.ts")" \
            "$(tok_plain "$TMPDIR_ROOT/$app/react/src/types.ts")"
    fi
    total_row
    react_impl_lok=$LAST_LOC; react_impl_tok=$LAST_TOK

    echo "  React (tests — hand-written, must cover transitions manually)"
    row "App.test.tsx" \
        "$(loc_plain "$TMPDIR_ROOT/$app/react/src/App.test.tsx")" \
        "$(tok_plain "$TMPDIR_ROOT/$app/react/src/App.test.tsx")"
    total_row
    react_test_loc=$LAST_LOC; react_test_tok=$LAST_TOK

    echo ""
    impl_tok_delta=$(( foster_impl_tok - react_impl_tok ))
    net=$(( impl_tok_delta - react_test_tok ))
    printf "  Implementation (Foster vs React):  %+d tokens  (%s)\n" \
        "$impl_tok_delta" "$([ $impl_tok_delta -gt 0 ] && echo "Foster costs more" || echo "Foster costs less")"
    printf "  Tests         (Foster vs React):  -%d tokens  (Foster tests are generated — 0 authored)\n" \
        "$react_test_tok"
    printf "  Net total:                        %+d tokens  (%s)\n" \
        "$net" "$([ $net -gt 0 ] && echo "Foster costs more overall" || echo "Foster costs less overall")"
done

echo ""
echo "═══════════════════════════════════════════════════════════════════"
echo "  Notes"
echo "  • React test coverage is manual (~70-80% of transitions)."
echo "    Foster test coverage is always 100% of edges, by construction."
echo "  • CSS excluded from both sides — equal footing."
echo "  • Server boilerplate (axum setup, tokio::main) excluded from Foster."
echo "  • React has equivalent boilerplate in package.json/vite.config —"
echo "    also excluded."
echo "  • See benchmarks/README.md for qualitative analysis."
echo "═══════════════════════════════════════════════════════════════════"
echo ""
