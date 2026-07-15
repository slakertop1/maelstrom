import { describe, it, expect } from "vitest";
import { fmtNum, fmtMs, lineChart } from "./charts";

describe("formatting", () => {
  it("fmtNum abbreviates and rounds", () => {
    expect(fmtNum(5)).toBe("5");
    expect(fmtNum(1234)).toBe("1234");
    expect(fmtNum(15000)).toBe("15.0k");
    expect(fmtNum(2_000_000)).toBe("2.0M");
  });

  it("fmtMs switches to seconds past 1000ms", () => {
    expect(fmtMs(42)).toContain("ms");
    expect(fmtMs(2500)).toContain(" s");
  });
});

describe("lineChart y-axis never overflows the plot (regression)", () => {
  // The axis top must always be >= the peak so the line stays inside the box.
  function maxY(svg: string): { top: number; minPointY: number } {
    // plot top padding is 30 in lineChart
    const pts = [...svg.matchAll(/<(?:polyline|polygon) points="([^"]+)"/g)]
      .flatMap((m) => m[1].trim().split(/\s+/))
      .map((pair) => parseFloat(pair.split(",")[1]))
      .filter((n) => !isNaN(n));
    return { top: 30, minPointY: Math.min(...pts) };
  }

  for (const peak of [1, 7, 42, 105, 999, 1234, 55000]) {
    it(`fits peak ${peak}`, () => {
      const svg = lineChart({
        series: [{ name: "s", color: "#000", points: [{ x: 0, y: 0 }, { x: 1, y: peak }, { x: 2, y: peak * 0.9 }] }],
      });
      const { top, minPointY } = maxY(svg);
      // no plotted point sits above the top of the plotting area
      expect(minPointY).toBeGreaterThanOrEqual(top - 0.5);
    });
  }
});

describe("lineChart drops non-finite points instead of corrupting the SVG (e4)", () => {
  function pointStrings(svg: string): string[] {
    return [...svg.matchAll(/<(?:polyline|polygon) points="([^"]+)"/g)].map((m) => m[1]);
  }

  it("filters out NaN/Infinity points from a polyline", () => {
    const svg = lineChart({
      series: [
        {
          name: "s",
          color: "#000",
          points: [
            { x: 0, y: 10 },
            { x: 1, y: NaN },
            { x: 2, y: Infinity },
            { x: NaN, y: 5 },
            { x: 3, y: 20 },
          ],
        },
      ],
    });
    for (const attr of pointStrings(svg)) {
      expect(attr).not.toMatch(/NaN/);
      expect(attr).not.toMatch(/Infinity/);
      for (const pair of attr.trim().split(/\s+/)) {
        const [px, py] = pair.split(",").map(Number);
        expect(Number.isFinite(px)).toBe(true);
        expect(Number.isFinite(py)).toBe(true);
      }
    }
  });

  it("filters out non-finite points from a filled polygon too", () => {
    const svg = lineChart({
      series: [
        {
          name: "s",
          color: "#000",
          fill: true,
          points: [
            { x: 0, y: 10 },
            { x: 1, y: NaN },
            { x: 2, y: 30 },
          ],
        },
      ],
    });
    const polygon = pointStrings(svg)[0];
    expect(polygon).not.toMatch(/NaN/);
  });

  it("renders an empty but valid chart when every point is non-finite", () => {
    const svg = lineChart({
      series: [{ name: "s", color: "#000", points: [{ x: NaN, y: NaN }, { x: Infinity, y: -Infinity }] }],
    });
    expect(svg).not.toMatch(/NaN/);
    expect(svg).not.toMatch(/Infinity/);
    expect(svg.startsWith("<svg")).toBe(true);
  });

  it("a single bad point does not corrupt the surrounding valid points", () => {
    const svg = lineChart({
      series: [
        {
          name: "s",
          color: "#000",
          points: [
            { x: 0, y: 5 },
            { x: 1, y: NaN },
            { x: 2, y: 15 },
          ],
        },
      ],
    });
    const attr = pointStrings(svg).find((p) => !p.includes("polygon")) ?? pointStrings(svg)[0];
    // exactly the two finite points should remain
    expect(attr.trim().split(/\s+/).length).toBe(2);
  });
});
