<script lang="ts">
  // Legacy /compose deep links now redirect into the chat shell so
  // bookmarks and external `?to=<username>` URLs keep working.
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";

  onMount(() => {
    const to = page.url.searchParams.get("to") ?? "";
    const replyTo = page.url.searchParams.get("reply_to") ?? "";
    const params = new URLSearchParams();
    if (to) params.set("contact", to.trim().toLowerCase());
    if (replyTo) params.set("reply_to", replyTo);
    const query = params.toString();
    void goto(query ? `/?${query}` : "/", { replaceState: true });
  });
</script>

<p class="muted">Redirecting…</p>

<style>
  p {
    padding: 1.5rem;
  }
</style>
