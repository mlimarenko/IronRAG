<script setup lang="ts">
import { computed } from 'vue'

const props = defineProps<{
  content: string
}>()

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
}

function renderMarkdown(value: string): string {
  let html = escapeHtml(value)

  html = html.replace(/```(\w*)\n([\s\S]*?)```/g, (_match, _lang, code) =>
    `<pre><code>${code.replace(/\n$/, '')}</code></pre>`,
  )
  html = html.replace(/`([^`\n]+)`/g, '<code>$1</code>')
  html = html.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
  html = html.replace(/(?<!\*)\*(?!\*)(.+?)(?<!\*)\*(?!\*)/g, '<em>$1</em>')
  html = html.replace(
    /\[([^\]]+)\]\(([^)]+)\)/g,
    '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>',
  )

  html = html.replace(/((?:^|\n)- .+(?:\n- .+)*)/g, (block) => {
    const items = block
      .split('\n')
      .filter((line) => line.startsWith('- '))
      .map((line) => `<li>${line.slice(2)}</li>`)
      .join('')
    return `<ul>${items}</ul>`
  })

  html = html
    .split(/(<pre[\s\S]*?<\/pre>)/g)
    .map((segment, index) => (index % 2 === 0 ? segment.replace(/\n/g, '<br/>') : segment))
    .join('')

  return html
}

const renderedHtml = computed(() => renderMarkdown(props.content))
</script>

<template>
  <div class="rr-assistant-markdown" v-html="renderedHtml" />
</template>
