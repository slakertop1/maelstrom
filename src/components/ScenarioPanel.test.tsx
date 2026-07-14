// ScenarioPanel: the "unset variables" problem must be visible the moment an
// endpoint is checked (inline, per row) — not only after pressing Run.
import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import ScenarioPanel, { looksLikeDatasetTypo } from "./ScenarioPanel";
import { buildRequest, unresolvedVars, builtStrings } from "../requestBuilder";
import { Collection, newRequest } from "../types";

function makeCollection(): Collection {
  const ok = { ...newRequest("healthy"), url: "https://api.example.com/health" };
  const broken = {
    ...newRequest("crops"),
    url: "https://api.example.com/crops?crop_id={{data.crops.crop_id}}",
  };
  return { id: "c1", name: "Svc", requests: [ok, broken] };
}

// The same wiring App.tsx uses for the missingVars prop.
const missingVars = (r: any) => unresolvedVars(builtStrings(buildRequest(r, null)));

function renderPanel(collection = makeCollection()) {
  return render(
    <ScenarioPanel
      collection={collection}
      running={false}
      progress={null}
      progressLog={[]}
      result={null}
      error={null}
      onStart={vi.fn()}
      onStop={vi.fn()}
      onExportHtml={vi.fn()}
      onExportConfig={vi.fn()}
      onClose={vi.fn()}
      tokenRefreshes={0}
      missingVars={missingVars}
    />
  );
}

// The endpoint's raw URL (which also contains the literal {{var}}) is always
// visible in the table, so warnings are asserted via the .scenario-missing
// badge, not by searching the whole document for the var text.
describe("ScenarioPanel inline unset-variables warning", () => {
  it("shows the missing vars right when the endpoint is checked", () => {
    const { container } = renderPanel();
    // Nothing checked → no warning badge anywhere.
    expect(container.querySelector(".scenario-missing")).toBeNull();

    // Check the broken endpoint (second row checkbox).
    const boxes = screen.getAllByRole("checkbox");
    fireEvent.click(boxes[1]);

    // The row now warns inline, without pressing Run.
    const badge = container.querySelector(".scenario-missing");
    expect(badge).not.toBeNull();
    expect(badge!.textContent).toContain("Unset variables");
    expect(badge!.textContent).toContain("{{data.crops.crop_id}}");
    // …and hints that this looks like a dataset reference missing the `$`.
    expect(badge!.textContent).toContain("Looks like a dataset reference");
    // The footer summary counts the problematic selection.
    expect(screen.getByText(/1 selected endpoint/)).toBeInTheDocument();
  });

  it("does not warn for a checked endpoint whose vars all resolve", () => {
    const { container } = renderPanel();
    const boxes = screen.getAllByRole("checkbox");
    fireEvent.click(boxes[0]); // the healthy endpoint
    expect(container.querySelector(".scenario-missing")).toBeNull();
    expect(screen.queryByText(/selected endpoint/)).toBeNull();
  });

  it("clears the warning when the endpoint is unchecked", () => {
    const { container } = renderPanel();
    const boxes = screen.getAllByRole("checkbox");
    fireEvent.click(boxes[1]);
    expect(container.querySelector(".scenario-missing")).not.toBeNull();
    fireEvent.click(boxes[1]);
    expect(container.querySelector(".scenario-missing")).toBeNull();
  });
});

describe("looksLikeDatasetTypo", () => {
  it("flags data./file. prefixes and nothing else", () => {
    expect(looksLikeDatasetTypo("data.crops.crop_id")).toBe(true);
    expect(looksLikeDatasetTypo("file.images")).toBe(true);
    expect(looksLikeDatasetTypo("BaseURL")).toBe(false);
    expect(looksLikeDatasetTypo("database")).toBe(false); // no dot after "data"
  });
});
