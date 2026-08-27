<template>
  <section
    class="relative flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden rounded-lg border border-border/70 bg-workspace text-foreground"
  >
    <div v-if="$slots.summary" class="flex-none border-b border-border/70">
      <slot name="summary" />
    </div>

    <header
      v-if="$slots.header"
      class="md-workspace-toolbar flex w-full flex-none items-stretch border-b border-border"
    >
      <div class="flex min-w-0 flex-1 items-stretch">
        <slot name="header" />
      </div>
    </header>

    <div v-if="$slots.notice" class="m-0 flex-none bg-muted/35 px-4 py-2 text-content-secondary text-muted-foreground">
      <slot name="notice" />
    </div>

    <div class="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden">
      <slot />
    </div>
  </section>
</template>
