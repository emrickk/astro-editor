import {
  Decoration,
  type DecorationSet,
  EditorView,
  WidgetType,
} from '@codemirror/view'
import {
  type EditorState,
  RangeSetBuilder,
  StateField,
} from '@codemirror/state'
import { imageUrlsOnLine } from '../../images'

/**
 * Inline image previews: every line that references an image (markdown
 * `![](...)` or a raw `<img src>`) gets a thumbnail strip rendered directly
 * below it, so posts read as posts instead of URL soup. Remote and data
 * URLs only; the text stays fully editable above the preview.
 *
 * Implemented as a StateField because block decorations affect vertical
 * layout and may not be provided from a view plugin (doing so makes the
 * editor loop on layout and freeze).
 */

class ImageStripWidget extends WidgetType {
  constructor(private readonly urls: string[]) {
    super()
  }

  override eq(other: ImageStripWidget): boolean {
    return this.urls.join('\n') === other.urls.join('\n')
  }

  override get estimatedHeight(): number {
    return 166
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

function computeDecorations(state: EditorState): DecorationSet {
  const builder = new RangeSetBuilder<Decoration>()
  for (let lineNumber = 1; lineNumber <= state.doc.lines; lineNumber++) {
    const line = state.doc.line(lineNumber)
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
  }
  return builder.finish()
}

const imagePreviewField = StateField.define<DecorationSet>({
  create: computeDecorations,
  update(decorations, transaction) {
    if (!transaction.docChanged) return decorations
    return computeDecorations(transaction.state)
  },
  provide: field => EditorView.decorations.from(field),
})

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
  return [imagePreviewField, imagePreviewTheme]
}
