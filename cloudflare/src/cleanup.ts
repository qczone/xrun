export default {
  fetch(): Response {
    return new Response("Cloudflare relay has been removed", { status: 410 });
  },
} satisfies ExportedHandler;
