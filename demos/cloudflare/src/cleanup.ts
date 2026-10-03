export default {
  fetch(): Response {
    return new Response("Cloudflare transport demo has been removed", { status: 410 });
  },
} satisfies ExportedHandler;
