import { load, nameArgument, save, wrangler } from "./config";
const state = await load(nameArgument(), false);
if (state.account && process.env.CLOUDFLARE_ACCOUNT_ID !== state.account) throw new Error("Use the same CLOUDFLARE_ACCOUNT_ID as the saved deployment");
// Only remove a deployment recorded by this project's deployment command.
await wrangler(["deploy", "--config", "wrangler.cleanup.jsonc", "--name", state.name]);
await wrangler(["delete", state.name]);
state.removed = true;
await save(state);
console.log(`Removed ${state.name} and its Durable Object namespace.`);
