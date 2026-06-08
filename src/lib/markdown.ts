/**
 * 极简 markdown → HTML（块 ① 渲染辅助）。
 *
 * 自己写、不引库（环境 npm 依赖树坏着，装不上 marked 之类）。覆盖 agent 输出里
 * 常见的：代码围栏 ```、行内 `code`、**粗体**、*斜体*、# 标题、- / 1. 列表、
 * [链接](url)、段落与换行。先转义 HTML 再套这套受控标签 —— LLM 内容无法注入。
 *
 * 用法：`{@html md(text)}`。生成的标签带 `md-` 前缀类，样式在使用方用 :global 写。
 */

function escapeHtml(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
}

// 行内元素（已对整段转义过 HTML，这里只做 markdown 标记）。
function inline(s: string): string {
  return s
    .replace(/`([^`]+)`/g, (_m, c) => `<code class="md-code">${c}</code>`)
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    .replace(/(^|[^*])\*([^*\s][^*]*)\*/g, '$1<em>$2</em>')
    .replace(
      /\[([^\]]+)\]\(([^)\s]+)\)/g,
      '<a class="md-a" href="$2" target="_blank" rel="noopener noreferrer">$1</a>'
    );
}

const BLOCK_START = /^(```|#{1,4}\s|\s*[-*]\s|\s*\d+\.\s)/;

export function md(src: string): string {
  if (!src) return '';
  const lines = escapeHtml(src).split('\n');
  let html = '';
  let i = 0;
  let inUl = false;
  let inOl = false;
  const closeLists = () => {
    if (inUl) {
      html += '</ul>';
      inUl = false;
    }
    if (inOl) {
      html += '</ol>';
      inOl = false;
    }
  };

  while (i < lines.length) {
    const line = lines[i];

    // 代码围栏 ```lang
    if (/^```/.test(line)) {
      closeLists();
      i++;
      let code = '';
      while (i < lines.length && !/^```\s*$/.test(lines[i])) {
        code += lines[i] + '\n';
        i++;
      }
      i++; // 跳过收尾 ```
      html += `<pre class="md-pre"><code>${code.replace(/\n$/, '')}</code></pre>`;
      continue;
    }

    // 标题 # … ####
    const h = line.match(/^(#{1,4})\s+(.*)$/);
    if (h) {
      closeLists();
      const lvl = h[1].length;
      html += `<div class="md-h md-h${lvl}">${inline(h[2])}</div>`;
      i++;
      continue;
    }

    // 无序列表 - / *
    const ul = line.match(/^\s*[-*]\s+(.*)$/);
    if (ul) {
      if (inOl) {
        html += '</ol>';
        inOl = false;
      }
      if (!inUl) {
        html += '<ul class="md-ul">';
        inUl = true;
      }
      html += `<li>${inline(ul[1])}</li>`;
      i++;
      continue;
    }

    // 有序列表 1.
    const ol = line.match(/^\s*\d+\.\s+(.*)$/);
    if (ol) {
      if (inUl) {
        html += '</ul>';
        inUl = false;
      }
      if (!inOl) {
        html += '<ol class="md-ol">';
        inOl = true;
      }
      html += `<li>${inline(ol[1])}</li>`;
      i++;
      continue;
    }

    // 空行
    if (line.trim() === '') {
      closeLists();
      i++;
      continue;
    }

    // 段落：连续的非块级行用 <br> 连接
    closeLists();
    let para = inline(line);
    i++;
    while (i < lines.length && lines[i].trim() !== '' && !BLOCK_START.test(lines[i])) {
      para += '<br>' + inline(lines[i]);
      i++;
    }
    html += `<p class="md-p">${para}</p>`;
  }

  closeLists();
  return html;
}
