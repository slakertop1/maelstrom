// Full-flow UI tests: render the real <App/>, compose an HTTP request, send it,
// then run a load test — with the Tauri backend (invoke) and events mocked.
import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen, waitFor, act, cleanup, fireEvent } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { HttpResponseData, LoadTestResult } from "./types";

// Shared, hoisted state the mocks and the test both reach.
const h = vi.hoisted(() => ({
  invoke: undefined as unknown as (cmd: string, args?: any) => Promise<any>,
  listeners: {} as Record<string, ((e: { payload: any }) => void)[]>,
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: any) => h.invoke(cmd, args),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: (name: string, cb: (e: { payload: any }) => void) => {
    (h.listeners[name] ||= []).push(cb);
    return Promise.resolve(() => {});
  },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ save: vi.fn(), open: vi.fn() }));

import App from "./App";

function emit(name: string, payload: any) {
  act(() => {
    (h.listeners[name] || []).forEach((cb) => cb({ payload }));
  });
}

const fakeResponse: HttpResponseData = {
  status: 200,
  status_text: "OK",
  headers: [["content-type", "application/json"]],
  body: '{"id":1,"title":"hello"}',
  body_base64: false,
  size_bytes: 24,
  duration_ms: 42,
};

const fakeLoadResult: LoadTestResult = {
  url: "https://api.example.com",
  method: "GET",
  vus: 20,
  duration_secs: 30,
  rps_limit: null,
  started_at: "2026-07-04 15:00:00",
  actual_duration_ms: 30000,
  total_requests: 12345,
  errors: 0,
  error_rate: 0,
  rps_avg: 411,
  latency_min_ms: 2,
  latency_max_ms: 300,
  latency_avg_ms: 40,
  p50_ms: 36,
  p75_ms: 55,
  p90_ms: 90,
  p95_ms: 120,
  p99_ms: 260,
  status_counts: [["200", 12345]],
  timeline: [
    { sec: 1, requests: 400, errors: 0, avg_ms: 40, p50_ms: 36, p95_ms: 120, p99_ms: 260 },
  ],
  histogram: [{ from_ms: 0, to_ms: 50, count: 12345 }],
  stopped_early: false,
};

beforeEach(() => {
  h.listeners = {};
  cleanup();
});

describe("full flow: compose request → send → load test", () => {
  it("sends the composed HTTP request and shows the response", async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === "load_state") return ""; // no saved state → defaults
      if (cmd === "send_request") return fakeResponse;
      return undefined;
    });
    h.invoke = invoke as any;
    const user = userEvent.setup();

    render(<App />);

    // App loads the default example request into the URL field.
    const url = (await screen.findByPlaceholderText(/api\.example\.com/i)) as HTMLInputElement;
    await user.clear(url);
    await user.type(url, "https://api.example.com/users/1");

    await user.click(screen.getByRole("button", { name: "Send" }));

    // Backend was called with our request.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        "send_request",
        expect.objectContaining({
          spec: expect.objectContaining({ method: "GET", url: "https://api.example.com/users/1" }),
        })
      );
    });

    // Response is rendered.
    expect(await screen.findByText(/200/)).toBeInTheDocument();
    expect(await screen.findByText(/hello/)).toBeInTheDocument();
  });

  it("warns before sending when the URL has an unset {{variable}}", async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === "load_state") return "";
      if (cmd === "send_request") return fakeResponse;
      return undefined;
    });
    h.invoke = invoke as any;
    const user = userEvent.setup();

    render(<App />);
    const url = (await screen.findByPlaceholderText(/api\.example\.com/i)) as HTMLInputElement;
    // set a URL containing an unresolved env variable (fireEvent avoids userEvent's {{ parsing)
    fireEvent.change(url, { target: { value: "https://{{BaseURL}}/v1/orders" } });

    await user.click(screen.getByRole("button", { name: "Send" }));

    // Warning modal lists the missing variable and the request is NOT sent yet.
    expect(await screen.findByText(/Unset variables/)).toBeInTheDocument();
    expect(screen.getByText("{{BaseURL}}")).toBeInTheDocument();
    expect(invoke).not.toHaveBeenCalledWith("send_request", expect.anything());

    // "Anyway" proceeds with the send.
    await user.click(screen.getByRole("button", { name: "Anyway" }));
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("send_request", expect.anything());
    });
  });

  it("gRPC: load methods → pick → call → shows response", async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === "load_state") return "";
      if (cmd === "grpc_list_methods")
        return [
          {
            service: "demo.Greeter",
            method: "SayHello",
            path: "/demo.Greeter/SayHello",
            client_streaming: false,
            server_streaming: false,
            input_type: "demo.HelloRequest",
            output_type: "demo.HelloReply",
          },
        ];
      if (cmd === "grpc_request_template") return '{\n  "name": ""\n}';
      if (cmd === "grpc_call")
        return { responses: ['{"message":"Привет, Мир!"}'], server_streaming: false, duration_ms: 7 };
      return undefined;
    });
    h.invoke = invoke as any;
    const user = userEvent.setup();

    render(<App />);
    await screen.findByPlaceholderText(/api\.example\.com/i);

    // Switch request kind to gRPC.
    await user.click(screen.getByRole("button", { name: "gRPC" }));

    // Point at a .proto and load its methods.
    const proto = (await screen.findByPlaceholderText(/service\.proto/i)) as HTMLInputElement;
    fireEvent.change(proto, { target: { value: "C:/svc/greeter.proto" } });
    await user.click(screen.getByRole("button", { name: /Load methods/ }));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        "grpc_list_methods",
        expect.objectContaining({ proto: expect.objectContaining({ proto_path: "C:/svc/greeter.proto" }) })
      );
    });

    // Pick the method (the dropdown is now populated).
    const grpcSelect = (await waitFor(() => {
      const s = screen
        .getAllByRole("combobox")
        .find((el) =>
          Array.from((el as HTMLSelectElement).options).some((o) =>
            o.textContent?.includes("SayHello")
          )
        );
      if (!s) throw new Error("method option not ready");
      return s as HTMLSelectElement;
    })) as HTMLSelectElement;
    await user.selectOptions(grpcSelect, "demo.Greeter::SayHello");

    // Body gets prefilled from the request template.
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        "grpc_request_template",
        expect.objectContaining({ service: "demo.Greeter", method: "SayHello" })
      );
    });

    // Enter the endpoint and call.
    const endpoint = screen.getByPlaceholderText(/localhost:50051/i) as HTMLInputElement;
    fireEvent.change(endpoint, { target: { value: "http://127.0.0.1:50055" } });
    await user.click(screen.getByRole("button", { name: "Call" }));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        "grpc_call",
        expect.objectContaining({
          spec: expect.objectContaining({
            endpoint: "http://127.0.0.1:50055",
            service: "demo.Greeter",
            method: "SayHello",
          }),
        })
      );
    });

    expect(await screen.findByText(/Привет, Мир!/)).toBeInTheDocument();
  });

  it("starts a load test and renders the finished report", async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === "load_state") return "";
      if (cmd === "start_load_test") return undefined;
      return undefined;
    });
    h.invoke = invoke as any;
    const user = userEvent.setup();

    render(<App />);
    await screen.findByPlaceholderText(/api\.example\.com/i);

    // Switch to the load-test tab.
    await user.click(screen.getByText(/⚡ Load/));
    // Start the test.
    await user.click(await screen.findByRole("button", { name: /Run test/ }));

    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("start_load_test", expect.anything());
    });

    // Backend streams progress, then a final result.
    emit("load_progress", {
      elapsed_secs: 1,
      total_requests: 400,
      errors: 0,
      rps_current: 400,
      avg_ms: 40,
      p50_ms: 36,
      p95_ms: 120,
      p99_ms: 260,
      max_ms: 300,
      point: fakeLoadResult.timeline[0],
    });
    emit("load_finished", fakeLoadResult);

    // The finished banner appears, then the aggregate numbers (total requests
    // and the 200-status count both render as "12.3k").
    await screen.findByText(/Test finished/, {}, { timeout: 3000 });
    expect(screen.getAllByText("12.3k").length).toBeGreaterThan(0);
  });

  it("exports the current request as a CLI scenario from the Load tab", async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === "load_state") return "";
      return undefined;
    });
    h.invoke = invoke as any;
    const user = userEvent.setup();

    render(<App />);
    await screen.findByPlaceholderText(/api\.example\.com/i);

    // Load tab → "↓ Export for CI" opens the export dialog for the current request.
    await user.click(screen.getByText(/⚡ Load/));
    await user.click(await screen.findByRole("button", { name: /Export for CI/ }));

    expect(await screen.findByRole("button", { name: /Save config/ })).toBeInTheDocument();
  });

  it("saves an auth profile in one request and applies it in another", async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === "load_state") return "";
      return undefined;
    });
    h.invoke = invoke as any;
    const user = userEvent.setup();

    render(<App />);
    await screen.findByPlaceholderText(/api\.example\.com/i);

    // Request 1: configure Bearer auth and save it as a profile.
    await user.click(screen.getByText("Auth"));
    const typeSelect = screen
      .getAllByRole("combobox")
      .find((s) =>
        Array.from((s as HTMLSelectElement).options).some((o) => o.value === "bearer")
      ) as HTMLSelectElement;
    await user.selectOptions(typeSelect, "bearer");
    const tokenInput = await screen.findByPlaceholderText(/\{\{token\}\}/);
    fireEvent.change(tokenInput, { target: { value: "sekret-token-123" } });
    await user.click(screen.getByRole("button", { name: /Save as profile/ }));

    // Request 2: a fresh request has no auth…
    await user.click(screen.getByTitle(/Add request/));
    await user.click(screen.getByText("Auth"));
    expect(screen.queryByPlaceholderText(/\{\{token\}\}/)).toBeNull();

    // …apply the saved profile from the dropdown.
    const profileSelect = screen
      .getAllByRole("combobox")
      .find((s) =>
        Array.from((s as HTMLSelectElement).options).some((o) =>
          o.textContent?.includes("bearer …-123")
        )
      ) as HTMLSelectElement;
    expect(profileSelect).toBeTruthy();
    const opt = Array.from(profileSelect.options).find((o) =>
      o.textContent?.includes("bearer …-123")
    )!;
    await user.selectOptions(profileSelect, opt.value);
    await user.click(screen.getByRole("button", { name: "Apply" }));

    // The second request now carries the profile's credentials.
    const applied = (await screen.findByPlaceholderText(/\{\{token\}\}/)) as HTMLInputElement;
    expect(applied.value).toBe("sekret-token-123");
  });

  it("a token fetched while the user keeps editing does not revert the edits", async () => {
    // The OAuth fetch hangs until we resolve it — simulating a slow IdP/SSO.
    let resolveToken!: (v: unknown) => void;
    const tokenPromise = new Promise((res) => (resolveToken = res));
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === "load_state") return "";
      if (cmd === "fetch_oauth_token") return tokenPromise;
      return undefined;
    });
    h.invoke = invoke as any;
    const user = userEvent.setup();

    render(<App />);
    const url = (await screen.findByPlaceholderText(/api\.example\.com/i)) as HTMLInputElement;

    // Configure OAuth2 (client_credentials) with a token URL and start the fetch.
    await user.click(screen.getByText("Auth"));
    const typeSelect = screen
      .getAllByRole("combobox")
      .find((s) =>
        Array.from((s as HTMLSelectElement).options).some((o) => o.value === "oauth2")
      ) as HTMLSelectElement;
    await user.selectOptions(typeSelect, "oauth2");
    const tokenUrl = await screen.findByPlaceholderText(/oauth\/token/);
    fireEvent.change(tokenUrl, { target: { value: "https://idp.example.com/oauth/token" } });
    await user.click(screen.getByRole("button", { name: /Get token/ }));

    // While the fetch is in flight the user keeps editing the request.
    fireEvent.change(url, { target: { value: "https://api.example.com/EDIT-DURING-FETCH" } });

    // The token arrives late.
    await act(async () => {
      resolveToken({
        access_token: "tok-1",
        token_type: "Bearer",
        expires_in: 3600,
        refresh_token: null,
        scope: null,
      });
    });

    // The token landed AND the mid-flight edit survived (the old code merged a
    // stale snapshot of the whole request back, reverting the URL).
    await screen.findByText(/Token active/);
    expect(url.value).toBe("https://api.example.com/EDIT-DURING-FETCH");
  });

  it("keeps unsaved edits when switching to another request and back", async () => {
    const invoke = vi.fn(async (cmd: string) => {
      if (cmd === "load_state") return "";
      return undefined;
    });
    h.invoke = invoke as any;
    const user = userEvent.setup();

    render(<App />);
    const url = (await screen.findByPlaceholderText(/api\.example\.com/i)) as HTMLInputElement;

    // Edit the current request WITHOUT pressing Save…
    fireEvent.change(url, { target: { value: "https://api.example.com/EDITED" } });

    // …switch away to a brand-new request (the editor shows a blank URL)…
    await user.click(screen.getByTitle(/Add request/));
    await waitFor(() => expect(url.value).toBe(""));

    // …and come back: the edit must have been auto-saved, not reset.
    await user.click(screen.getByText(/Example: JSON API/));
    await waitFor(() => expect(url.value).toBe("https://api.example.com/EDITED"));
  });
});
