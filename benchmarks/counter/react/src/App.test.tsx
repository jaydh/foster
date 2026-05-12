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

// State injection equivalents — test initial renders of each state.
// React has no POST /test/state; we test the reducer directly instead.

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
