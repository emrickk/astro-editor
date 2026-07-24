import { EditorView } from '@codemirror/view'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { useEditorStore } from '../../../store/editorStore'
import { useProjectStore } from '../../../store/projectStore'
import {
  getActiveProjectOperation,
  resetProjectOperationLeaseForTests,
  tryAcquireProjectOperation,
} from '../../../store/projectOperationLease'
import { processDroppedFiles } from './fileProcessing'
import { handleTauriFileDrop } from './handlers'
import type { FileEntry } from '@/types'
import type { ProcessedFile } from './types'

vi.mock('./fileProcessing', () => ({ processDroppedFiles: vi.fn() }))

const PROJECT_PATH = '/project'
const CURRENT_FILE: FileEntry = {
  id: 'posts/current',
  path: '/project/src/content/posts/current.md',
  name: 'current',
  extension: 'md',
  collection: 'posts',
  last_modified: null,
  frontmatter: null,
}

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason?: unknown) => void
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise
    reject = rejectPromise
  })
  return { promise, resolve, reject }
}

function createEditorView() {
  const container = document.createElement('div')
  container.setAttribute('data-editor-container', '')
  vi.spyOn(container, 'getBoundingClientRect').mockReturnValue({
    x: 0,
    y: 0,
    top: 0,
    left: 0,
    right: 100,
    bottom: 100,
    width: 100,
    height: 100,
    toJSON: () => ({}),
  })
  const editor = document.createElement('div')
  container.appendChild(editor)

  const dispatch = vi.fn()
  const view = {
    dom: editor,
    state: { selection: { main: { from: 4 } } },
    dispatch,
  } as unknown as EditorView
  return { view, dispatch }
}

const payload = {
  paths: ['/tmp/photo.png'],
  position: { x: 10, y: 10 },
}

describe('handleTauriFileDrop operation safety', () => {
  beforeEach(() => {
    vi.mocked(processDroppedFiles).mockReset()
    resetProjectOperationLeaseForTests()
    useProjectStore.setState({
      projectPath: PROJECT_PATH,
      isOperationLocked: false,
    })
    useEditorStore.setState({
      currentFile: CURRENT_FILE,
      isOperationLocked: false,
    })
  })

  afterEach(() => {
    resetProjectOperationLeaseForTests()
  })

  it('claims the workflow lease before awaiting image processing', async () => {
    const processing = deferred<ProcessedFile[]>()
    vi.mocked(processDroppedFiles).mockReturnValue(processing.promise)
    const { view, dispatch } = createEditorView()

    const pendingDrop = handleTauriFileDrop(payload, view)

    expect(getActiveProjectOperation()).toEqual({
      kind: 'image',
      projectPath: PROJECT_PATH,
    })
    expect(tryAcquireProjectOperation('pull', PROJECT_PATH)).toBeNull()

    processing.resolve([
      {
        originalPath: '/tmp/photo.png',
        filename: 'photo.png',
        isImage: true,
        markdownText: '![](https://cdn.example.com/photo.webp)',
      },
    ])

    await expect(pendingDrop).resolves.toEqual({
      success: true,
      insertText: '![](https://cdn.example.com/photo.webp)',
    })
    expect(dispatch).toHaveBeenCalledWith({
      changes: {
        from: 4,
        insert: '![](https://cdn.example.com/photo.webp)',
      },
      selection: { anchor: 43 },
    })
    expect(getActiveProjectOperation()).toBeNull()
  })

  it('does not insert a late result into a different open file', async () => {
    const processing = deferred<ProcessedFile[]>()
    vi.mocked(processDroppedFiles).mockReturnValue(processing.promise)
    const { view, dispatch } = createEditorView()
    const pendingDrop = handleTauriFileDrop(payload, view)

    useEditorStore.setState({
      currentFile: { ...CURRENT_FILE, id: 'posts/other', name: 'other' },
    })
    processing.resolve([
      {
        originalPath: '/tmp/photo.png',
        filename: 'photo.png',
        isImage: true,
        markdownText: '![](https://cdn.example.com/photo.webp)',
      },
    ])

    const result = await pendingDrop
    expect(result.success).toBe(false)
    expect(result.error).toMatch(/open file changed/)
    expect(dispatch).not.toHaveBeenCalled()
    expect(getActiveProjectOperation()).toBeNull()
  })

  it('refuses a drop while another project operation owns the lease', async () => {
    const pullLease = tryAcquireProjectOperation('pull', PROJECT_PATH)
    expect(pullLease).not.toBeNull()
    const { view, dispatch } = createEditorView()

    const result = await handleTauriFileDrop(payload, view)

    expect(result).toEqual({
      success: false,
      insertText: '',
      error: 'Finish the current project operation before dropping files',
    })
    expect(processDroppedFiles).not.toHaveBeenCalled()
    expect(dispatch).not.toHaveBeenCalled()
    pullLease?.release()
  })
})
