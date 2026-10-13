import { expect, test } from "bun:test";
import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { fixture, selectOption } from "./fixtures";
import type { Status } from "../src/api";

const account = { id: "a".repeat(32), name: "个人账户" };
const relay = {
  provider: "cloudflare",
  name: "xrun-relay",
  account_id: account.id,
  link: "https://xrun-relay.example.workers.dev",
};
const create = async () => {
  await act(async () =>
    fireEvent.submit(document.getElementById("cloudflare-create-form")!),
  );
};

test("Cloudflare creates a relay and manager with one account without exposing its private link", async () => {
  const { calls } = await fixture(
    { cloudflare_accounts: () => [account], deploy_cloudflare: () => relay },
    false,
  );
  fireEvent.click(screen.getByRole("button", { name: "新建网络" }));
  fireEvent.change(screen.getByLabelText("本机名称"), {
    target: { value: "mac-pro" },
  });
  await create();
  await screen.findByRole("button", { name: "邀请设备" });
  expect(
    calls.find((call) => call.command === "deploy_cloudflare")?.args,
  ).toEqual({ name: "xrun-relay", account: account.id, region: "auto" });
  expect(calls.find((call) => call.command === "connect")?.args).toEqual({
    link: relay.link,
    name: "mac-pro",
    allowManager: false,
  });
  expect(document.body.textContent).not.toContain("secret-deployment-route");
});

test("multiple Cloudflare accounts require an explicit choice before deployment", async () => {
  const second = { id: "b".repeat(32), name: "团队账户" };
  const { calls } = await fixture(
    {
      cloudflare_accounts: () => [account, second],
      deploy_cloudflare: () => relay,
    },
    false,
  );
  fireEvent.click(screen.getByRole("button", { name: "新建网络" }));
  await create();
  const select = await screen.findByLabelText("Cloudflare 账户");
  expect(calls.some((call) => call.command === "deploy_cloudflare")).toBe(
    false,
  );
  await selectOption(select, second.id);
  await create();
  expect(
    calls.find((call) => call.command === "deploy_cloudflare")?.args.account,
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
      deploy_cloudflare: () => relay,
      connect: () => {
        if (++attempts === 1) throw "CONNECT_TIMEOUT: unavailable";
        current.local.joined = true;
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
    calls.filter((call) => call.command === "deploy_cloudflare"),
  ).toHaveLength(1);
  expect(calls.filter((call) => call.command === "connect")).toHaveLength(2);
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
        call.command === "deploy_cloudflare" || call.command === "connect",
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
  expect(calls.some((call) => call.command === "deploy_cloudflare")).toBe(
    false,
  );
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
  ).toEqual({ reauthorize: true, remember: true });
  fireEvent.click(screen.getByRole("button", { name: "退出授权" }));
  await screen.findByText("授权未能撤销，请重试。");
  expect(option.checked).toBe(true);
  expect(remembered).toBe(true);
  failLogout = false;
  fireEvent.click(screen.getByRole("button", { name: "退出授权" }));
  await screen.findByText("授权已撤销");
  expect(remembered).toBe(false);
});

test("Cloudflare deployment passes an explicitly selected region to the native deployer", async () => {
  const { calls } = await fixture(
    { cloudflare_accounts: () => [account], deploy_cloudflare: () => relay },
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
    calls.find((call) => call.command === "deploy_cloudflare")?.args.region,
  ).toBe("apac-ne");
});
