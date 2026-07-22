import {
  Decoration,
  type DecorationSet,
  EditorView,
  ViewPlugin,
  type ViewUpdate,
  WidgetType,
} from '@codemirror/view'
import { RangeSetBuilder } from '@codemirror/state'
import { imageUrlsOnLine } from '../../images'

/**
 * Inline image previews: every line that references an image (markdown
 * `![](...)` or a raw `<img src>`) gets a thumbnail strip rendered directly
 * below it, so posts read as posts instead of URL soup. Remote and data
 * URLs only; the text stays fully editable above the preview.
 */

class ImageStripWidget extends WidgetType {
  constructor(private readonly urls: string[]) {
    super()
  }

  override eq(other: ImageStripWidget): boolean {
    return this.urls.join('\n') === other.urls.join('\n')
  }

  toDOM(): HTMLElement {
    const strip = document.createElement('div')
    strip.className = 'cm-image-preview-strip'
    for (const url of this.urls) {
      const img = document.createElement('img')
      img.src = url
      img.loading = 'lazy'
      img.decoding = 'async'
      img.draggable = false
      img.addEventListener('error', () => {
        img.classList.add('cm-image-preview-broken')
        img.alt = 'image failed to load'
      })
      strip.appendChild(img)
    }
    return strip
  }

  override ignoreEvent(): boolean {
    return true
  }
}

function buildDecorations(view: EditorView): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>()
  for (const { from, to } of view.visibleRanges) {
    let pos = from
    while (pos <= to) {
      const line = view.state.doc.lineAt(pos)
      const urls = imageUrlsOnLine(line.text)
      if (urls.length > 0) {
        builder.add(
          line.to,
          line.to,
          Decoration.widget({
            widget: new ImageStripWidget(urls),
            block: true,
            side: 1,
          })
        )
      }
      pos = line.to + 1
    }
  }
  return builder.finish()
}

const imagePreviewPlugin = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet

    constructor(view: EditorView) {
      this.decorations = buildDecorations(view)
    }

    update(update: ViewUpdate) {
      if (update.docChanged || update.viewportChanged) {
        this.decorations = buildDecorations(update.view)
      }
    }
  },
  { decorations: plugin => plugin.decorations }
)

const imagePreviewTheme = EditorView.baseTheme({
  '.cm-image-preview-strip': {
    display: 'flex',
    flexWrap: 'wrap',
    gap: '8px',
    padding: '6px 0 10px',
  },
  '.cm-image-preview-strip img': {
    maxHeight: '150px',
    maxWidth: '46%',
    borderRadius: '6px',
    objectFit: 'contain',
    backgroundColor: 'rgba(127, 127, 127, 0.08)',
  },
  '.cm-image-preview-strip img.cm-image-preview-broken': {
    minWidth: '120px',
    minHeight: '40px',
  },
})

export function imagePreview() {
  return [imagePreviewPlugin, imagePreviewTheme]
}
