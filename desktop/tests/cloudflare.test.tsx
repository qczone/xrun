import { expect, test } from "bun:test";
import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { fixture, selectOption } from "./fixtures";
import type { DeploymentProgress, RelayDeployment, Status } from "../src/api";

const account = { id: "a".repeat(32), name: "个人账户" };
const relay: RelayDeployment = {
  provider: "cloudflare",
  name: "xrun-relay",
  account_id: account.id,
  link: "https://xrun-relay.example.workers.dev",
};
let eventSequence = 0;
async function deploymentEvent(
  args: Record<string, unknown>,
  stage: DeploymentProgress["stage"],
  status: DeploymentProgress["status"] = "running",
  extra: Partial<DeploymentProgress> = {},
) {
  await emit("xrun-deployment-progress", {
    event: "deployment_progress",
    operation_id: args.progressId,
    provider: "cloudflare",
    operation: "create_cloudflare_network",
    sequence: ++eventSequence,
    timestamp_ms: Date.now(),
    stage,
    status,
    elapsed_ms: 1000,
    stage_elapsed_ms: 500,
    ...extra,
  });
}
const create = async () => {
  await act(async () =>
    fireEvent.submit(document.getElementById("cloudflare-create-form")!),
  );
};

test("Cloudflare creates a relay and manager with one account without exposing its private link", async () => {
  const { calls } = await fixture(
    {
      cloudflare_accounts: () => [account],
      create_cloudflare_network: () => relay,
    },
    false,
  );
  fireEvent.click(screen.getByRole("button", { name: "新建网络" }));
  fireEvent.change(screen.getByLabelText("本机名称"), {
    target: { value: "mac-pro" },
  });
  await create();
  await screen.findByRole("button", { name: "邀请设备" });
  expect(
    calls.find((call) => call.command === "create_cloudflare_network")?.args,
  ).toMatchObject({
    workerName: "xrun-relay",
    account: account.id,
    deviceName: "mac-pro",
    region: "auto",
    resume: false,
  });
  expect(document.body.textContent).not.toContain("secret-deployment-route");
});

test("multiple Cloudflare accounts require an explicit choice before deployment", async () => {
  const second = { id: "b".repeat(32), name: "团队账户" };
  const { calls } = await fixture(
    {
      cloudflare_accounts: () => [account, second],
      create_cloudflare_network: () => relay,
    },
    false,
  );
  fireEvent.click(screen.getByRole("button", { name: "新建网络" }));
  await create();
  const select = await screen.findByLabelText("Cloudflare 账户");
  expect(
    calls.some((call) => call.command === "create_cloudflare_network"),
  ).toBe(false);
  await selectOption(select, second.id);
  await create();
  expect(
    calls.find((call) => call.command === "create_cloudflare_network")?.args
      .account,
  ).toBe(second.id);
  expect(
    calls.filter((call) => call.command === "cloudflare_accounts"),
  ).toHaveLength(1);
});

test("deployment survives a connection failure and retry does not redeploy the Worker", async () => {
  let attempts = 0;
  let current!: Status;
  const { calls, status } = await fixture(
    {
      cloudflare_accounts: () => [account],
      create_cloudflare_network: async (args) => {
        await deploymentEvent(args, "deployed", "completed", {
          deployment: relay,
        });
        if (++attempts === 1) throw "CONNECT_TIMEOUT: unavailable";
        current.local.joined = true;
        return relay;
      },
    },
    false,
  );
  current = status;
  fireEvent.click(screen.getByRole("button", { name: "新建网络" }));
  await create();
  await screen.findByText("中转已部署，重试时将继续创建网络。");
  expect(document.body.textContent).not.toContain("secret-deployment-route");
  await act(async () =>
    fireEvent.click(screen.getByRole("button", { name: "复制部署链接" })),
  );
  expect(
    calls.find((call) => call.command === "copy_identifier")?.args.value,
  ).toBe(relay.link);
  await create();
  expect(
    calls.filter((call) => call.command === "create_cloudflare_network"),
  ).toHaveLength(2);
  expect(
    calls
      .filter((call) => call.command === "create_cloudflare_network")
      .map((call) => call.args.resume),
  ).toEqual([false, true]);
});

test("authorization can reopen the browser and cancel without deployment or saved identity", async () => {
  let finish!: (value: null) => void;
  const authorization = new Promise<null>((resolve) => {
    finish = resolve;
  });
  const { calls } = await fixture(
    {
      cloudflare_accounts: async () => {
        await emit(
          "xrun-cloudflare-authorization",
          "https://dash.cloudflare.com/oauth2/auth?state=test",
        );
        return authorization;
      },
      cancel_cloudflare_authorization: () => {
        finish(null);
      },
    },
    false,
  );
  fireEvent.click(screen.getByRole("button", { name: "新建网络" }));
  fireEvent.submit(document.getElementById("cloudflare-create-form")!);
  await screen.findByText("请在浏览器完成授权。");
  await act(async () =>
    fireEvent.click(screen.getByRole("button", { name: "打开授权页面" })),
  );
  await act(async () =>
    fireEvent.click(screen.getByRole("button", { name: "取消授权" })),
  );
  await waitFor(() =>
    expect(
      (screen.getByRole("button", { name: "授权并创建" }) as HTMLButtonElement)
        .disabled,
    ).toBe(false),
  );
  expect(
    calls.some((call) => call.command === "open_cloudflare_authorization"),
  ).toBe(true);
  expect(
    calls.some(
      (call) =>
        call.command === "create_cloudflare_network" ||
        call.command === "connect",
    ),
  ).toBe(false);
});

test("saved deployment display copies a full private link only on request", async () => {
  const { calls, page } = await fixture({ cloudflare_relays: () => [relay] });
  await page("设置");
  fireEvent.click(screen.getByText("网络与服务", { selector: "summary span" }));
  await screen.findByText("本机部署的中转");
  expect(document.body.textContent).not.toContain("secret-deployment-route");
  await act(async () =>
    fireEvent.click(
      screen.getByRole("button", { name: "复制 xrun-relay 的部署链接" }),
    ),
  );
  expect(
    calls.find((call) => call.command === "copy_identifier")?.args.value,
  ).toBe(relay.link);
});

test("expired or revoked login can be reauthorized from the desktop without deploying", async () => {
  const { calls } = await fixture(
    {
      cloudflare_accounts: ({ reauthorize }) => {
        if (!reauthorize)
          throw { code: "UNAUTHENTICATED", message: "login revoked" };
        return [account];
      },
    },
    false,
  );
  fireEvent.click(screen.getByRole("button", { name: "新建网络" }));
  await create();
  await act(async () =>
    fireEvent.click(screen.getByRole("button", { name: "重新授权" })),
  );
  expect(
    calls
      .filter((call) => call.command === "cloudflare_accounts")
      .map((call) => call.args.reauthorize),
  ).toEqual([false, true]);
  expect(
    calls.some((call) => call.command === "create_cloudflare_network"),
  ).toBe(false);
});

test("authorization cleanup failure before deployment never reports a created network", async () => {
  await fixture(
    {
      cloudflare_accounts: () => {
        throw {
          code: "OAUTH_REVOCATION_FAILED",
          message: "retry authorization cleanup",
        };
      },
    },
    false,
  );
  fireEvent.click(screen.getByRole("button", { name: "新建网络" }));
  await create();
  await screen.findByText("网络未能创建，请查看详情后重试。");
  expect(screen.queryByText("网络已创建，授权清理未完成。")).toBeNull();
  expect(screen.queryByRole("button", { name: "邀请设备" })).toBeNull();
});

test("manager settings explicitly opt into remembered login and keep it when revocation fails", async () => {
  let remembered = false;
  let failLogout = true;
  const { calls, status, poll, page } = await fixture({
    cloudflare_remembered_login: () => remembered,
    cloudflare_accounts: ({ remember }) => {
      remembered = !!remember;
      return [account];
    },
    logout_cloudflare: () => {
      if (failLogout)
        throw { code: "OAUTH_REVOCATION_FAILED", message: "retry logout" };
      remembered = false;
    },
  });
  status.network!.is_manager = true;
  await poll();
  await page("设置");
  const option = (await screen.findByLabelText(
    "记住登录（管理设备）",
  )) as HTMLInputElement;
  expect(option.checked).toBe(false);
  fireEvent.click(option);
  fireEvent.click(screen.getByRole("button", { name: "授权账号" }));
  await screen.findByText("账号已授权");
  expect(
    calls.find((call) => call.command === "cloudflare_accounts")?.args,
  ).toMatchObject({ reauthorize: true, remember: true });
  fireEvent.click(screen.getByRole("button", { name: "退出授权" }));
  await screen.findByText("授权未能撤销，请重试。");
  expect(option.checked).toBe(true);
  expect(remembered).toBe(true);
  failLogout = false;
  fireEvent.click(screen.getByRole("button", { name: "退出授权" }));
  await screen.findByText("授权已撤销");
  expect(remembered).toBe(false);
});

test("settings keep temporary authorization and logout progress visible until cleanup finishes", async () => {
  let authorized!: (value: (typeof account)[]) => void;
  let loggedOut!: () => void;
  const authorization = new Promise<(typeof account)[]>((resolve) => {
    authorized = resolve;
  });
  const logout = new Promise<void>((resolve) => {
    loggedOut = resolve;
  });
  const { calls, status, poll, page } = await fixture({
    cloudflare_accounts: async (args) => {
      await deploymentEvent(args, "authorization_received", "completed");
      await deploymentEvent(args, "authorization_cleanup");
      return authorization;
    },
    logout_cloudflare: async (args) => {
      await deploymentEvent(args, "authorization_cleanup");
      return logout;
    },
  });
  status.network!.is_manager = true;
  await poll();
  await page("设置");
  fireEvent.click(screen.getByRole("button", { name: "授权账号" }));
  await screen.findByText(/正在撤销授权/);
  expect(screen.queryByRole("button", { name: "打开授权页面" })).toBeNull();
  expect(screen.queryByText("账号已授权")).toBeNull();
  expect(
    calls.find((call) => call.command === "cloudflare_accounts")?.args,
  ).toMatchObject({ remember: false, finishTemporary: true });
  await act(async () => {
    authorized([account]);
  });
  await screen.findByText("账号已授权");
  expect(
    calls.some((call) => call.command === "cancel_cloudflare_authorization"),
  ).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "退出授权" }));
  await screen.findByText(/正在撤销授权/);
  await act(async () => {
    loggedOut();
  });
  await screen.findByText("授权已撤销");
});

test("Cloudflare deployment passes an explicitly selected region to the native deployer", async () => {
  const { calls } = await fixture(
    {
      cloudflare_accounts: () => [account],
      create_cloudflare_network: () => relay,
    },
    false,
  );
  fireEvent.click(screen.getByRole("button", { name: "新建网络" }));
  const advanced =
    document.querySelector<HTMLDetailsElement>(".setup-advanced")!;
  await act(async () => {
    advanced.open = true;
  });
  await selectOption(screen.getByLabelText("中转区域"), "apac-ne");
  await create();
  expect(
    calls.find((call) => call.command === "create_cloudflare_network")?.args
      .region,
  ).toBe("apac-ne");
});

test("authorization return advances progress and removes browser controls before account loading finishes", async () => {
  let finish!: (value: (typeof account)[]) => void;
  let args!: Record<string, unknown>;
  const waiting = new Promise<(typeof account)[]>((resolve) => {
    finish = resolve;
  });
  await fixture(
    {
      cloudflare_accounts: async (request) => {
        args = request;
        await deploymentEvent(request, "authorization", "waiting");
        return waiting;
      },
      create_cloudflare_network: () => relay,
    },
    false,
  );
  fireEvent.click(screen.getByRole("button", { name: "新建网络" }));
  fireEvent.submit(document.getElementById("cloudflare-create-form")!);
  await screen.findByRole("button", { name: "打开授权页面" });
  await act(async () => {
    await deploymentEvent(args, "authorization_received", "completed");
    await deploymentEvent(args, "token_exchange");
    await deploymentEvent(args, "authorization", "waiting", { sequence: 1 });
  });
  await screen.findByText(/正在验证授权/);
  expect(screen.queryByRole("button", { name: "打开授权页面" })).toBeNull();
  expect(screen.queryByRole("button", { name: "取消授权" })).toBeNull();
  await act(async () => {
    finish([account]);
  });
  await screen.findByRole("button", { name: "邀请设备" });
});

test("network progress wins over concurrent cleanup and stale operations cannot replace it", async () => {
  let finish!: (value: typeof relay) => void;
  let args!: Record<string, unknown>;
  const waiting = new Promise<typeof relay>((resolve) => {
    finish = resolve;
  });
  await fixture(
    {
      cloudflare_accounts: () => [account],
      create_cloudflare_network: async (request) => {
        args = request;
        await deploymentEvent(request, "deployed", "completed", {
          deployment: relay,
        });
        await deploymentEvent(request, "authorization_cleanup");
        await deploymentEvent(request, "network_creation");
        return waiting;
      },
    },
    false,
  );
  fireEvent.click(screen.getByRole("button", { name: "新建网络" }));
  fireEvent.submit(document.getElementById("cloudflare-create-form")!);
  await screen.findByText(/正在创建网络 ·/);
  await act(async () => {
    await deploymentEvent({ progressId: "stale-operation" }, "worker_upload");
  });
  expect(screen.queryByText(/正在部署中转 ·/)).toBeNull();
  await act(async () => {
    await deploymentEvent(args, "network_creation", "completed");
  });
  await screen.findByText(/正在撤销授权 ·/);
  await act(async () => {
    finish(relay);
  });
  await screen.findByRole("button", { name: "邀请设备" });
});

test("saved relays resume without Cloudflare authorization or a new upload", async () => {
  const { calls } = await fixture(
    {
      cloudflare_relays: () => [relay],
      create_cloudflare_network: () => relay,
    },
    false,
  );
  fireEvent.click(screen.getByRole("button", { name: "新建网络" }));
  await selectOption(
    await screen.findByLabelText("继续使用已有中转"),
    relay.name,
  );
  await create();
  expect(calls.some((call) => call.command === "cloudflare_accounts")).toBe(
    false,
  );
  expect(
    calls.find((call) => call.command === "create_cloudflare_network")?.args
      .resume,
  ).toBe(true);
  await screen.findByRole("button", { name: "邀请设备" });
});

test("a name conflict opens the name field instead of replacing the existing Worker", async () => {
  const { calls } = await fixture(
    {
      cloudflare_accounts: () => [account],
      create_cloudflare_network: () => {
        throw { code: "ALREADY_EXISTS", message: "Worker name in use" };
      },
    },
    false,
  );
  fireEvent.click(screen.getByRole("button", { name: "新建网络" }));
  await create();
  await screen.findByText("此名称已被占用，请更换名称。");
  const field = screen.getByLabelText("中转名称");
  expect(field.closest("details")?.open).toBe(true);
  expect(
    calls.filter((call) => call.command === "create_cloudflare_network"),
  ).toHaveLength(1);
});

test("cleanup failure preserves the created network and reports a separate cleanup outcome", async () => {
  let current!: Status;
  const { status } = await fixture(
    {
      cloudflare_accounts: () => [account],
      create_cloudflare_network: async (args) => {
        current.local.joined = true;
        current.network = {
          network_id: "created-network",
          manager_id: "self",
          manager_name: "mac1",
          is_manager: true,
          relay_addresses: [relay.link],
        };
        await deploymentEvent(args, "deployed", "completed", {
          deployment: relay,
        });
        throw {
          code: "NETWORK_CREATED_CLEANUP_FAILED",
          message: "temporary authorization cleanup unconfirmed",
        };
      },
    },
    false,
  );
  current = status;
  fireEvent.click(screen.getByRole("button", { name: "新建网络" }));
  await create();
  await screen.findByText("网络已创建，授权清理未完成。");
  await screen.findByRole("button", { name: "邀请设备" });
});
