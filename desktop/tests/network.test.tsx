import { expect, test } from "bun:test";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { Profiler } from "react";
import { fixture } from "./fixtures";
import type { Status } from "../src/api";
import { NetworkSetup } from "../src/components/NetworkSetup";
import { setLanguage } from "../src/i18n";

const fillLink = (link: string) => {
  if (!screen.queryByLabelText("接入链接"))
    fireEvent.click(screen.getByRole("button", { name: "使用链接" }));
  fireEvent.change(screen.getByLabelText("接入链接"), {
    target: { value: link },
  });
};
const submit = async () => {
  await act(async () => {
    fireEvent.submit(document.getElementById("connect-form")!);
  });
};

test("creating on an owned relay directs the device to a manager invitation", async () => {
  await fixture(
    {
      connect: () => {
        throw { code: "RELAY_CLAIMED", message: "Relay is already claimed" };
      },
    },
    false,
  );
  fillLink("https://relay.test");
  expect(screen.getByText("将创建网络，本机成为管理设备")).toBeTruthy();
  expect(screen.queryByLabelText("允许管理设备访问本机")).toBeNull();
  await submit();
  await screen.findByText("中转已绑定网络，请向管理设备获取邀请后加入。");
});

for (const allowManager of [true, false]) {
  test(`invitation joining saves explicit manager consent (${allowManager}) and clears the secret`, async () => {
    const { calls } = await fixture({}, false);
    fillLink(" xrun://test-secret ");
    expect(screen.getByText("将加入已有网络")).toBeTruthy();
    const checkbox = screen.getByLabelText(
      "允许管理设备访问本机",
    ) as HTMLInputElement;
    expect(checkbox.checked).toBe(true);
    if (!allowManager) fireEvent.click(checkbox);
    fireEvent.change(screen.getByLabelText("本机名称"), {
      target: { value: "mac2" },
    });
    await submit();
    await waitFor(() => expect(screen.queryByLabelText("接入链接")).toBeNull());
    expect(calls.find((call) => call.command === "connect")?.args).toEqual({
      link: "xrun://test-secret",
      name: "mac2",
      allowManager,
    });
    expect(document.body.textContent).not.toContain("test-secret");
  });
}

test("link detection replaces separate modes and duplicate submissions are prevented", async () => {
  let complete!: () => void;
  const pending = new Promise<void>((resolve) => {
    complete = resolve;
  });
  let current!: Status;
  const { calls, status } = await fixture(
    {
      connect: async () => {
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
  fillLink("xrun://old-secret");
  expect(screen.getByLabelText("允许管理设备访问本机")).toBeTruthy();
  fillLink("xrun-relay://creation-secret");
  expect(screen.queryByLabelText("允许管理设备访问本机")).toBeNull();
  fireEvent.change(screen.getByLabelText("本机名称"), {
    target: { value: "mac2" },
  });
  fireEvent.submit(document.getElementById("connect-form")!);
  fireEvent.submit(document.getElementById("connect-form")!);
  expect(calls.filter((call) => call.command === "connect")).toHaveLength(1);
  await act(async () => complete());
  await screen.findByRole("button", { name: "邀请设备" });
  expect(calls.find((call) => call.command === "connect")?.args).toEqual({
    link: "xrun-relay://creation-secret",
    name: "mac2",
    allowManager: false,
  });
  expect(document.body.textContent).not.toContain("creation-secret");
});

test("creation can retry after publication fails without discarding its link", async () => {
  let current!: Status;
  let attempts = 0;
  const { calls, status } = await fixture(
    {
      connect: () => {
        current.local.joined = true;
        if (++attempts === 1) throw "CONNECT_FAILED: relay unavailable";
      },
    },
    false,
  );
  current = status;
  fillLink("xrun-relay://retry-secret");
  await submit();
  await screen.findByRole("button", { name: "重试连接" });
  expect((screen.getByLabelText("接入链接") as HTMLInputElement).value).toBe(
    "xrun-relay://retry-secret",
  );
  await submit();
  await waitFor(() => expect(screen.queryByLabelText("接入链接")).toBeNull());
  expect(calls.filter((call) => call.command === "connect")).toHaveLength(2);
});

for (const invitation of [true, false]) {
  test(`registration (${invitation ? "join" : "create"}) retries only service startup after saving identity`, async () => {
    let current!: Status;
    const { calls, status } = await fixture(
      {
        connect: () => {
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
    fillLink(invitation ? "xrun://saved-secret" : "xrun-relay://saved-secret");
    await submit();
    await screen.findByText(
      invitation
        ? "已加入网络，后台服务未启动。请启动服务。"
        : "网络已创建，后台服务未启动。请启动服务。",
    );
    expect(screen.queryByLabelText("接入链接")).toBeNull();
    expect(document.body.textContent).not.toContain("saved-secret");
    await act(async () =>
      fireEvent.click(screen.getByRole("button", { name: "启动服务" })),
    );
    expect(calls.filter((call) => call.command === "connect")).toHaveLength(1);
    expect(calls.filter((call) => call.command === "start")).toHaveLength(1);
    expect(screen.queryByRole("alert")).toBeNull();
  });
}

test("a completed registration never flashes the secret form after a service failure", async () => {
  setLanguage("zh");
  let complete!: (success: boolean) => void;
  const pending = new Promise<boolean>((resolve) => {
    complete = resolve;
  });
  let registered = false;
  const visible: boolean[] = [];
  const form = () => (
    <Profiler
      id="network-setup"
      onRender={() => {
        if (registered)
          visible.push(document.getElementById("connect-form") !== null);
      }}
    >
      <NetworkSetup
        joined={registered}
        startFailed={registered}
        busy={false}
        pending={null}
        action={() => pending}
        onCreated={() => {}}
      />
    </Profiler>
  );
  const view = render(form());
  fillLink("https://relay.test");
  fireEvent.change(screen.getByLabelText("本机名称"), {
    target: { value: "mac2" },
  });
  fireEvent.submit(document.getElementById("connect-form")!);
  registered = true;
  view.rerender(form());
  await act(async () => complete(false));
  expect(visible.length).toBeGreaterThan(0);
  expect(visible.every((value) => !value)).toBe(true);
});
