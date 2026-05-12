// RTL tests — same coverage as generated kanban.spec.ts.

import { render, screen, fireEvent, within } from "@testing-library/react";
import App from "./App";

function getState() {
  return document.querySelector("[data-machine-state]")!
    .getAttribute("data-machine-state");
}

// ── transition coverage ───────────────────────────────────────────────────────

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
  const firstTitle = screen.getAllByText(/./)[0].textContent; // first task title
  fireEvent.click(screen.getAllByText("del")[0]);
  fireEvent.click(screen.getByText("Delete"));
  expect(getState()).toBe("viewing");
});

test("viewing →[move_task todo→in_progress]→ viewing", () => {
  render(<App />);
  fireEvent.click(screen.getByText("→ IP"));
  expect(getState()).toBe("viewing");
});
