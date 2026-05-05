<script lang="ts">
  // Legacy /compose deep links now redirect into the chat shell so
  // bookmarks and external `?to=<username>` URLs keep working.
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";

  onMount(() => {
    const to = page.url.searchParams.get("to") ?? "";
    const target = to
      ? `/?contact=${encodeURIComponent(to.trim().toLowerCase())}`
      : "/";
    void goto(target, { replaceState: true });
  });
</script>

<p class="muted">Redirecting…</p>

<style>
  p {
    padding: 1.5rem;
  }
</style>
