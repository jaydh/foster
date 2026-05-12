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

// ── missing import ────────────────────────────────────────────────────────────
// Intentionally omitted so this file is framework-agnostic readable.
// In a real project: import { useReducer } from "react";
import { useReducer } from "react";
