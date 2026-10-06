import { expect, test } from "bun:test";
import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { fixture } from "./fixtures";
import type { Status } from "../src/api";
test("joining passes the link only to join and clears it from the form", async () => {
  const { calls } = await fixture({}, false);
  const link = screen.getByLabelText("邀请链接") as HTMLInputElement;
  fireEvent.change(link, { target: { value: "xrun://test-secret" } });
  fireEvent.change(screen.getByLabelText("本机名称"), {
    target: { value: "mac2" },
  });
  await act(async () => {
    fireEvent.submit(document.getElementById("join-form")!);
  });
  await waitFor(() => expect(screen.queryByLabelText("邀请链接")).toBeNull());
  expect(calls.find((call) => call.command === "join")?.args).toEqual({
    link: "xrun://test-secret",
    name: "mac2",
  });
  expect(document.body.textContent).not.toContain("test-secret");
});

test("network creation clears secrets, prevents duplicate submissions and opens member management", async () => {
  let complete!: () => void;
  const pending = new Promise<void>((resolve) => {
    complete = resolve;
  });
  let current!: Status;
  const { calls, status } = await fixture(
    {
      create_network: async () => {
        await pending;
        current.local.joined = true;
        current.network = {
          network_id: "new-network",
          manager_id: "self",
          manager_name: "mac2",
          is_manager: true,
          relay_addresses: ["https://relay.example:8080"],
        };
      },
    },
    false,
  );
  current = status;
  fireEvent.change(screen.getByLabelText("邀请链接"), {
    target: { value: "xrun://old-secret" },
  });
  fireEvent.click(screen.getByRole("button", { name: "创建新网络" }));
  const link = screen.getByLabelText("中转部署链接") as HTMLInputElement;
  expect(link.value).toBe("");
  fireEvent.change(link, { target: { value: "xrun-relay://creation-secret" } });
  fireEvent.change(screen.getByLabelText("本机名称"), {
    target: { value: "mac2" },
  });
  fireEvent.submit(document.getElementById("create-network-form")!);
  fireEvent.submit(document.getElementById("create-network-form")!);
  expect(
    calls.filter((call) => call.command === "create_network"),
  ).toHaveLength(1);
  await act(async () => complete());
  await waitFor(() =>
    expect(screen.queryByLabelText("中转部署链接")).toBeNull(),
  );
  expect(
    document
      .querySelector('[data-page="devices"]')
      ?.getAttribute("aria-current"),
  ).toBe("page");
  expect(calls.find((call) => call.command === "create_network")?.args).toEqual(
    {
      link: "xrun-relay://creation-secret",
      name: "mac2",
    },
  );
  expect(document.body.textContent).not.toContain("creation-secret");
  expect(calls.some((call) => call.command === "join")).toBe(false);
});

test("creation can retry after an identity was saved but publication failed", async () => {
  let current!: Status;
  let attempts = 0;
  const { calls, status } = await fixture(
    {
      create_network: () => {
        current.local.joined = true;
        if (++attempts === 1) throw "CONNECT_FAILED: relay unavailable";
      },
    },
    false,
  );
  current = status;
  fireEvent.click(screen.getByRole("button", { name: "创建新网络" }));
  fireEvent.change(screen.getByLabelText("中转部署链接"), {
    target: { value: "xrun-relay://retry-secret" },
  });
  fireEvent.change(screen.getByLabelText("本机名称"), {
    target: { value: "mac2" },
  });
  fireEvent.submit(document.getElementById("create-network-form")!);
  await screen.findByRole("button", { name: "重试创建网络" });
  expect(
    (screen.getByLabelText("中转部署链接") as HTMLInputElement).value,
  ).toBe("xrun-relay://retry-secret");
  fireEvent.submit(document.getElementById("create-network-form")!);
  await waitFor(() =>
    expect(screen.queryByLabelText("中转部署链接")).toBeNull(),
  );
  expect(
    calls.filter((call) => call.command === "create_network"),
  ).toHaveLength(2);
});

for (const command of ["join", "create_network"] as const) {
  test(`${command} clears the link after registration and retries only service startup`, async () => {
    let current!: Status;
    const { calls, status } = await fixture(
      {
        [command]: () => {
          current.local.joined = true;
          current.local.daemon_running = false;
          current.local.daemon_connected = false;
          current.error = {
            code: "SERVICE_START_FAILED",
            message: "system approval required",
          };
          throw current.error;
        },
        start: () => {
          current.local.daemon_running = true;
          current.error = null;
        },
      },
      false,
    );
    current = status;
    if (command === "create_network")
      fireEvent.click(screen.getByRole("button", { name: "创建新网络" }));
    fireEvent.change(
      screen.getByLabelText(command === "join" ? "邀请链接" : "中转部署链接"),
      { target: { value: "xrun://registered-secret" } },
    );
    fireEvent.change(screen.getByLabelText("本机名称"), {
      target: { value: "mac2" },
    });
    fireEvent.submit(
      document.getElementById(
        command === "join" ? "join-form" : "create-network-form",
      )!,
    );
    await screen.findByText(
      command === "join"
        ? "已加入网络，后台服务未启动。请启动后台服务。"
        : "网络已创建，后台服务未启动。请启动后台服务。",
    );
    await waitFor(() =>
      expect(
        screen.queryByLabelText(
          command === "join" ? "邀请链接" : "中转部署链接",
        ),
      ).toBeNull(),
    );
    expect(document.body.textContent).not.toContain("registered-secret");
    fireEvent.click(screen.getByRole("button", { name: "启动后台服务" }));
    await screen.findByText("后台服务已启动");
    expect(calls.filter((call) => call.command === command)).toHaveLength(1);
    expect(calls.filter((call) => call.command === "start")).toHaveLength(1);
    expect(screen.queryByRole("alert")).toBeNull();
  });
}
