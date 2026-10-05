import { wrangler } from "./config";
await wrangler(["deploy", "--dry-run", "--outdir", ".wrangler/build"]);
